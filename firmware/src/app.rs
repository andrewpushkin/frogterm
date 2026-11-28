//! The device's side of one wake: hardware set-up, then the wake logic shared
//! with the PC emulator (`frogterm_core::app`), then deep sleep. This file
//! implements `Device` for the reTerminal: panel, buzzer, NVS, flash, Wi-Fi.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use frogterm_core::album::{self, Photo};
use frogterm_core::app::{self, Device, Led, Online, Readings, State, Via, Wake};
use frogterm_core::beep::Tone;
use frogterm_core::buttons::Gesture;
use frogterm_core::calendar::{self, DayEvent, IcsScanner};
use frogterm_core::config::Config;
use frogterm_core::framebuf::{FrameBuffer, Ink};
use frogterm_core::info::Info;
use frogterm_core::photo_list::{after_refresh, PhotoList};
use frogterm_core::weather::{self, Forecast};
use frogterm_core::{beep, jpeg, schedule, screen, stock, tz};

use crate::board::Board;
use crate::epd::Epd;
use crate::net::{self, HttpStatus, Net};
use crate::rtc::Rtc;
use crate::settings::host;
use crate::sleep::{button_held, deep_sleep, wake_cause};
use crate::store::Store;

/// Sleep after a crash restart before trying again.
const CRASH_PAUSE_SECS: u64 = 30 * 60;
/// Whole update must finish within this, or the device gives up and sleeps.
const WATCHDOG: Duration = Duration::from_secs(300);
/// Settings mode ends after this in any case (battery).
const SETTINGS_MAX: Duration = Duration::from_secs(60 * 60);
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
    /// The state, and the settings if they are usable.
    fn open(part: EspDefaultNvsPartition) -> Result<(Nvs, State, Option<Config>)> {
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
            ready_beep: match nvs.get_u8("ready_beep")? {
                Some(1) => Some(true),
                Some(2) => Some(false),
                _ => None,
            },
            pending_sync: nvs.get_u8("pending_sync")? == Some(1),
        };
        let cfg = crate::settings::load(&nvs);
        Ok((Nvs { nvs, last_ap }, state, cfg))
    }

    fn save(&mut self, s: &State) {
        let nvs = &mut self.nvs;
        let _ = nvs.set_i64("target", s.target);
        let _ = nvs.set_u8("attempt", s.attempt);
        let _ = nvs.set_u8("stock_n", s.stock_count);
        let _ = nvs.set_u32("seen", s.seen);
        let _ = nvs.set_u8("current", s.current);
        let _ = nvs.set_u8("sync_failed", u8::from(s.sync_failed));
        let _ = nvs.set_u8("ready_beep", s.ready_beep.map_or(0, |ok| if ok { 1 } else { 2 }));
        let _ = nvs.set_u8("pending_sync", u8::from(s.pending_sync));
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
    /// `None` while the settings cannot be used (settings mode only).
    cfg: Option<&'d Config>,
    board: Board<'d>,
    epd: Epd<'d>,
    rtc: Rtc<'d>,
    rtc_now: Option<i64>,
    /// When `rtc_now` was read.
    rtc_read: Instant,
    nvs: Nvs,
    /// Taken by the (single) Wi-Fi session of a wake.
    modem: Option<Modem<'static>>,
    sysloop: EspSystemEventLoop,
    nvs_part: EspDefaultNvsPartition,
}

impl Reterminal<'_> {
    /// The time now, from the RTC reading at the start of the wake.
    fn now(&self) -> Option<i64> {
        self.rtc_now.map(|t| t + self.rtc_read.elapsed().as_secs() as i64).or_else(unix_now)
    }
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

    fn go_online(&mut self, now: Option<i64>, batch: u8) -> Result<Online> {
        let tick = |step: &str| log::info!("progress: {step}");
        log_heap("before Wi-Fi");
        // read the stored photo list from flash while Wi-Fi joins
        let preload = if batch > 0 { Some(crate::spawn_with_internal_stack("preload", 8 * 1024, || Store.load_list())?) } else { None };
        let modem = self.modem.take().ok_or_else(|| anyhow!("Wi-Fi was already used in this wake"))?;
        let cfg = self.cfg.ok_or_else(|| anyhow!("no usable settings"))?;
        let (ssid, password) = (cfg.wifi_ssid.as_deref().unwrap_or(""), cfg.wifi_password.as_deref().unwrap_or(""));
        let mut net = Net::connect(modem, self.sysloop.clone(), self.nvs_part.clone(), ssid, password, self.nvs.last_ap)?;
        tick("Wi-Fi up");
        if net.joined.is_some() {
            self.nvs.last_ap = net.joined;
        }
        log_heap("Wi-Fi up");
        let stored_list = preload.and_then(|h| h.join().ok()).flatten();
        // a newer release first: installed, it restarts at once and does this
        // sync itself (a full one), so nothing downloaded here is thrown away
        if cfg.auto_update && crate::ota::auto_update(&mut net, &self.nvs.nvs) {
            let _ = self.nvs.nvs.set_u8("pending_sync", 1);
            net.stop();
            crate::ota::restart();
        }
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
    let mut wake = wake_cause();
    // serial commands (testing); after a power-on (a reset), one typed at
    // once is the button that woke the device
    let typed = crate::serial::start(wake == Wake::PowerOn);
    // a button acts on release; held 10 s, green or right opens settings mode
    let gesture = match wake {
        Wake::Button(p) => Some(crate::sleep::gesture_after_wake(p)),
        Wake::PowerOn => crate::serial::first_command(typed),
        Wake::Timer => None,
    };
    if let (Wake::PowerOn, Some(Gesture::Press(p) | Gesture::Hold(p))) = (wake, gesture) {
        wake = Wake::Button(p);
    }
    log::info!("frogterm {} woke by {wake:?} ({gesture:?})", crate::ota::version());
    crate::ota::log_rollback();
    let p = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs_part = EspDefaultNvsPartition::take()?;
    let (mut nvs, mut state, cfg) = Nvs::open(nvs_part.clone())?;
    if let Some(c) = &cfg {
        tz::set_zone(c.zone());
    }
    let mut rtc = Rtc::new(p.i2c0, p.pins.gpio19, p.pins.gpio20)?;
    let rtc_read = Instant::now();
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
    log::info!("RTC {rtc_now:?}");

    let next = match (gesture, &cfg) {
        (Some(Gesture::Hold(p)), _) => app::via_for(p).map_or(Next::Wake(wake), Next::Settings),
        // nothing to run on: the settings page, on the device's own Wi-Fi
        (_, None) if wake != Wake::Timer => Next::Settings(Via::AccessPoint),
        (_, None) => {
            log::warn!("no usable settings: sleeping until a button is pressed");
            crate::board::set_led(Led::Off);
            planned_sleep(schedule::when_time_unknown())
        }
        _ => Next::Wake(wake),
    };

    // woke early: straight back to sleep, before powering anything up. At
    // the end of the panel's rest: LED off and the final beep (ready)
    if let Some(e) = app::early_wake(wake, rtc_now, &mut state) {
        log::info!("woke early, sleeping {} s more", e.secs);
        if let Some(tones) = e.beep {
            crate::board::play(tones);
            nvs.save(&state);
        }
        crate::board::set_led(e.led);
        planned_sleep(e.secs);
    }
    // busy from here on, until the panel's rest after this wake is over
    crate::board::set_led(Led::On);

    let mut board = Board::new(p.pins.gpio21, p.pins.gpio16, p.pins.gpio14)?;
    let gestures = crate::sleep::spawn_gesture_listener();
    // watchdog: never stay awake (battery) if anything hangs
    let limit = if next == Next::Wake(wake) { WATCHDOG } else { SETTINGS_MAX + Duration::from_secs(120) };
    std::thread::spawn(move || {
        std::thread::sleep(limit);
        log::error!("awake longer than {} s, giving up", limit.as_secs());
        deep_sleep(30 * 60, schedule::arm_button(button_held()));
    });
    let battery_mv = board.battery_mv();

    let mut epd = Epd::new(p.spi2, p.pins.gpio7, p.pins.gpio9, p.pins.gpio10, p.pins.gpio11, p.pins.gpio12, p.pins.gpio13)?;
    if cfg!(feature = "bars") {
        epd.show(&FrameBuffer::colour_bars())?;
        board.beep(beep::SUCCESS);
        deep_sleep(24 * 3600, schedule::arm_button(button_held()));
    }

    let mut dev = Reterminal { cfg: cfg.as_ref(), board, epd, rtc, rtc_now, rtc_read, nvs, modem: Some(p.modem), sysloop, nvs_part };
    let secs = match (next, &cfg) {
        (Next::Settings(via), _) => settings_mode(&mut dev, &mut state, via, &gestures),
        (Next::Wake(wake), Some(cfg)) => {
            let readings = Readings { now: rtc_now, battery_mv, room_c: room.map(|r| r.0) };
            with_rules(cfg, |rules| app::run(&mut dev, &mut state, rules, wake, readings))
        }
        (Next::Wake(_), None) => schedule::when_time_unknown(), // not reached: no settings open settings mode
    };
    planned_sleep(secs)
}

/// The end of a wake that went as planned: this firmware works (a new one
/// is kept, see `ota::confirm`), then deep sleep; the buttons stay active.
fn planned_sleep(secs: u64) -> ! {
    crate::ota::confirm();
    deep_sleep(secs, schedule::arm_button(button_held()))
}

/// What this wake does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    Wake(Wake),
    Settings(Via),
}

/// `f` with the wake logic's settings from `cfg`.
fn with_rules<T>(cfg: &Config, f: impl FnOnce(&app::Config) -> T) -> T {
    let times = cfg.update_times();
    f(&app::Config {
        update_times: &times,
        refresh_minutes: cfg.refresh_minutes,
        photo_batch: cfg.photo_batch,
        opacity: cfg.overlay_opacity,
        place: cfg.label(),
        panel_rest: true,
    })
}

/// Settings mode: the page on the network `via`, until green is pressed (or
/// nobody opened the page for 15 min) with usable settings. Returns the
/// seconds to sleep; the sync with the new settings comes at that timer wake
/// (fresh Wi-Fi, the settings read again).
fn settings_mode(dev: &mut Reterminal, state: &mut State, via: Via, gestures: &Receiver<Gesture>) -> u64 {
    app::start_settings(dev);
    let Some(modem) = dev.modem.take() else { return app::settings_failed(dev, state, dev.now()) };
    let portal = match crate::portal::Portal::open(via, modem, dev.sysloop.clone(), dev.nvs_part.clone(), dev.cfg, dev.nvs.last_ap) {
        Ok(p) => p,
        Err(e) => {
            log::error!("settings mode could not start: {e:#}");
            return app::settings_failed(dev, state, dev.now());
        }
    };
    if let Err(e) = app::show_settings(dev, state, &portal.network_name, &portal.address, dev.now()) {
        log::warn!("settings screen: {e:#}");
    }
    crate::sleep::set_settings_mode(true);
    let started = Instant::now();
    let idle_limit = Duration::from_secs(app::SETTINGS_IDLE_SECS);
    let secs = loop {
        let pressed = gestures.recv_timeout(Duration::from_secs(1)).is_ok();
        let idle = portal.idle() >= idle_limit;
        let over = started.elapsed() >= SETTINGS_MAX;
        let installed = portal.installed();
        if !(pressed || idle || over || installed) {
            continue;
        }
        if installed {
            // let the browser get its answer, then start the new firmware at
            // once; with usable settings it syncs (after the panel's rest)
            std::thread::sleep(Duration::from_secs(2));
            if let Some(cfg) = crate::settings::load(&dev.nvs.nvs) {
                with_rules(&cfg, |rules| app::end_settings(dev, state, rules, dev.now()));
            }
            crate::sleep::set_settings_mode(false);
            portal.close();
            crate::ota::restart();
        }
        if idle {
            log::info!("nobody opened the settings page for {} min", app::SETTINGS_IDLE_SECS / 60);
        }
        match crate::settings::load(&dev.nvs.nvs) {
            Some(cfg) => break with_rules(&cfg, |rules| app::end_settings(dev, state, rules, dev.now())),
            // still unusable: green stays in settings mode; left alone, the
            // device sleeps (battery) until a button opens it again
            None if pressed => dev.beep(beep::ERROR),
            None => {
                dev.beep(beep::ERROR);
                dev.led(Led::Off);
                break schedule::when_time_unknown();
            }
        }
    };
    crate::sleep::set_settings_mode(false);
    portal.close();
    secs
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
fn online(cfg: &Config, net: &mut Net, now: i64, batch: u8, stored_list: Option<PhotoList>) -> Result<Online> {
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
                .filter_map(|_| random_screen(&mut list, &mut store, cfg.overlay_opacity, now).map_err(|e| log::warn!("{e:#}")).ok())
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
fn photo_list(cfg: &Config, net: &mut Net, store: &mut Store, now: i64, old: Option<PhotoList>) -> Result<PhotoList> {
    if let Some(l) = old.as_ref().filter(|l| l.is_fresh_for(&cfg.album_url, now as u64)) {
        log::info!("photo list: {} photos, {:.1} days old", l.photos.len(), l.age_days(now as u64));
        return old.ok_or_else(|| anyhow!("no photo list"));
    }
    let (fetched, complete) = read_album(net, &cfg.album_url);
    log::info!("album read: {} photos, complete: {complete}", fetched.len());
    // a failed read keeps the old list as it is in flash: nothing to save
    let keeps_old = !(complete && !fetched.is_empty()) && old.is_some();
    let list = after_refresh(old, fetched, complete, &cfg.album_url, now as u64).ok_or_else(|| anyhow!("no photo list"))?;
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

fn fetch_weather(cfg: &Config) -> Result<Forecast> {
    let t = std::time::Instant::now();
    let (lat, lon) = match cfg.coordinates() {
        Some(c) => c,
        None => {
            let body = net::get_bytes(&weather::geocode_url(&cfg.city))?;
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
fn fetch_calendars(cfg: &Config, today: frogterm_core::date::Date) -> (Option<calendar::TwoDays>, usize) {
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
