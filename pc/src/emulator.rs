//! The device on the PC: the firmware's wake logic (`frogterm_core::app`)
//! with a PC `Device` — the panel is a window (or out/screen.png), flash is
//! out/, Wi-Fi is the PC's internet. Click the three buttons in the window as
//! you would press them on the reTerminal; the timer wakes (scheduled updates,
//! retries) happen too while the emulator runs.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context, Result};
use embedded_graphics::pixelcolor::RgbColor;
use frogterm_core::app::{self, Device, Led, Online, Readings, State, Via, Wake};
use frogterm_core::beep::{self, Tone};
use frogterm_core::buttons::{Gesture, Press};
use frogterm_core::calendar::{self, DayEvent, IcsScanner};
use frogterm_core::date::Date;
use frogterm_core::framebuf::{FrameBuffer, Ink, HEIGHT, WIDTH};
use frogterm_core::info::Info;
use frogterm_core::photo_list::PhotoList;
use frogterm_core::screen::{self, Slot};
use frogterm_core::weather::{self, Forecast};
use frogterm_core::{battery, tz};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::config::Config;
use crate::net;
use crate::portal_server::{self, Served, Server};
use crate::window;

const MAX_PAGES: usize = 100;
/// The settings page's port, the first free one of these.
const SETTINGS_PORTS: std::ops::RangeInclusive<u16> = 8080..=8089;
const ATTEMPTS: usize = 3;

struct Options {
    config: PathBuf,
    out_dir: PathBuf,
    refresh: bool,
    /// Overrides `overlay_opacity` from the config.
    opacity: Option<u8>,
    /// Fixed random seed, to compare settings on the same photos.
    seed: Option<u64>,
    /// The PC has no sensors: pretend values to preview the header.
    room_c: Option<f32>,
    battery_pct: Option<u8>,
    /// No window: one wake (power-on, or this button / hold), then exit.
    once: Option<Option<Gesture>>,
    /// Skip the e-paper rest between refreshes.
    no_rest: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: frogterm-pc emulator [options]\n\
         \n\
         Runs the device in a window: click its three buttons (or press\n\
         Left / Space / Right). Without a display use --once.\n\
         \n\
         --config FILE    album link and city (default: config.toml)\n\
         --out DIR        the emulated flash and screen.png (default: out)\n\
         --refresh        re-read the whole album on the next photo download\n\
         --opacity N      box opacity 0..100 (overrides overlay_opacity)\n\
         --seed N         fixed random seed (same photos every run)\n\
         --room C         pretend room temperature (the PC has no sensor)\n\
         --battery P      pretend battery percentage\n\
         --no-rest        refresh at once, without the panel's rest between refreshes\n\
         --once [BUTTON]  no window: one wake (power-on, or BUTTON = green, next,\n\
         \x20                previous; hold-green, hold-next = the settings screen),\n\
         \x20                save out/screen.png (the panel) and out/window.png\n\
         \x20                (the whole window) and exit\n\
         \n\
         Hold green 10 s: settings mode as the device's own Wi-Fi; hold right\n\
         10 s: settings mode on this PC's network. The settings page is served\n\
         on port 8080 and rewrites --config. Without usable settings the\n\
         emulator starts in settings mode."
    );
    std::process::exit(2);
}

/// `--once` button: a press, or `hold-green` / `hold-next` (10-s holds).
fn parse_once(s: &str) -> Option<Gesture> {
    match s {
        "hold-green" | "hold-update" => Some(Gesture::Hold(Press::Update)),
        "hold-next" | "hold-right" => Some(Gesture::Hold(Press::Next)),
        _ => parse_press(s).map(Gesture::Press),
    }
}

fn parse_press(s: &str) -> Option<Press> {
    match s {
        "green" | "update" => Some(Press::Update),
        "next" | "right" => Some(Press::Next),
        "previous" | "prev" | "left" => Some(Press::Previous),
        _ => None,
    }
}

pub fn main(args: &[String]) -> Result<()> {
    let mut o = Options {
        config: "config.toml".into(),
        out_dir: "out".into(),
        refresh: false,
        opacity: None,
        seed: None,
        room_c: None,
        battery_pct: None,
        once: None,
        no_rest: false,
    };
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => o.config = it.next().unwrap_or_else(|| usage()).into(),
            "--out" => o.out_dir = it.next().unwrap_or_else(|| usage()).into(),
            "--refresh" => o.refresh = true,
            "--no-rest" => o.no_rest = true,
            "--opacity" => o.opacity = Some(it.next().and_then(|v| v.parse().ok()).filter(|&v| v <= 100).unwrap_or_else(|| usage())),
            "--room" => o.room_c = Some(it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage())),
            "--battery" => o.battery_pct = Some(it.next().and_then(|v| v.parse().ok()).filter(|&v| v <= 100).unwrap_or_else(|| usage())),
            "--seed" => o.seed = Some(it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage())),
            "--once" => o.once = Some(it.next_if(|v| !v.starts_with("--")).map(|v| parse_once(v).unwrap_or_else(|| usage()))),
            _ => usage(),
        }
    }
    crate::logger::init();
    // settings that cannot be used: the device starts in settings mode
    let cfg = match Config::load(&o.config) {
        Ok(c) => Some(c),
        Err(e) => {
            log::warn!("{e:#}: starting in settings mode");
            None
        }
    };
    if let Some(c) = &cfg {
        tz::set_zone(c.zone());
    }
    fs::create_dir_all(o.out_dir.join("screens")).with_context(|| format!("create {}", o.out_dir.display()))?;
    let ui = Arc::new(window::Shared::default());
    let mut dev = Emulated::new(&o, cfg, ui.clone());
    let mut state = load_state(&o.out_dir);
    println!("frogterm emulator (config {}, flash in {})", o.config.display(), o.out_dir.display());
    let first = if dev.cfg.is_some() { Next::Wake(Wake::PowerOn) } else { Next::Settings(Via::AccessPoint) };

    if let Some(gesture) = o.once {
        let next = match gesture {
            Some(g) if dev.cfg.is_some() || matches!(g, Gesture::Hold(_)) => Next::after(g),
            _ => first,
        };
        match next {
            Next::Wake(wake) => {
                let secs = dev.wake(&mut state, wake);
                println!("the device would now sleep {}", window::duration(secs));
                ui.sleeping(Instant::now() + Duration::from_secs(secs));
            }
            Next::Settings(via) => match dev.open_settings(&mut state, via) {
                Ok(()) => {
                    ui.set_settings(true);
                    println!("the device would now serve the settings page");
                }
                Err(secs) => println!("settings mode failed; the device would now sleep {}", window::duration(secs)),
            },
        }
        if let Some(img) = window::snapshot(&ui) {
            let png = o.out_dir.join("window.png");
            img.save(&png).with_context(|| format!("save {}", png.display()))?;
            println!("the window: {}", png.display());
        }
        return Ok(());
    }

    // the device runs in its own thread, like the real one, while the window
    // takes the button clicks
    let (tx, rx) = mpsc::channel::<Gesture>();
    let device_ui = ui.clone();
    std::thread::Builder::new().name("device".into()).spawn(move || {
        let mut next = first;
        loop {
            device_ui.set_busy(true);
            let secs = match next {
                Next::Wake(wake) => dev.wake(&mut state, wake),
                Next::Settings(via) => match dev.settings(&mut state, via, &rx) {
                    SettingsEnd::Sync => dev.wake(&mut state, Wake::Button(Press::Update)),
                    SettingsEnd::Failed(secs) => secs,
                    SettingsEnd::Closed => return,
                },
            };
            device_ui.sleeping(Instant::now() + Duration::from_secs(secs));
            // deep sleep: a button press or the timer wakes the device
            next = match rx.recv_timeout(Duration::from_secs(secs)) {
                Ok(g) => Next::after(g),
                Err(RecvTimeoutError::Timeout) => Next::Wake(Wake::Timer),
                Err(RecvTimeoutError::Disconnected) => return,
            };
        }
    })?;
    window::run(&ui, tx)
}

/// What the device does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    Wake(Wake),
    Settings(Via),
}

impl Next {
    /// After a button gesture: a press wakes the device, a hold opens settings mode.
    fn after(g: Gesture) -> Next {
        match g {
            Gesture::Press(p) => Next::Wake(Wake::Button(p)),
            Gesture::Hold(p) => app::via_for(p).map_or(Next::Wake(Wake::Button(p)), Next::Settings),
        }
    }
}

/// How settings mode ended.
enum SettingsEnd {
    /// Green (or the idle timeout) with usable settings: a green-button sync next.
    Sync,
    /// It could not start: sleep this long.
    Failed(u64),
    /// The window was closed.
    Closed,
}

/// The PC as the reTerminal.
struct Emulated {
    /// `None` while the settings cannot be used (settings mode until they can).
    cfg: Option<Config>,
    config_path: PathBuf,
    out_dir: PathBuf,
    agent: ureq::Agent,
    rng: StdRng,
    refresh_album: bool,
    /// `--opacity`, over the config's `overlay_opacity`.
    opacity: Option<u8>,
    panel_rest: bool,
    room_c: Option<f32>,
    battery_mv: Option<u32>,
    ui: Arc<window::Shared>,
    /// The settings page's server, started the first time settings mode opens.
    server: Option<Server>,
}

impl Emulated {
    fn new(o: &Options, cfg: Option<Config>, ui: Arc<window::Shared>) -> Emulated {
        Emulated {
            config_path: o.config.clone(),
            out_dir: o.out_dir.clone(),
            agent: net::agent(),
            rng: o.seed.map_or_else(StdRng::from_entropy, StdRng::seed_from_u64),
            refresh_album: o.refresh,
            opacity: o.opacity,
            panel_rest: !o.no_rest,
            room_c: o.room_c,
            battery_mv: o.battery_pct.map(battery_mv_for),
            cfg,
            ui,
            server: None,
        }
    }

    /// The settings, or an error while they cannot be used.
    fn settings_now(&self) -> Result<Config> {
        self.cfg.clone().context("no usable settings (hold green 10 s for the settings page)")
    }

    fn box_opacity(&self, cfg: &Config) -> u8 {
        self.opacity.unwrap_or(cfg.overlay_opacity)
    }

    /// One wake with the shared logic; returns the seconds to sleep.
    fn wake(&mut self, state: &mut State, wake: Wake) -> u64 {
        println!();
        let Ok(cfg) = self.settings_now() else {
            log::warn!("no usable settings: nothing to do until settings mode fixes them");
            return frogterm_core::schedule::when_time_unknown();
        };
        let label = cfg.label().to_string();
        let times = cfg.update_times();
        let rules = app::Config {
            update_times: &times,
            refresh_minutes: cfg.refresh_minutes,
            photo_batch: cfg.photo_batch,
            opacity: self.box_opacity(&cfg),
            place: &label,
            panel_rest: self.panel_rest,
        };
        let readings = Readings { now: self.clock(), battery_mv: self.battery_mv, room_c: self.room_c };
        app::run(self, state, &rules, wake, readings)
    }

    /// Settings mode up to the screen: the chirp, the page's server, the
    /// screen with where to find it. `Err` = it could not start (sleep this long).
    fn open_settings(&mut self, state: &mut State, via: Via) -> Result<(), u64> {
        app::start_settings(self);
        let port = match &self.server {
            Some(s) => s.port(),
            None => match Server::bind(SETTINGS_PORTS) {
                Ok(s) => {
                    let port = s.port();
                    self.server = Some(s);
                    port
                }
                Err(e) => {
                    log::error!("{e:#}");
                    return Err(app::settings_failed(self, state, self.clock()));
                }
            },
        };
        // the PC cannot be an access point: the page is on this PC, the screen
        // shows the device's network name
        let (network, address) = match via {
            Via::AccessPoint => (app::AP_SSID.to_string(), format!("http://localhost:{port}/")),
            Via::HomeWifi => match portal_server::lan_address() {
                Some(ip) => (
                    self.cfg.as_ref().and_then(|c| c.wifi_ssid.clone()).unwrap_or_else(|| "this PC's network".into()),
                    format!("http://{ip}:{port}/"),
                ),
                None => {
                    log::error!("settings mode: this PC is on no network");
                    return Err(app::settings_failed(self, state, self.clock()));
                }
            },
        };
        log::info!("settings page at {address} (Wi-Fi {network})");
        if let Err(e) = app::show_settings(self, state, &network, &address, self.clock()) {
            log::warn!("settings screen: {e:#}");
        }
        Ok(())
    }

    /// Settings mode: serves the page until green is pressed (or nobody
    /// opened it for `SETTINGS_IDLE_SECS`) with usable settings.
    fn settings(&mut self, state: &mut State, via: Via, gestures: &Receiver<Gesture>) -> SettingsEnd {
        if let Err(secs) = self.open_settings(state, via) {
            return SettingsEnd::Failed(secs);
        }
        self.ui.set_settings(true);
        let Some(mut server) = self.server.take() else { return SettingsEnd::Failed(frogterm_core::schedule::when_time_unknown()) };
        let idle = Duration::from_secs(app::SETTINGS_IDLE_SECS);
        let mut last_request = Instant::now();
        let end = loop {
            let finish = match gestures.try_recv() {
                Ok(Gesture::Press(Press::Update) | Gesture::Hold(Press::Update)) => true,
                Ok(_) | Err(TryRecvError::Empty) => false,
                Err(TryRecvError::Disconnected) => break SettingsEnd::Closed,
            };
            let timed_out = last_request.elapsed() >= idle;
            if timed_out {
                log::info!("nobody opened the settings page for {} min", app::SETTINGS_IDLE_SECS / 60);
                last_request = Instant::now();
            }
            if finish || timed_out {
                if self.reload() {
                    break SettingsEnd::Sync;
                }
                self.beep(beep::ERROR); // settings still unusable: stays in settings mode
            }
            match server.serve_once(&self.config_path, Duration::from_millis(100)) {
                Ok(Served::Nothing) => {}
                Ok(Served::Request | Served::Saved) => last_request = Instant::now(),
                Err(e) => log::warn!("settings page: {e:#}"),
            }
        };
        self.server = Some(server);
        self.ui.set_settings(false);
        log::info!("settings mode ends");
        end
    }

    /// Reads the settings file again; false (with the reason logged) if it
    /// cannot be used.
    fn reload(&mut self) -> bool {
        match Config::load(&self.config_path) {
            Ok(c) => {
                tz::set_zone(c.zone());
                self.cfg = Some(c);
                true
            }
            Err(e) => {
                log::warn!("{e:#}");
                false
            }
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.out_dir.join(name)
    }
}

/// The buzzer: the PC names the sound in the log (and rings the terminal
/// bell for the final beeps).
pub fn play(tones: &[Tone]) {
    let sound = match tones {
        t if t == beep::PRESS => "chirp (press noticed)",
        t if t == beep::BUSY => "pip-pip-pip (busy, press ignored)",
        t if t == beep::SUCCESS => "beep (done, ready)",
        t if t == beep::ERROR => "low beep beep beep (failed, ready)",
        _ => "beep",
    };
    log::info!("sound: {sound}");
    if tones == beep::SUCCESS || tones == beep::ERROR {
        print!("\x07");
        std::io::stdout().flush().ok();
    }
}

/// Battery voltage that shows as `pct` %.
fn battery_mv_for(pct: u8) -> u32 {
    (3000..=4300).find(|&mv| battery::percent(mv) >= pct).unwrap_or(4300)
}

impl Device for Emulated {
    fn beep(&mut self, tones: &[Tone]) {
        play(tones);
    }

    fn led(&mut self, led: Led) {
        log::info!(
            "LED {}",
            match led {
                Led::Off => "off (ready)",
                Led::On => "on (busy)",
            }
        );
        self.ui.set_led(led);
    }

    fn clock(&self) -> Option<i64> {
        Some(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
    }

    fn show(&mut self, fb: FrameBuffer) -> Result<()> {
        let png = self.path("screen.png");
        to_rgb_image(&fb).save(&png).with_context(|| format!("save {}", png.display()))?;
        log::info!("panel refreshed ({})", png.display());
        self.ui.show(fb);
        Ok(())
    }

    fn load_screen(&mut self, slot: u8) -> Option<FrameBuffer> {
        FrameBuffer::from_vec(fs::read(self.path(&format!("screens/{slot}.bin"))).ok()?)
    }

    fn save_screen(&mut self, slot: u8, fb: &FrameBuffer) -> Result<()> {
        let path = self.path(&format!("screens/{slot}.bin"));
        fs::write(&path, fb.as_bytes()).with_context(|| format!("save {}", path.display()))
    }

    fn load_info(&mut self) -> Option<Info> {
        Info::from_text(&fs::read_to_string(self.path("info.txt")).ok()?)
    }

    fn save_info(&mut self, info: &Info) {
        if let Err(e) = fs::write(self.path("info.txt"), info.to_text()) {
            log::warn!("could not store weather/calendar: {e}");
        }
    }

    fn save_state(&mut self, state: &State) {
        if let Err(e) = fs::write(self.path("state.txt"), state_to_text(state)) {
            log::warn!("could not store the state: {e}");
        }
    }

    fn go_online(&mut self, now: Option<i64>, batch: u8) -> Result<Online> {
        let now = now.or_else(|| self.clock()).context("time unknown")?;
        log::info!("online: weather, calendars{}", if batch > 0 { format!(" and {batch} new photos") } else { String::new() });
        let today = tz::to_local(now).0;
        let cfg = self.settings_now()?;
        let (agent, cfg) = (&self.agent, &cfg);
        let (forecast, events) = std::thread::scope(|s| {
            let weather = s.spawn(|| fetch_weather(agent, cfg).map_err(|e| log::warn!("weather: {e:#}")).ok());
            let events = fetch_calendars(agent, cfg, today);
            (weather.join().ok().flatten(), events)
        });
        let (events, calendars_failed) = events;
        let screens = if batch > 0 { self.photo_batch(now as u64, batch)? } else { Vec::new() };
        // all calendars failing shows as `events: None`; some of them only here
        let missing = if calendars_failed > 0 && events.is_some() { vec!["a calendar"] } else { vec![] };
        Ok(Online { now, forecast, events, batch: screens, missing })
    }
}

impl Emulated {
    /// `n` ready-to-show photo screens, downloaded at the same time.
    fn photo_batch(&mut self, now: u64, n: u8) -> Result<Vec<FrameBuffer>> {
        let list_path = self.path("photo_list.txt");
        let cfg = self.settings_now()?;
        let mut list = photo_list(&self.agent, &cfg, &list_path, now, std::mem::take(&mut self.refresh_album))?;
        let t = Instant::now();
        let seeds: Vec<u64> = (0..n).map(|_| self.rng.gen()).collect();
        let (agent, list_ref, opacity) = (&self.agent, &list, self.box_opacity(&cfg));
        let results: Vec<Result<FrameBuffer, bool>> = std::thread::scope(|s| {
            let handles: Vec<_> = seeds
                .into_iter()
                .map(|seed| s.spawn(move || random_screen(agent, list_ref, opacity, &mut StdRng::seed_from_u64(seed))))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or(Err(false))).collect()
        });
        if results.iter().any(|r| matches!(r, Err(true))) {
            // cached photo links expired: re-read the album next time
            log::warn!("photo links no longer work; the list is {:.1} days old, it will be re-read", list.age_days(now));
            list.fetched_at = 0;
            fs::write(&list_path, list.to_text())?;
        }
        let screens: Vec<FrameBuffer> = results.into_iter().filter_map(Result::ok).collect();
        log::info!("downloaded {} of {n} photo screens in {:.1} s", screens.len(), t.elapsed().as_secs_f32());
        Ok(screens)
    }
}

/// Cached list if fresh, otherwise read the whole album and cache it.
fn photo_list(agent: &ureq::Agent, cfg: &Config, path: &Path, now: u64, refresh: bool) -> Result<PhotoList> {
    let cached = fs::read_to_string(path).ok().and_then(|t| PhotoList::from_text(&t));
    if let Some(l) = cached.filter(|l| !refresh && l.is_fresh_for(&cfg.album_url, now)) {
        log::info!("photo list: {} photos, {:.1} days old (re-read after 7 days)", l.photos.len(), l.age_days(now));
        return Ok(l);
    }
    log::info!("reading the whole album");
    let t = Instant::now();
    let photos = net::fetch_album(agent, &cfg.album_url, MAX_PAGES, |_| {
        print!(".");
        std::io::stdout().flush().ok();
    })?;
    println!();
    log::info!("album: {} photos, {:.1} s", photos.len(), t.elapsed().as_secs_f32());
    ensure!(!photos.is_empty(), "the album has no photos");
    let list = PhotoList { album_url: cfg.album_url.clone(), fetched_at: now, photos };
    fs::write(path, list.to_text()).with_context(|| format!("save {}", path.display()))?;
    Ok(list)
}

/// Up to `ATTEMPTS` tries: a random photo (plus a tall partner if it is tall),
/// each downloaded already sized for its slot and placed as the device does.
/// `Err(true)` = the photo links expired.
fn random_screen(agent: &ureq::Agent, list: &PhotoList, opacity: u8, rng: &mut StdRng) -> Result<FrameBuffer, bool> {
    let mut expired = false;
    for _ in 0..ATTEMPTS {
        let first = rng.gen_range(0..list.photos.len());
        let slots = screen::plan(&list.photos, first, screen::FRESH, |n| rng.gen_range(0..n));
        let mut fb = FrameBuffer::new(Ink::White);
        match fill_slots(agent, list, &slots, opacity, &mut fb) {
            Ok(()) => return Ok(fb),
            Err((i, e)) => {
                log::warn!("photo #{i}: {e:#}");
                expired |= net::is_status(&e, &[403, 404, 410]);
            }
        }
    }
    log::warn!("{ATTEMPTS} tries in a row failed");
    Err(expired)
}

/// Downloads and places every slot; on failure returns the photo index and the error.
fn fill_slots(
    agent: &ureq::Agent,
    list: &PhotoList,
    slots: &[Slot],
    opacity: u8,
    fb: &mut FrameBuffer,
) -> Result<(), (usize, anyhow::Error)> {
    for s in slots {
        let p = &list.photos[s.photo];
        let (w, h) = (s.area.size.width, s.area.size.height);
        let jpeg = net::download_photo(agent, p, w, h).map_err(|e| (s.photo, e))?;
        let rgb = decode(&jpeg.bytes, w, h).map_err(|e| (s.photo, e))?;
        app::place_photo(fb, &s.area, rgb, opacity, p.taken).map_err(|e| (s.photo, e))?;
        log::info!(
            "photo #{} of {} ({}x{} {}, taken {}): {} KB as {w}x{h}",
            s.photo,
            list.photos.len(),
            p.width,
            p.height,
            if p.is_tall() { "tall" } else { "wide" },
            p.taken.map_or("?".to_string(), |d| d.iso()),
            jpeg.bytes.len() / 1024,
        );
    }
    Ok(())
}

/// JPEG → `w`×`h` RGB bytes with the device's decoder; resize only as a
/// safety net if Google ever sends another size.
fn decode(jpeg: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    if let Ok(rgb) = frogterm_core::jpeg::decode_rgb(jpeg, w, h) {
        return Ok(rgb);
    }
    let img = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).context("decode JPEG")?;
    let img = if (img.width(), img.height()) == (w, h) { img } else { img.resize_to_fill(w, h, image::imageops::FilterType::Triangle) };
    Ok(img.to_rgb8().into_raw())
}

/// Today's and tomorrow's events of all calendars (`None` if none could be
/// read; none configured is an empty day) and how many calendars failed.
fn fetch_calendars(agent: &ureq::Agent, cfg: &Config, today: Date) -> (Option<calendar::TwoDays>, usize) {
    if cfg.calendars.is_empty() {
        log::info!("no calendars in config");
        return (Some((Vec::new(), Vec::new())), 0);
    }
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = cfg.calendars.iter().map(|url| s.spawn(move || read_calendar(agent, url, today))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("thread panicked")))).collect()
    });
    let (mut all_today, mut all_tomorrow) = (Vec::new(), Vec::new());
    let mut ok = 0;
    for (i, r) in results.into_iter().enumerate() {
        match r {
            Ok((t, tm, bytes)) => {
                ok += 1;
                log::info!("calendar {}: {} events today, {} tomorrow, {} KB", i + 1, t.len(), tm.len(), bytes / 1024);
                all_today.extend(t);
                all_tomorrow.extend(tm);
            }
            Err(e) => log::warn!("calendar {}: {e:#}", i + 1),
        }
    }
    let failed = cfg.calendars.len() - ok;
    if ok == 0 {
        return (None, failed);
    }
    calendar::sort_events(&mut all_today);
    calendar::sort_events(&mut all_tomorrow);
    (Some((all_today, all_tomorrow)), failed)
}

/// Streams one iCal file through two scanners (today, tomorrow), as the
/// device does; returns both days' events and the size.
fn read_calendar(agent: &ureq::Agent, url: &str, today: Date) -> Result<(Vec<DayEvent>, Vec<DayEvent>, usize)> {
    let resp = agent.get(url).call().map_err(net::http_err).context("download calendar")?;
    let mut reader = resp.into_reader();
    let mut scan_today = IcsScanner::new(today);
    let mut scan_tomorrow = IcsScanner::new(Date::from_days(today.days_since_epoch() + 1));
    let mut buf = [0u8; 4096];
    let mut total = 0;
    loop {
        let n = std::io::Read::read(&mut reader, &mut buf)?;
        if n == 0 {
            break;
        }
        total += n;
        scan_today.feed(&buf[..n]);
        scan_tomorrow.feed(&buf[..n]);
    }
    Ok((scan_today.finish(), scan_tomorrow.finish(), total))
}

fn fetch_weather(agent: &ureq::Agent, cfg: &Config) -> Result<Forecast> {
    let (lat, lon) = match cfg.coordinates() {
        Some(c) => c,
        None => {
            let place = weather::parse_geocode(&net::get_text(agent, &weather::geocode_url(&cfg.city))?)
                .with_context(|| format!("looking up city {:?}", cfg.city))?;
            (place.latitude, place.longitude)
        }
    };
    let f = weather::parse_forecast(&net::get_text(agent, &weather::forecast_url(lat, lon))?)?;
    let line: Vec<String> = f
        .slots
        .iter()
        .step_by(weather::STEP_HOURS)
        .take(weather::SLOTS)
        .map(|s| format!("{:02}h {:.0}° {}%", s.hour, s.temp_c, s.precip_pct))
        .collect();
    log::info!("weather at {lat:.4}, {lon:.4}: {}", line.join(" | "));
    Ok(f)
}

const STATE_HEADER: &str = "frogterm-state v1";

/// The device's NVS state as text (out/state.txt).
fn state_to_text(s: &State) -> String {
    format!(
        "{STATE_HEADER}\ntarget {}\nattempt {}\nstock {}\nseen {}\nrefreshed {}\ncurrent {}\nsync_failed {}\nready_beep {}\n",
        s.target,
        s.attempt,
        s.stock_count,
        s.seen,
        s.last_refresh.map_or("-".into(), |t| t.to_string()),
        s.current,
        u8::from(s.sync_failed),
        s.ready_beep.map_or("-", |ok| if ok { "ok" } else { "error" })
    )
}

fn state_from_text(text: &str) -> Option<State> {
    let mut lines = text.lines();
    if lines.next()? != STATE_HEADER {
        return None;
    }
    let mut s = State::default();
    for line in lines {
        let (k, v) = line.split_once(' ')?;
        match k {
            "target" => s.target = v.parse().ok()?,
            "attempt" => s.attempt = v.parse().ok()?,
            "stock" => s.stock_count = v.parse().ok()?,
            "seen" => s.seen = v.parse().ok()?,
            "refreshed" => s.last_refresh = if v == "-" { None } else { Some(v.parse().ok()?) },
            "current" => s.current = v.parse().ok()?,
            "sync_failed" => s.sync_failed = v == "1",
            "ready_beep" => s.ready_beep = if v == "-" { None } else { Some(v == "ok") },
            _ => {}
        }
    }
    Some(s)
}

/// The stored state, or a fresh device.
fn load_state(out_dir: &Path) -> State {
    fs::read_to_string(out_dir.join("state.txt")).ok().and_then(|t| state_from_text(&t)).unwrap_or_default()
}

/// What the panel would show, using the inks' real-looking colours.
pub fn to_rgb_image(fb: &FrameBuffer) -> image::RgbImage {
    image::RgbImage::from_fn(WIDTH, HEIGHT, |x, y| {
        let c = fb.get(x, y).rgb();
        image::Rgb([c.r(), c.g(), c.b()])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_image_uses_panel_colours() {
        let mut fb = FrameBuffer::new(Ink::White);
        fb.set(0, 0, Ink::Red);
        fb.set(WIDTH - 1, HEIGHT - 1, Ink::Blue);
        let img = to_rgb_image(&fb);
        assert_eq!(img.dimensions(), (WIDTH, HEIGHT));
        let px = |x, y| img.get_pixel(x, y).0;
        let c = Ink::Red.rgb();
        assert_eq!(px(0, 0), [c.r(), c.g(), c.b()]);
        let c = Ink::Blue.rgb();
        assert_eq!(px(WIDTH - 1, HEIGHT - 1), [c.r(), c.g(), c.b()]);
        let c = Ink::White.rgb();
        assert_eq!(px(1, 0), [c.r(), c.g(), c.b()]);
    }

    #[test]
    fn state_round_trips_through_text() {
        let s = State {
            target: 1_795_600_000,
            attempt: 2,
            stock_count: 10,
            seen: 0b1011,
            last_refresh: Some(1_795_590_000),
            current: 3,
            sync_failed: true,
            ready_beep: Some(false),
        };
        assert_eq!(state_from_text(&state_to_text(&s)), Some(s));
        assert_eq!(state_from_text(&state_to_text(&State::default())), Some(State::default()));
        assert_eq!(state_from_text("garbage"), None);
    }

    #[test]
    fn pretend_battery_percentage_maps_to_a_voltage() {
        for pct in [0, 50, 90, 100] {
            assert!(battery::percent(battery_mv_for(pct)) >= pct);
        }
        assert!(!battery::too_low_to_update(battery_mv_for(50)));
    }

    #[test]
    fn once_takes_holds_too() {
        assert_eq!(parse_once("hold-green"), Some(Gesture::Hold(Press::Update)));
        assert_eq!(parse_once("hold-next"), Some(Gesture::Hold(Press::Next)));
        assert_eq!(parse_once("next"), Some(Gesture::Press(Press::Next)));
        assert_eq!(parse_once("hold-blue"), None);
        assert_eq!(Next::after(Gesture::Hold(Press::Update)), Next::Settings(Via::AccessPoint));
        assert_eq!(Next::after(Gesture::Hold(Press::Next)), Next::Settings(Via::HomeWifi));
        assert_eq!(Next::after(Gesture::Press(Press::Update)), Next::Wake(Wake::Button(Press::Update)));
    }

    #[test]
    fn button_names() {
        assert_eq!(parse_press("green"), Some(Press::Update));
        assert_eq!(parse_press("next"), Some(Press::Next));
        assert_eq!(parse_press("previous"), Some(Press::Previous));
        assert_eq!(parse_press("blue"), None);
    }
}
