//! The device's side of one wake: hardware set-up, then the wake logic shared
//! with the PC emulator (`frogterm_core::app`), then deep sleep. This file
//! implements `Device` for the reTerminal: panel, buzzer, NVS, flash, Wi-Fi.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use frogterm_core::album::{self, Photo};
use frogterm_core::app::{self, Device, Led, Online, Readings, State};
use frogterm_core::beep::Tone;
use frogterm_core::calendar::{self, DayEvent, IcsScanner};
use frogterm_core::framebuf::{FrameBuffer, Ink};
use frogterm_core::info::Info;
use frogterm_core::photo_list::{after_refresh, PhotoList};
use frogterm_core::weather::{self, Forecast};
use frogterm_core::{beep, jpeg, schedule, screen, stock, tz};

use crate::board::Board;
use crate::epd::Epd;
use crate::net::{self, HttpStatus, Net};
use crate::rtc::Rtc;
use crate::settings::{host, settings, Settings};
use crate::sleep::{button_held, deep_sleep, wake_cause};
use crate::store::Store;

/// Whole update must finish within this, or the device gives up and sleeps
/// (300 s for the update, plus a button press waiting out the panel's rest).
/// Sleep after a crash restart before trying again.
const CRASH_PAUSE_SECS: u64 = 30 * 60;
const WATCHDOG: Duration = Duration::from_secs(300 + schedule::PANEL_REST_SECS as u64);
const MAX_PAGES: usize = 100;
const ATTEMPTS: usize = 3;
/// Stack of the download threads (TLS handshake needs ~10 KB).
const BG_STACK: usize = 24 * 1024;

/// `app::State` and the last access point, kept in NVS between wakes.
struct Nvs {
    nvs: EspNvs<NvsDefault>,
    /// Last access point joined (BSSID, channel).
    last_ap: Option<([u8; 6], u8)>,
}

impl Nvs {
    fn open(part: EspDefaultNvsPartition) -> Result<(Nvs, State)> {
        let nvs = EspNvs::new(part, "frogterm", true)?;
        let mut ap = [0u8; 7];
        let last_ap = match nvs.get_blob("last_ap", &mut ap)? {
            Some(b) if b.len() == 7 => Some(([b[0], b[1], b[2], b[3], b[4], b[5]], b[6])),
            _ => None,
        };
        // older firmware kept a counter: photos 0..stock_next were shown
        let seen = match nvs.get_u32("seen")? {
            Some(s) => s,
            None => stock::from_counter(nvs.get_u8("stock_next")?.unwrap_or(0)),
        };
        let state = State {
            target: nvs.get_i64("target")?.unwrap_or(0),
            attempt: nvs.get_u8("attempt")?.unwrap_or(0),
            stock_count: nvs.get_u8("stock_n")?.unwrap_or(0),
            seen,
            last_refresh: nvs.get_i64("refreshed")?,
            current: nvs.get_u8("current")?.unwrap_or(0),
            sync_failed: nvs.get_u8("sync_failed")? == Some(1),
        };
        Ok((Nvs { nvs, last_ap }, state))
    }

    fn save(&mut self, s: &State) {
        let nvs = &mut self.nvs;
        let _ = nvs.set_i64("target", s.target);
        let _ = nvs.set_u8("attempt", s.attempt);
        let _ = nvs.set_u8("stock_n", s.stock_count);
        let _ = nvs.set_u32("seen", s.seen);
        let _ = nvs.set_u8("current", s.current);
        let _ = nvs.set_u8("sync_failed", u8::from(s.sync_failed));
        if let Some(t) = s.last_refresh {
            let _ = nvs.set_i64("refreshed", t);
        }
        if let Some((bssid, channel)) = self.last_ap {
            let mut b = [0u8; 7];
            b[..6].copy_from_slice(&bssid);
            b[6] = channel;
            let _ = nvs.set_blob("last_ap", &b);
        }
    }
}

/// The reTerminal as `app::Device`.
struct Reterminal<'d> {
    cfg: &'d Settings,
    board: Board<'d>,
    epd: Epd<'d>,
    rtc: Rtc<'d>,
    rtc_now: Option<i64>,
    nvs: Nvs,
    /// Taken by the (single) Wi-Fi session of a wake.
    modem: Option<Modem<'static>>,
    sysloop: EspSystemEventLoop,
    nvs_part: EspDefaultNvsPartition,
}

impl Device for Reterminal<'_> {
    fn beep(&mut self, tones: &[Tone]) {
        self.board.beep(tones);
    }

    fn led(&mut self, led: Led) {
        crate::board::set_led(led);
    }

    fn clock(&self) -> Option<i64> {
        unix_now()
    }

    fn show(&mut self, fb: FrameBuffer) -> Result<()> {
        self.epd.show(&fb)
    }

    fn load_screen(&mut self, slot: u8) -> Option<FrameBuffer> {
        Store.load_screen(slot as usize)
    }

    fn save_screen(&mut self, slot: u8, fb: &FrameBuffer) -> Result<()> {
        Store.save_screen(slot as usize, fb)
    }

    /// Weather + events of the last fetch. NVS spreads its writes over its
    /// pages (wear levelling), so the ~24 writes a day don't wear the flash.
    fn load_info(&mut self) -> Option<Info> {
        let mut buf = vec![0u8; 8 * 1024];
        let bytes = self.nvs.nvs.get_blob("info", &mut buf).ok()??;
        Info::from_text(&String::from_utf8_lossy(bytes))
    }

    fn save_info(&mut self, info: &Info) {
        if let Err(e) = self.nvs.nvs.set_blob("info", info.to_text().as_bytes()) {
            log::warn!("could not store weather/calendar: {e}");
        }
    }

    fn save_state(&mut self, state: &State) {
        self.nvs.save(state);
    }

    fn pause(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn go_online(&mut self, now: Option<i64>, batch: u8) -> Result<Online> {
        let tick = |step: &str| log::info!("progress: {step}");
        log_heap("before Wi-Fi");
        // read the stored photo list from flash while Wi-Fi joins
        let preload = if batch > 0 { Some(crate::spawn_with_internal_stack("preload", 8 * 1024, || Store.load_list())?) } else { None };
        let modem = self.modem.take().ok_or_else(|| anyhow!("Wi-Fi was already used in this wake"))?;
        let cfg = self.cfg;
        let mut net = Net::connect(modem, self.sysloop.clone(), self.nvs_part.clone(), cfg.wifi_ssid, cfg.wifi_password, self.nvs.last_ap)?;
        tick("Wi-Fi up");
        if net.joined.is_some() {
            self.nvs.last_ap = net.joined;
        }
        log_heap("Wi-Fi up");
        let stored_list = preload.and_then(|h| h.join().ok()).flatten();
        // the RTC keeps good time; wait for NTP only if it lost power
        let now = now.or_else(|| net.synced_time(Duration::from_secs(15))).ok_or_else(|| anyhow!("time unknown"))?;
        let fetched = online(cfg, &mut net, now, batch, stored_list)?;
        // every Wi-Fi session corrects the RTC from NTP (usually already
        // done during the downloads, so this rarely waits)
        match net.synced_time(Duration::from_secs(10)) {
            Some(t) => match self.rtc.write(t) {
                Ok(()) => log::info!("RTC set from NTP (was {:+} s off)", self.rtc_now.map_or(0, |r| r - t)),
                Err(e) => log::warn!("RTC write failed: {e:#}"),
            },
            None => log::warn!("NTP did not answer within 10 s, RTC kept as is"),
        }
        net.stop();
        tick("downloads done");
        Ok(fetched)
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
    // a crash restarts the chip at once; without this pause a crash that
    // repeats would re-download and re-store a photo batch every minute
    // (flash wear, battery). Buttons still wake it.
    if let Some(why) = crate::sleep::crash_reset() {
        log::error!("restarted after a crash ({why}), pausing {} min", CRASH_PAUSE_SECS / 60);
        crate::board::set_led(Led::Off);
        deep_sleep(CRASH_PAUSE_SECS, schedule::arm_button(button_held()));
    }
    let p = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs_part = EspDefaultNvsPartition::take()?;
    let cfg = settings();
    if let Some(zone) = tz::Zone::parse(cfg.timezone) {
        tz::set_zone(zone);
    }
    let (nvs, mut state) = Nvs::open(nvs_part.clone())?;
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

    // woke early: straight back to sleep, before powering anything up
    // (and the LED: off once the panel's rest after the last refresh is over)
    if let Some(secs) = app::sleep_early(wake, rtc_now, &state) {
        log::info!("woke {secs} s early");
        let (led, secs) = app::led_while_asleep(rtc_now, &state, secs);
        crate::board::set_led(led);
        deep_sleep(secs, schedule::arm_button(button_held()));
    }

    let mut board = Board::new(p.pins.gpio21, p.pins.gpio16, p.pins.gpio14)?;
    crate::sleep::spawn_press_listener();
    // watchdog: never stay awake (battery) if anything hangs
    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        log::error!("update took longer than {} s, giving up", WATCHDOG.as_secs());
        deep_sleep(30 * 60, schedule::arm_button(button_held()));
    });
    let battery_mv = board.battery_mv();

    let mut epd = Epd::new(p.spi2, p.pins.gpio7, p.pins.gpio9, p.pins.gpio10, p.pins.gpio11, p.pins.gpio12, p.pins.gpio13)?;
    if cfg!(feature = "bars") {
        epd.show(&FrameBuffer::colour_bars())?;
        board.beep(beep::SUCCESS);
        deep_sleep(24 * 3600, schedule::arm_button(button_held()));
    }

    let rules = app::Config {
        update_times: &cfg.update_times,
        refresh_minutes: cfg.refresh_minutes,
        photo_batch: cfg.photo_batch,
        opacity: cfg.opacity,
        place: cfg.place,
        panel_rest: true,
    };
    let mut dev = Reterminal { cfg: &cfg, board, epd, rtc, rtc_now, nvs, modem: Some(p.modem), sysloop, nvs_part };
    let readings = Readings { now: rtc_now, battery_mv, room_c: room.map(|r| r.0) };
    let secs = app::run(&mut dev, &mut state, &rules, wake, readings);
    // buttons stay active: a press wakes the device
    deep_sleep(secs, schedule::arm_button(button_held()))
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

/// Online part: fresh weather + calendars, and `batch` new ready-to-show
/// photo screens (0 = none).
fn online(cfg: &Settings, net: &mut Net, now: i64, batch: u8, stored_list: Option<PhotoList>) -> Result<Online> {
    let (today, _) = tz::to_local(now);
    let mut store = Store;
    // weather and calendars download in the background while we do the photos
    let (forecast, events, batch) = std::thread::scope(|s| -> Result<_> {
        let spawn = |name: &str| std::thread::Builder::new().name(name.into()).stack_size(BG_STACK);
        let weather = spawn("weather").spawn_scoped(s, || fetch_weather(cfg))?;
        let calendars = spawn("calendars").spawn_scoped(s, || fetch_calendars(cfg, today))?;
        let screens = if batch > 0 {
            let t = std::time::Instant::now();
            let mut list = photo_list(cfg, net, &mut store, now, stored_list)?;
            let screens: Vec<FrameBuffer> = (0..batch)
                .filter_map(|_| random_screen(&mut list, &mut store, cfg.opacity, now).map_err(|e| log::warn!("{e:#}")).ok())
                .collect();
            log::info!("downloaded {} photo screens in {:.1} s", screens.len(), t.elapsed().as_secs_f32());
            screens
        } else {
            Vec::new()
        };
        let forecast = weather.join().map_err(|_| anyhow!("weather thread panicked"))?;
        let forecast = forecast.map_err(|e| log::warn!("weather: {e:#}")).ok();
        let events = calendars.join().map_err(|_| anyhow!("calendar thread panicked"))?;
        Ok((forecast, events, screens))
    })?;
    let (events, calendars_failed) = events;
    // all calendars failing shows as `events: None`; some of them only here
    let missing = if calendars_failed > 0 && events.is_some() { vec!["a calendar"] } else { vec![] };
    Ok(Online { now, forecast, events, batch, missing })
}

/// Cached list from flash, re-read from Google when missing or a week old.
fn photo_list(cfg: &Settings, net: &mut Net, store: &mut Store, now: i64, old: Option<PhotoList>) -> Result<PhotoList> {
    if let Some(l) = old.as_ref().filter(|l| l.is_fresh_for(cfg.album_url, now as u64)) {
        log::info!("photo list: {} photos, {:.1} days old", l.photos.len(), l.age_days(now as u64));
        return old.ok_or_else(|| anyhow!("no photo list"));
    }
    let (fetched, complete) = read_album(net, cfg.album_url);
    log::info!("album read: {} photos, complete: {complete}", fetched.len());
    // a failed read keeps the old list as it is in flash: nothing to save
    let keeps_old = !(complete && !fetched.is_empty()) && old.is_some();
    let list = after_refresh(old, fetched, complete, cfg.album_url, now as u64).ok_or_else(|| anyhow!("no photo list"))?;
    if !keeps_old {
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
            if seen.insert(album::url_key(&p.url)) {
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
    // std seeds RandomState from the hardware RNG (esp_fill_random)
    let entropy = std::hash::BuildHasher::hash_one(&std::collections::hash_map::RandomState::new(), now);
    let mut seed = (now as u64) ^ entropy | 1;
    let mut rand = move |n: usize| {
        // xorshift
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
                    let (pw, ph, taken) = (photo.width, photo.height, photo.taken);
                    std::thread::Builder::new().name("photo".into()).stack_size(BG_STACK).spawn_scoped(sc, move || {
                        let bytes = net::get_bytes(&url).with_context(|| format!("photo #{idx}"))?;
                        log::info!("photo #{idx} ({pw}x{ph}, taken {taken:?}) as {w}x{h}, {} KB", bytes.len() / 1024);
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
        let filled = slots
            .iter()
            .zip(decoded)
            .try_for_each(|(s, rgb)| app::place_photo(&mut fb, &s.area, rgb?, opacity, list.photos[s.photo].taken));
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

/// All of today's and tomorrow's events of all calendars (`None` if none could
/// be read; none configured is an empty day) and how many calendars failed.
fn fetch_calendars(cfg: &Settings, today: frogterm_core::date::Date) -> (Option<calendar::TwoDays>, usize) {
    if cfg.calendars.is_empty() {
        return (Some((Vec::new(), Vec::new())), 0);
    }
    let t = std::time::Instant::now();
    // every calendar in its own thread
    let tomorrow = frogterm_core::date::Date::from_days(today.days_since_epoch() + 1);
    let results: Vec<Option<(Vec<DayEvent>, Vec<DayEvent>)>> = std::thread::scope(|s| {
        let handles: Vec<_> = cfg
            .calendars
            .iter()
            .map(|url| {
                std::thread::Builder::new().name("calendar".into()).stack_size(BG_STACK).spawn_scoped(s, move || {
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
    let failed = cfg.calendars.len() - ok;
    if ok == 0 {
        return (None, failed);
    }
    let (mut all_today, mut all_tomorrow) = (Vec::new(), Vec::new());
    for (a, b) in results.into_iter().flatten() {
        all_today.extend(a);
        all_tomorrow.extend(b);
    }
    calendar::sort_events(&mut all_today);
    calendar::sort_events(&mut all_tomorrow);
    (Some((all_today, all_tomorrow)), failed)
}
