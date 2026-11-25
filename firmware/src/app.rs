//! One wake of the device (spec 7.1 / 7.2): the same steps as `make emulator`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use frogterm_core::album::{self, Photo};
use frogterm_core::calendar::{self, DayEvent, IcsScanner};
use frogterm_core::dither::{dither_area, veil};
use frogterm_core::framebuf::{FrameBuffer, Ink};
use frogterm_core::layout::{draw_events_box, draw_weather_box, events_rect, weather_rect, Backdrop, EventsInfo};
use frogterm_core::photo_list::{after_refresh, PhotoList};
use frogterm_core::info::Info;
use frogterm_core::schedule::{self, UpdatePlan, WakePlan};
use frogterm_core::weather::{self, Forecast};
use frogterm_core::{battery, beep, jpeg, screen, tz};

use crate::board::Board;
use crate::epd::Epd;
use crate::net::{self, HttpStatus, Net};
use crate::rtc::Rtc;
use crate::settings::{host, settings, Settings};
use crate::sleep::{button_held, deep_sleep, wake_cause, Wake};
use crate::store::Store;

/// Whole update must finish within this, or the device gives up and sleeps.
const WATCHDOG: Duration = Duration::from_secs(300);
const MAX_PAGES: usize = 100;
const ATTEMPTS: usize = 3;
/// Stack of the download threads (TLS handshake needs ~10 KB).
const BG_STACK: usize = 24 * 1024;

/// Persisted between wakes (NVS).
struct State {
    nvs: EspNvs<NvsDefault>,
    /// Unix time of the planned daily update.
    target: i64,
    /// Failed updates in a row.
    attempt: u8,
    /// Last access point joined (BSSID, channel).
    last_ap: Option<([u8; 6], u8)>,
    /// Photo screens stored in flash, and the next one to show.
    stock_count: u8,
    stock_next: u8,
    /// Unix time of the last panel refresh.
    last_refresh: Option<i64>,
    /// A button press was postponed for panel rest (the timer wake carries it out).
    press_postponed: bool,
}

impl State {
    fn load(part: EspDefaultNvsPartition) -> Result<State> {
        let nvs = EspNvs::new(part, "frogterm", true)?;
        let target = nvs.get_i64("target")?.unwrap_or(0);
        let attempt = nvs.get_u8("attempt")?.unwrap_or(0);
        let mut ap = [0u8; 7];
        let last_ap = match nvs.get_blob("last_ap", &mut ap)? {
            Some(b) if b.len() == 7 => Some(([b[0], b[1], b[2], b[3], b[4], b[5]], b[6])),
            _ => None,
        };
        let stock_count = nvs.get_u8("stock_n")?.unwrap_or(0);
        let stock_next = nvs.get_u8("stock_next")?.unwrap_or(0);
        let last_refresh = nvs.get_i64("refreshed")?;
        let press_postponed = nvs.get_u8("pressed")?.unwrap_or(0) == 1;
        Ok(State { nvs, target, attempt, last_ap, stock_count, stock_next, last_refresh, press_postponed })
    }

    /// Weather + events of the last fetch. NVS spreads its writes over its
    /// pages (wear levelling), so the ~24 writes a day don't wear the flash.
    fn load_info(&self) -> Option<Info> {
        let mut buf = vec![0u8; 8 * 1024];
        let bytes = self.nvs.get_blob("info", &mut buf).ok()??;
        Info::from_text(&String::from_utf8_lossy(bytes))
    }

    fn save_info(&mut self, info: &Info) {
        if let Err(e) = self.nvs.set_blob("info", info.to_text().as_bytes()) {
            log::warn!("could not store weather/calendar: {e}");
        }
    }

    fn save(&mut self) {
        let _ = self.nvs.set_i64("target", self.target);
        let _ = self.nvs.set_u8("attempt", self.attempt);
        let _ = self.nvs.set_u8("stock_n", self.stock_count);
        let _ = self.nvs.set_u8("stock_next", self.stock_next);
        let _ = self.nvs.set_u8("pressed", u8::from(self.press_postponed));
        if let Some(t) = self.last_refresh {
            let _ = self.nvs.set_i64("refreshed", t);
        }
        if let Some((bssid, channel)) = self.last_ap {
            let mut b = [0u8; 7];
            b[..6].copy_from_slice(&bssid);
            b[6] = channel;
            let _ = self.nvs.set_blob("last_ap", &b);
        }
    }
}

pub fn run() -> ! {
    match run_inner() {
        Ok(never) => match never {},
        Err(e) => {
            log::error!("fatal: {e:#}");
            deep_sleep(schedule::when_time_unknown(), schedule::arm_button(button_held()))
        }
    }
}

fn run_inner() -> Result<std::convert::Infallible> {
    let p = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs_part = EspDefaultNvsPartition::take()?;
    let cfg = settings();
    let mut state = State::load(nvs_part.clone())?;
    let mut rtc = Rtc::new(p.i2c0, p.pins.gpio19, p.pins.gpio20)?;
    let rtc_now = rtc.read().unwrap_or_else(|e| {
        log::warn!("RTC read failed: {e:#}");
        None
    });
    // measured first, before Wi-Fi and the CPU warm the board up
    let room = rtc.room_climate().unwrap_or_else(|e| {
        log::warn!("room sensor: {e:#}");
        None
    });
    log::info!("room {room:?} (°C, %RH)");
    let wake = wake_cause();
    log::info!("frogterm {} woke by {wake:?}, RTC {rtc_now:?}", env!("CARGO_PKG_VERSION"));

    if wake == Wake::Timer {
        if let WakePlan::Sleep(secs) = schedule::on_timer_wake(rtc_now, state.target) {
            log::info!("woke {secs} s early");
            deep_sleep(secs, schedule::arm_button(button_held()));
        }
    }

    let mut board = Board::new(p.pins.gpio6, p.pins.gpio21, p.pins.gpio16, p.pins.gpio14)?;
    if wake == Wake::Button {
        board.beep(beep::PRESS); // "I saw the press"
    }
    crate::sleep::spawn_press_listener();
    // audible progress, only when a person pressed the button (06:00 stays quiet)
    let by_person = schedule::started_by_person(wake == Wake::Button, wake == Wake::Timer, state.press_postponed);
    let progress = |step: &str| {
        if by_person {
            log::info!("progress: {step}");
            crate::board::play(beep::TICK);
        }
    };
    board.led(true);
    // watchdog: never stay awake (battery) if anything hangs
    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        log::error!("update took longer than {} s, giving up", WATCHDOG.as_secs());
        deep_sleep(30 * 60, schedule::arm_button(button_held()));
    });

    let battery_mv = board.battery_mv();
    log::info!("battery {battery_mv:?} mV = {:?} %", battery_mv.map(battery::percent));
    if battery_mv.is_some_and(battery::too_low_to_update) {
        log::warn!("battery too low for a panel refresh");
        board.beep(beep::ERROR);
        sleep_until_next(&mut state, rtc_now, cfg.update_hour);
    }

    // if this update hangs or crashes, the watchdog's 30-minute sleep must
    // count as a retry, not as an early wake before the daily update
    let provisional = schedule::after_failure(rtc_now.or_else(unix_now), state.attempt.saturating_add(1), cfg.update_hour);
    state.target = provisional.target;
    state.save();

    let mut epd = Epd::new(p.spi2, p.pins.gpio7, p.pins.gpio9, p.pins.gpio10, p.pins.gpio11, p.pins.gpio12, p.pins.gpio13)?;
    if cfg!(feature = "bars") {
        epd.show(&FrameBuffer::colour_bars())?;
        board.beep(beep::SUCCESS);
        deep_sleep(24 * 3600, schedule::arm_button(button_held()));
    }

    // e-paper care: at least 180 s between refreshes; a quick second press
    // is done automatically when the panel may refresh again
    if let Some(now) = rtc_now {
        if let Some(wait) = schedule::panel_rest(now, state.last_refresh) {
            log::info!("panel refreshed {} s ago, updating in {wait} s", schedule::PANEL_REST_SECS - wait as i64);
            state.target = now + wait as i64;
            // remember that a person asked, so the postponed update still ticks
            state.press_postponed |= wake == Wake::Button;
            state.save();
            board.led(false);
            deep_sleep(wait, false);
        }
    }

    state.press_postponed = false; // carried out now
    // what this wake has to fetch: nothing, weather+calendar, or also photos
    let stored_info = state.load_info();
    let next_photo = (state.stock_next < state.stock_count)
        .then(|| Store.load_screen(state.stock_next as usize))
        .flatten();
    let photos_left = if next_photo.is_some() { (state.stock_count - state.stock_next) as usize } else { 0 };
    let plan = match rtc_now {
        Some(now) => schedule::plan_update(
            now,
            stored_info.as_ref().map(|i| (i.fetched_at, i.date)),
            photos_left,
            i64::from(cfg.refresh_minutes) * 60,
        ),
        None if photos_left == 0 => UpdatePlan::RefreshAll,
        None => UpdatePlan::RefreshInfo,
    };
    log::info!("plan {plan:?}: {photos_left} stored photos left");

    let result = (|| -> Result<Option<i64>> {
        let (now, info, batch) = match plan {
            UpdatePlan::Offline => {
                let now = rtc_now.ok_or_else(|| anyhow!("time unknown"))?;
                (now, stored_info.clone().ok_or_else(|| anyhow!("no stored info"))?, None)
            }
            UpdatePlan::RefreshInfo | UpdatePlan::RefreshAll => {
                let all = plan == UpdatePlan::RefreshAll;
                log_heap("before Wi-Fi");
                // read the stored photo list from flash while Wi-Fi joins
                let preload = if all {
                    Some(crate::spawn_with_internal_stack("preload", 8 * 1024, || Store.load_list())?)
                } else {
                    None
                };
                let mut net = Net::connect(p.modem, sysloop, nvs_part, cfg.wifi_ssid, cfg.wifi_password, state.last_ap)?;
                progress("Wi-Fi up");
                if net.joined.is_some() {
                    state.last_ap = net.joined;
                }
                log_heap("Wi-Fi up");
                let stored_list = preload.and_then(|h| h.join().ok()).flatten();
                // the RTC keeps good time; wait for NTP only if it lost power
                let now = rtc_now.or_else(|| net.synced_time(Duration::from_secs(15))).ok_or_else(|| anyhow!("time unknown"))?;
                let (info, batch) = online(&cfg, &mut net, now, all, stored_list, stored_info.as_ref())?;
                match net.synced_time(Duration::ZERO) {
                    Some(t) => {
                        if let Err(e) = rtc.write(t) {
                            log::warn!("RTC write failed: {e:#}");
                        }
                    }
                    None => log::info!("NTP not done yet, RTC kept as is"),
                }
                net.stop();
                progress("downloads done");
                (now, info, batch)
            }
        };

        // flash writes, with Wi-Fi already off
        if plan != UpdatePlan::Offline {
            state.save_info(&info);
        }
        let photo = match batch {
            Some(batch) => {
                let mut saved = 0u8;
                for (i, fb) in batch.iter().enumerate() {
                    match Store.save_screen(i, fb) {
                        Ok(()) => saved += 1,
                        Err(e) => {
                            log::warn!("could not store photo {i}: {e:#}");
                            break;
                        }
                    }
                }
                if saved > 0 {
                    log::info!("stored a new batch of {saved} photos");
                    state.stock_count = saved;
                    state.stock_next = 0;
                } else if state.stock_count > 0 {
                    log::warn!("no new photos, showing the stored ones again");
                    state.stock_next = 0;
                }
                batch.into_iter().next().or_else(|| Store.load_screen(state.stock_next as usize))
            }
            None => next_photo,
        };
        let photo = photo.ok_or_else(|| anyhow!("no photo to show"))?;
        state.stock_next = state.stock_next.saturating_add(1).min(state.stock_count);

        let fb = compose(&cfg, photo, &info, now, battery_mv, room.map(|r| r.0));
        progress("drawing the screen");
        epd.show(&fb)?;
        state.last_refresh = Some(now);
        Ok(Some(now))
    })();

    let now = match result {
        Ok(now) => {
            board.beep(beep::SUCCESS);
            state.attempt = 0;
            now
        }
        Err(e) => {
            log::error!("update failed: {e:#}");
            board.beep(beep::ERROR);
            state.attempt = state.attempt.saturating_add(1);
            let retry = schedule::after_failure(rtc_now.or_else(unix_now), state.attempt, cfg.update_hour);
            if schedule::retry_after_failure(state.attempt).is_none() {
                state.attempt = 0; // back to the daily schedule
            }
            state.target = retry.target;
            state.save();
            board.led(false);
            deep_sleep(retry.secs, schedule::arm_button(button_held()));
        }
    };
    board.led(false);
    sleep_until_next(&mut state, now.or_else(|| unix_now()), cfg.update_hour)
}

/// Free memory, for diagnosing out-of-memory crashes.
fn log_heap(stage: &str) {
    use esp_idf_svc::sys::{heap_caps_get_free_size, heap_caps_get_largest_free_block, MALLOC_CAP_INTERNAL, MALLOC_CAP_SPIRAM};
    unsafe {
        log::info!(
            "heap @{stage}: internal {} KB free (largest {} KB), PSRAM {} KB free",
            heap_caps_get_free_size(MALLOC_CAP_INTERNAL) / 1024,
            heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL) / 1024,
            heap_caps_get_free_size(MALLOC_CAP_SPIRAM) / 1024
        );
    }
}

fn unix_now() -> Option<i64> {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    (t > 1_700_000_000).then_some(t) // the clock was set (after Nov 2023)
}

fn sleep_until_next(state: &mut State, now: Option<i64>, hour: u8) -> ! {
    let Some(now) = now else {
        log::warn!("time unknown, retrying in an hour");
        state.save();
        deep_sleep(schedule::when_time_unknown(), schedule::arm_button(button_held()));
    };
    state.target = schedule::next_update(now, hour);
    state.save();
    let (d, s) = tz::to_local(state.target);
    log::info!("sleeping until {} {:02}:{:02} local", d.label(), s / 3600, s % 3600 / 60);
    deep_sleep((state.target - now).max(1) as u64, schedule::arm_button(button_held()))
}

/// Online part: fresh weather + calendars, and with `all` a new batch of
/// ready-to-show photo screens. Weather/calendar that fail fall back to the
/// stored ones.
fn online(
    cfg: &Settings,
    net: &mut Net,
    now: i64,
    all: bool,
    stored_list: Option<PhotoList>,
    stored_info: Option<&Info>,
) -> Result<(Info, Option<Vec<FrameBuffer>>)> {
    let (today, _) = tz::to_local(now);
    let mut store = Store;
    // weather and calendars download in the background while we do the photos
    let (forecast, events, batch) = std::thread::scope(|s| -> Result<_> {
        let spawn = |name: &str| std::thread::Builder::new().name(name.into()).stack_size(BG_STACK);
        let weather = spawn("weather").spawn_scoped(s, || fetch_weather(cfg))?;
        let calendars = spawn("calendars").spawn_scoped(s, || fetch_calendars(cfg, today))?;
        let batch = if all {
            let t = std::time::Instant::now();
            let mut list = photo_list(cfg, net, &mut store, now, stored_list)?;
            let batch: Vec<FrameBuffer> = (0..cfg.photo_batch)
                .filter_map(|_| random_screen(&mut list, &mut store, cfg.opacity, now).map_err(|e| log::warn!("{e:#}")).ok())
                .collect();
            log::info!("downloaded {} photo screens in {:.1} s", batch.len(), t.elapsed().as_secs_f32());
            Some(batch)
        } else {
            None
        };
        let forecast = weather.join().map_err(|_| anyhow!("weather thread panicked"))?;
        let forecast = forecast.map_err(|e| log::warn!("weather: {e:#}")).ok();
        let events = calendars.join().map_err(|_| anyhow!("calendar thread panicked"))?;
        Ok((forecast, events, batch))
    })?;
    let info = Info {
        fetched_at: now,
        date: today,
        forecast: forecast.or_else(|| stored_info.and_then(|i| i.forecast.clone())),
        events: events.as_ref().map(|e| e.0.clone()).or_else(|| stored_info.filter(|i| i.date == today).and_then(|i| i.events.clone())),
        tomorrow: events.map(|e| e.1).or_else(|| stored_info.filter(|i| i.date == today).and_then(|i| i.tomorrow.clone())),
    };
    Ok((info, batch))
}

/// Draws the events and weather boxes over a stored photo screen.
fn compose(cfg: &Settings, mut fb: FrameBuffer, info: &Info, now: i64, battery_mv: Option<u32>, room_c: Option<f32>) -> FrameBuffer {
    let (today, secs) = tz::to_local(now);
    let now_hm = ((secs / 3600) as u8, (secs % 3600 / 60) as u8);
    let backdrop = if cfg.opacity >= 100 { Backdrop::Solid } else { Backdrop::Clear };
    // sunrise/sunset replace the (static) place name when the weather is known
    let sun = info.forecast.as_ref().and_then(|f| f.sun_on(today)).map(|s| (s.rise, s.set));
    let status = match sun {
        Some(_) => format!("{:02}:{:02}", now_hm.0, now_hm.1),
        None => format!("{} {:02}:{:02}", cfg.place, now_hm.0, now_hm.1),
    };
    let agenda = calendar::agenda(info.events.clone(), info.tomorrow.clone(), now_hm);
    draw_weather_box(&mut fb, info.forecast.as_ref().and_then(|f| f.window(now)), backdrop);
    draw_events_box(
        &mut fb,
        &EventsInfo {
            date: &today.label(),
            status: &status,
            room_c,
            battery_pct: battery_mv.map(battery::percent),
            events: agenda.events.as_deref(),
            tomorrow: agenda.tomorrow,
            sun,
        },
        backdrop,
    );
    fb
}

/// Cached list from flash, re-read from Google when missing or a week old.
fn photo_list(cfg: &Settings, net: &mut Net, store: &mut Store, now: i64, old: Option<PhotoList>) -> Result<PhotoList> {
    if let Some(l) = old.as_ref().filter(|l| l.is_fresh_for(cfg.album_url, now as u64)) {
        log::info!("photo list: {} photos, {:.1} days old", l.photos.len(), l.age_days(now as u64));
        return Ok(l.clone());
    }
    let (fetched, complete) = read_album(net, cfg.album_url);
    log::info!("album read: {} photos, complete: {complete}", fetched.len());
    let list = after_refresh(old.clone(), fetched, complete, cfg.album_url, now as u64)
        .ok_or_else(|| anyhow!("no photo list"))?;
    if old.as_ref() != Some(&list) {
        if let Err(e) = store.save_list(&list) {
            log::warn!("could not save the photo list: {e:#}");
        }
    }
    Ok(list)
}

/// All pages of the album; `complete` is false if a page failed.
fn read_album(net: &mut Net, share_link: &str) -> (Vec<Photo>, bool) {
    let mut photos = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |page: album::AlbumPage| {
        for p in page.photos {
            if seen.insert(p.url.clone()) {
                photos.push(p);
            }
        }
        page.next_token
    };
    let first = (|| -> Result<(album::AlbumPage, String, String)> {
        let mut scanner = album::ShareScanner::new();
        let r = net.get(share_link, &mut |c| {
            scanner.feed(c);
            Ok(())
        })?;
        if r.status != 200 {
            bail!(HttpStatus(r.status));
        }
        let (id, key) = album::ids_from_share_url(&r.final_url)?;
        log_heap("share page downloaded, before JSON parse");
        let page = scanner.finish()?;
        log_heap("share page parsed");
        Ok((page, id, key))
    })();
    let (page, id, key) = match first {
        Ok(x) => x,
        Err(e) => {
            log::warn!("album share page: {e:#}");
            return (photos, false);
        }
    };
    let mut token = add(page);
    for n in 2..=MAX_PAGES {
        let Some(tok) = token else { return (photos, true) };
        let mut body = Vec::new();
        let page = net
            .post_form(album::RPC_URL, &album::rpc_body(&id, &key, &tok), &mut |c| {
                body.extend_from_slice(c);
                Ok(())
            })
            .and_then(|_| album::parse_rpc_response(&String::from_utf8_lossy(&body)));
        match page {
            Ok(p) => token = add(p),
            Err(e) => {
                log::warn!("album page {n}: {e:#}");
                return (photos, false);
            }
        }
    }
    (photos, token.is_none())
}

/// Up to `ATTEMPTS` tries of: random photo (+ fresh tall partner), each slot
/// downloaded already sized, veiled under the boxes and dithered into place.
fn random_screen(list: &mut PhotoList, store: &mut Store, opacity: u8, now: i64) -> Result<FrameBuffer> {
    let mut seed = (now as u64) ^ u64::from(unsafe { esp_idf_svc::sys::esp_random() });
    let mut rand = move |n: usize| {
        // xorshift, seeded from the hardware RNG
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    for _ in 0..ATTEMPTS {
        let first = rand(list.photos.len());
        let slots = screen::plan(&list.photos, first, screen::FRESH, &mut rand);
        // download + decode every slot at the same time
        let decoded: Vec<Result<Vec<u8>>> = std::thread::scope(|sc| {
            let handles: Vec<_> = slots
                .iter()
                .map(|s| {
                    let photo = &list.photos[s.photo];
                    let (w, h, idx) = (s.area.size.width, s.area.size.height, s.photo);
                    let url = photo.sized_url(w, h);
                    let (pw, ph) = (photo.width, photo.height);
                    std::thread::Builder::new().name("photo".into()).stack_size(BG_STACK).spawn_scoped(sc, move || {
                        let bytes = net::get_bytes(&url).with_context(|| format!("photo #{idx}"))?;
                        log::info!("photo #{idx} ({pw}x{ph}) as {w}x{h}, {} KB", bytes.len() / 1024);
                        jpeg::decode_rgb(&bytes, w, h)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.map_err(|e| anyhow!("thread: {e}"))
                        .and_then(|h| h.join().map_err(|_| anyhow!("photo thread panicked")))
                        .and_then(|r| r)
                })
                .collect()
        });
        let mut fb = FrameBuffer::new(Ink::White);
        let filled = slots.iter().zip(decoded).try_for_each(|(s, rgb)| -> Result<()> {
            let mut rgb = rgb?;
            if opacity < 100 {
                for boxed in [events_rect(), weather_rect()] {
                    veil(&mut rgb, &s.area, &boxed, opacity);
                }
            }
            dither_area(&rgb, &s.area, &mut fb)
        });
        match filled {
            Ok(()) => return Ok(fb),
            Err(e) => {
                log::warn!("{e:#}");
                if e.downcast_ref::<HttpStatus>().is_some_and(|s| matches!(s.0, 403 | 404 | 410)) {
                    // cached links expired: re-read the album on the next wake
                    list.fetched_at = 0;
                    let _ = store.save_list(list);
                }
            }
        }
    }
    bail!("{ATTEMPTS} tries in a row failed")
}

fn fetch_weather(cfg: &Settings) -> Result<Forecast> {
    let t = std::time::Instant::now();
    let (lat, lon) = match cfg.coords {
        Some(c) => c,
        None => {
            let body = net::get_bytes(&weather::geocode_url(cfg.city))?;
            let place = weather::parse_geocode(&String::from_utf8_lossy(&body))?;
            (place.latitude, place.longitude)
        }
    };
    let body = net::get_bytes(&weather::forecast_url(lat, lon))?;
    let f = weather::parse_forecast(&String::from_utf8_lossy(&body));
    log::info!("weather in {:.1} s", t.elapsed().as_secs_f32());
    f
}

/// All of today's and tomorrow's events of all calendars; `None` if none could be read.
fn fetch_calendars(cfg: &Settings, today: frogterm_core::date::Date) -> Option<(Vec<DayEvent>, Vec<DayEvent>)> {
    if cfg.calendars.is_empty() {
        return None;
    }
    let t = std::time::Instant::now();
    // every calendar in its own thread
    let tomorrow = frogterm_core::date::Date::from_days(today.days_since_epoch() + 1);
    let results: Vec<Option<(Vec<DayEvent>, Vec<DayEvent>)>> = std::thread::scope(|s| {
        let handles: Vec<_> = cfg
            .calendars
            .iter()
            .map(|url| {
                std::thread::Builder::new()
                    .name("calendar".into())
                    .stack_size(BG_STACK)
                    .spawn_scoped(s, move || {
                        // one download, two days
                        let (mut scan_today, mut scan_tomorrow) = (IcsScanner::new(today), IcsScanner::new(tomorrow));
                        let r = net::get(url, &mut |c| {
                            scan_today.feed(c);
                            scan_tomorrow.feed(c);
                            Ok(())
                        });
                        match r {
                            Ok(r) if r.status == 200 => Some((scan_today.finish(), scan_tomorrow.finish())),
                            Ok(r) => {
                                log::warn!("calendar on {}: HTTP {}", host(url), r.status);
                                None
                            }
                            Err(e) => {
                                log::warn!("calendar on {}: {e:#}", host(url));
                                None
                            }
                        }
                    })
            })
            .collect();
        handles.into_iter().map(|h| h.ok().and_then(|h| h.join().ok()).flatten()).collect()
    });
    let ok = results.iter().filter(|r| r.is_some()).count();
    log::info!("{ok} of {} calendars in {:.1} s", cfg.calendars.len(), t.elapsed().as_secs_f32());
    if ok == 0 {
        return None;
    }
    let (mut all_today, mut all_tomorrow) = (Vec::new(), Vec::new());
    for (a, b) in results.into_iter().flatten() {
        all_today.extend(a);
        all_tomorrow.extend(b);
    }
    calendar::sort_events(&mut all_today);
    calendar::sort_events(&mut all_tomorrow);
    Some((all_today, all_tomorrow))
}
