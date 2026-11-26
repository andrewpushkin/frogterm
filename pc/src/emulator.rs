//! The device on the PC: the firmware's wake logic (`frogterm_core::app`)
//! with a PC `Device` — the panel is a window (or out/screen.png), flash is
//! out/, Wi-Fi is the PC's internet. Click the three buttons in the window as
//! you would press them on the reTerminal; the timer wakes (06:00, retries,
//! postponed presses) happen too while the emulator runs.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context, Result};
use embedded_graphics::pixelcolor::RgbColor;
use frogterm_core::app::{self, Device, Online, Readings, State, Wake};
use frogterm_core::beep::{self, Tone};
use frogterm_core::buttons::Press;
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
use crate::window;

const MAX_PAGES: usize = 100;
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
    /// No window: one wake (power-on, or this button), then exit.
    once: Option<Option<Press>>,
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
         \x20                previous), save out/screen.png and exit"
    );
    std::process::exit(2);
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
            "--once" => o.once = Some(it.next_if(|v| !v.starts_with("--")).map(|v| parse_press(v).unwrap_or_else(|| usage()))),
            _ => usage(),
        }
    }
    crate::logger::init();
    let cfg = Config::load(&o.config)?;
    tz::set_zone(cfg.zone());
    fs::create_dir_all(o.out_dir.join("screens")).with_context(|| format!("create {}", o.out_dir.display()))?;
    let ui = Arc::new(window::Shared::default());
    let mut dev = Emulated::new(&o, cfg, ui.clone())?;
    let mut state = load_state(&o.out_dir);
    println!("frogterm emulator (config {}, flash in {})", o.config.display(), o.out_dir.display());

    if let Some(press) = o.once {
        let wake = press.map_or(Wake::PowerOn, Wake::Button);
        let secs = dev.wake(&mut state, wake);
        println!("the device would now sleep {}", window::duration(secs));
        return Ok(());
    }

    // the device runs in its own thread, like the real one, while the window
    // takes the button clicks
    let (tx, rx) = mpsc::channel::<Press>();
    let device_ui = ui.clone();
    std::thread::Builder::new().name("device".into()).spawn(move || {
        let mut wake = Wake::PowerOn;
        loop {
            device_ui.set_busy(true);
            let secs = dev.wake(&mut state, wake);
            device_ui.sleeping(Instant::now() + Duration::from_secs(secs));
            // deep sleep: a button press or the timer wakes the device
            wake = match rx.recv_timeout(Duration::from_secs(secs)) {
                Ok(p) => Wake::Button(p),
                Err(RecvTimeoutError::Timeout) => Wake::Timer,
                Err(RecvTimeoutError::Disconnected) => return,
            };
        }
    })?;
    window::run(&ui, tx)
}

/// The PC as the reTerminal.
struct Emulated {
    cfg: Config,
    out_dir: PathBuf,
    agent: ureq::Agent,
    rng: StdRng,
    refresh_album: bool,
    rules: (u8, bool),
    room_c: Option<f32>,
    battery_mv: Option<u32>,
    ui: Arc<window::Shared>,
}

impl Emulated {
    fn new(o: &Options, cfg: Config, ui: Arc<window::Shared>) -> Result<Emulated> {
        Ok(Emulated {
            out_dir: o.out_dir.clone(),
            agent: net::agent(),
            rng: o.seed.map_or_else(StdRng::from_entropy, StdRng::seed_from_u64),
            refresh_album: o.refresh,
            rules: (o.opacity.unwrap_or(cfg.overlay_opacity), !o.no_rest),
            room_c: o.room_c,
            battery_mv: o.battery_pct.map(battery_mv_for),
            cfg,
            ui,
        })
    }

    /// One wake with the shared logic; returns the seconds to sleep.
    fn wake(&mut self, state: &mut State, wake: Wake) -> u64 {
        println!();
        let label = self.cfg.label().to_string();
        let rules = app::Config {
            update_hour: self.cfg.update_hour,
            refresh_minutes: self.cfg.refresh_minutes,
            photo_batch: self.cfg.photo_batch,
            opacity: self.rules.0,
            place: &label,
            panel_rest: self.rules.1,
        };
        let readings = Readings { now: self.clock(), battery_mv: self.battery_mv, room_c: self.room_c };
        app::run(self, state, &rules, wake, readings)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.out_dir.join(name)
    }
}

/// Battery voltage that shows as `pct` %.
fn battery_mv_for(pct: u8) -> u32 {
    (3000..=4300).find(|&mv| battery::percent(mv) >= pct).unwrap_or(4300)
}

impl Device for Emulated {
    fn beep(&mut self, tones: &[Tone]) {
        let sound = match tones {
            t if t == beep::PRESS => "chirp (press noticed)",
            t if t == beep::TICK => "pip-pip (progress)",
            t if t == beep::SUCCESS => "beep (done)",
            t if t == beep::ERROR => "beep beep beep (failed)",
            _ => "beep",
        };
        log::info!("sound: {sound}");
        if tones == beep::SUCCESS || tones == beep::ERROR {
            print!("\x07");
            std::io::stdout().flush().ok();
        }
    }

    fn led(&mut self, on: bool) {
        self.ui.set_busy(on);
    }

    fn clock(&self) -> Option<i64> {
        Some(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
    }

    fn show(&mut self, fb: &FrameBuffer) -> Result<()> {
        let png = self.path("screen.png");
        to_rgb_image(fb).save(&png).with_context(|| format!("save {}", png.display()))?;
        log::info!("panel refreshed ({})", png.display());
        self.ui.show(fb.clone());
        Ok(())
    }

    fn load_screen(&mut self, slot: u8) -> Option<FrameBuffer> {
        FrameBuffer::from_bytes(&fs::read(self.path(&format!("screens/{slot}.bin"))).ok()?)
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

    fn go_online(&mut self, now: Option<i64>, batch: u8, ticks: bool) -> Result<Online> {
        let now = now.or_else(|| self.clock()).context("time unknown")?;
        log::info!("online: weather, calendars{}", if batch > 0 { format!(" and {batch} new photos") } else { String::new() });
        let today = tz::to_local(now).0;
        let (agent, cfg) = (&self.agent, &self.cfg);
        let (forecast, events) = std::thread::scope(|s| {
            let weather = s.spawn(|| fetch_weather(agent, cfg).map_err(|e| log::warn!("weather: {e:#}")).ok());
            let events = fetch_calendars(agent, cfg, today);
            (weather.join().ok().flatten(), events)
        });
        let screens = if batch > 0 { self.photo_batch(now as u64, batch)? } else { Vec::new() };
        if ticks {
            self.beep(beep::TICK);
        }
        Ok(Online { now, forecast, events, batch: screens })
    }
}

impl Emulated {
    /// `n` ready-to-show photo screens, downloaded at the same time.
    fn photo_batch(&mut self, now: u64, n: u8) -> Result<Vec<FrameBuffer>> {
        let list_path = self.path("photo_list.txt");
        let mut list = photo_list(&self.agent, &self.cfg, &list_path, now, std::mem::take(&mut self.refresh_album))?;
        let t = Instant::now();
        let seeds: Vec<u64> = (0..n).map(|_| self.rng.gen()).collect();
        let (agent, list_ref, opacity) = (&self.agent, &list, self.rules.0);
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
fn fill_slots(agent: &ureq::Agent, list: &PhotoList, slots: &[Slot], opacity: u8, fb: &mut FrameBuffer) -> Result<(), (usize, anyhow::Error)> {
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

/// Today's and tomorrow's events of all calendars; `None` if none could be read.
fn fetch_calendars(agent: &ureq::Agent, cfg: &Config, today: Date) -> Option<(Vec<DayEvent>, Vec<DayEvent>)> {
    if cfg.calendars.is_empty() {
        log::info!("no calendars in config");
        return None;
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
    if ok == 0 {
        return None;
    }
    calendar::sort_events(&mut all_today);
    calendar::sort_events(&mut all_tomorrow);
    Some((all_today, all_tomorrow))
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
        "{STATE_HEADER}\ntarget {}\nattempt {}\nstock {}\nseen {}\nrefreshed {}\npostponed {}\ncurrent {}\n",
        s.target,
        s.attempt,
        s.stock_count,
        s.seen,
        s.last_refresh.map_or("-".into(), |t| t.to_string()),
        s.postponed.map_or(0, Press::code),
        s.current
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
            "postponed" => s.postponed = Press::from_code(v.parse().ok()?),
            "current" => s.current = v.parse().ok()?,
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
            postponed: Some(Press::Next),
            current: 3,
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
    fn button_names() {
        assert_eq!(parse_press("green"), Some(Press::Update));
        assert_eq!(parse_press("next"), Some(Press::Next));
        assert_eq!(parse_press("previous"), Some(Press::Previous));
        assert_eq!(parse_press("blue"), None);
    }
}
