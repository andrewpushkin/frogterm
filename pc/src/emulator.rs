//! Emulates one wake of the device on the PC and saves the screen as a PNG.
//!
//! Same steps as the firmware (design 7.1): photo list (cached for a week) →
//! random photo (two side by side if portrait), downloaded already sized → dither to 6 inks → weather →
//! weather box → "panel refresh" (here: out/screen.png) → beep.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context, Result};
use embedded_graphics::pixelcolor::RgbColor;
use frogterm_core::dither::{dither_area, nearest_ink, veil};
use frogterm_core::framebuf::{FrameBuffer, Ink, HEIGHT, WIDTH};
use frogterm_core::calendar::{self, DayEvent, IcsScanner};
use frogterm_core::date::Date;
use frogterm_core::tz;
use frogterm_core::layout::{draw_events_box, draw_weather_box, events_rect, weather_rect, Backdrop, EventsInfo};
use frogterm_core::screen::{self, Slot};
use frogterm_core::weather::{self, Forecast};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::config::Config;
use crate::net;
use frogterm_core::photo_list::PhotoList;

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
}

fn usage() -> ! {
    eprintln!(
        "usage: frogterm-pc emulator [--config FILE] [--out DIR] [--refresh]\n\
         \n\
         --config FILE  album link and city (default: config.toml)\n\
         --out DIR      where screen.png and caches go (default: out)\n\
         --refresh      re-read the whole album even if the cached list is fresh\n\
         --opacity N    box opacity 0..100 (overrides overlay_opacity)\n\
         --seed N       fixed random seed (same photos every run)\n\
         --room C       pretend room temperature (the PC has no sensor)\n\
         --battery P    pretend battery percentage"
    );
    std::process::exit(2);
}

pub fn main(args: &[String]) -> Result<()> {
    let mut o = Options { config: "config.toml".into(), out_dir: "out".into(), refresh: false, opacity: None, seed: None, room_c: None, battery_pct: None };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => o.config = it.next().unwrap_or_else(|| usage()).into(),
            "--out" => o.out_dir = it.next().unwrap_or_else(|| usage()).into(),
            "--refresh" => o.refresh = true,
            "--opacity" => o.opacity = Some(it.next().and_then(|v| v.parse().ok()).filter(|&v| v <= 100).unwrap_or_else(|| usage())),
            "--room" => o.room_c = Some(it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage())),
            "--battery" => o.battery_pct = Some(it.next().and_then(|v| v.parse().ok()).filter(|&v| v <= 100).unwrap_or_else(|| usage())),
            "--seed" => o.seed = Some(it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage())),
            _ => usage(),
        }
    }
    run(&o)
}

fn run(o: &Options) -> Result<()> {
    let cfg = Config::load(&o.config)?;
    let opacity = o.opacity.unwrap_or(cfg.overlay_opacity);
    let backdrop = if opacity >= 100 { Backdrop::Solid } else { Backdrop::Clear };
    let mut rng: StdRng = match o.seed {
        Some(s) => StdRng::seed_from_u64(s),
        None => StdRng::from_entropy(),
    };
    fs::create_dir_all(&o.out_dir).with_context(|| format!("create {}", o.out_dir.display()))?;
    let agent = net::agent();
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    println!("frogterm emulator: one device update (config {})", o.config.display());

    println!("[1/5] Photo list");
    let list_path = o.out_dir.join("photo_list.txt");
    let mut list = photo_list(&agent, &cfg, &list_path, now, o.refresh)?;

    println!("[2/5] Random photo(s), downloaded already sized by Google, dithered to 6 colours");
    let last_photo = o.out_dir.join("last_photo.png");
    let mut fb = match random_screen(&agent, &mut list, &list_path, now, opacity, &mut rng) {
        Ok(fb) => {
            to_rgb_image(&fb).save(&last_photo).with_context(|| format!("save {}", last_photo.display()))?;
            fb
        }
        Err(e) => {
            println!("      {e:#}");
            ensure!(last_photo.exists(), "no photo could be downloaded and there is no cached last photo");
            println!("      using the last good photo instead ({})", last_photo.display());
            load_screen(&last_photo)?
        }
    };

    println!("[3/5] Weather");
    let forecast = match fetch_weather(&agent, &cfg) {
        Ok(f) => Some(f),
        Err(e) => {
            println!("      unavailable: {e:#}");
            None
        }
    };

    // Israel local time, as the device will compute it from its RTC.
    let (today, secs) = tz::to_local(now as i64);
    let now_hm = ((secs / 3600) as u8, (secs % 3600 / 60) as u8);
    println!("[4/5] Calendar ({})", today.label());
    let events = fetch_calendars(&agent, &cfg, today, now_hm);

    let status = format!("{} {:02}:{:02}", cfg.label(), now_hm.0, now_hm.1);
    draw_weather_box(&mut fb, forecast.as_ref().and_then(|f| f.window(now as i64)), backdrop);
    draw_events_box(&mut fb, &EventsInfo {
            date: &today.label(),
            status: &status,
            room_c: o.room_c,
            battery_pct: o.battery_pct,
            events: events.events.as_deref(),
            tomorrow: events.tomorrow,
        }, backdrop);
    let png = o.out_dir.join("screen.png");
    to_rgb_image(&fb).save(&png).with_context(|| format!("save {}", png.display()))?;
    println!("[5/5] Screen saved to {}", png.display());
    print!("\x07");
    println!("BEEP - update finished");
    Ok(())
}

/// Cached list if fresh, otherwise read the whole album and cache it.
fn photo_list(agent: &ureq::Agent, cfg: &Config, path: &Path, now: u64, refresh: bool) -> Result<PhotoList> {
    let cached = fs::read_to_string(path).ok().and_then(|t| PhotoList::from_text(&t));
    if let Some(l) = cached.filter(|l| !refresh && l.is_fresh_for(&cfg.album_url, now)) {
        println!("      cached: {} photos, {:.1} days old (re-read after 7 days)", l.photos.len(), l.age_days(now));
        return Ok(l);
    }
    print!("      reading the whole album ");
    std::io::stdout().flush().ok();
    let t = Instant::now();
    let photos = net::fetch_album(agent, &cfg.album_url, MAX_PAGES, |_| {
        print!(".");
        std::io::stdout().flush().ok();
    })?;
    println!(" {} photos, {:.1} s", photos.len(), t.elapsed().as_secs_f32());
    ensure!(!photos.is_empty(), "the album has no photos");
    let list = PhotoList {
        album_url: cfg.album_url.clone(),
        fetched_at: now,
        photos,
    };
    fs::write(path, list.to_text()).with_context(|| format!("save {}", path.display()))?;
    Ok(list)
}

/// Up to `ATTEMPTS` tries: pick a random photo (plus a tall partner if it is tall),
/// download each already sized for its slot and dither it into place.
fn random_screen(
    agent: &ureq::Agent,
    list: &mut PhotoList,
    list_path: &Path,
    now: u64,
    opacity: u8,
    rng: &mut StdRng,
) -> Result<FrameBuffer> {
    for _ in 0..ATTEMPTS {
        let first = rng.gen_range(0..list.photos.len());
        let slots = screen::plan(&list.photos, first, screen::FRESH, |n| rng.gen_range(0..n));
        let mut fb = FrameBuffer::new(Ink::White);
        match fill_slots(agent, list, &slots, opacity, &mut fb) {
            Ok(()) => return Ok(fb),
            Err((i, e)) if net::is_status(&e, &[403, 404, 410]) => {
                // Cached photo links expired: worth knowing how old the list was.
                println!(
                    "      photo #{i}: link no longer works ({e:#}); the list is {:.1} days old, \
                     it will be re-read on the next run",
                    list.age_days(now)
                );
                list.fetched_at = 0;
                fs::write(list_path, list.to_text())?;
            }
            Err((i, e)) => println!("      photo #{i}: {e:#}"),
        }
    }
    bail!("{ATTEMPTS} tries in a row failed")
}

/// Downloads and dithers every slot; on failure returns the photo index and the error.
fn fill_slots(
    agent: &ureq::Agent,
    list: &PhotoList,
    slots: &[Slot],
    opacity: u8,
    fb: &mut FrameBuffer,
) -> std::result::Result<(), (usize, anyhow::Error)> {
    for s in slots {
        let p = &list.photos[s.photo];
        let (w, h) = (s.area.size.width, s.area.size.height);
        let t = Instant::now();
        let jpeg = net::download_photo(agent, p, w, h).map_err(|e| (s.photo, e))?;
        let mut rgb = decode(&jpeg.bytes, w, h).map_err(|e| (s.photo, e))?;
        if opacity < 100 {
            // the boxes will be transparent: lighten the photo under them first
            for boxed in [events_rect(), weather_rect()] {
                veil(&mut rgb, &s.area, &boxed, opacity);
            }
        }
        dither_area(&rgb, &s.area, fb).map_err(|e| (s.photo, e))?;
        println!(
            "      photo #{} of {} ({}x{} {}): {} KB {} JPEG as {w}x{h} at x={}, {:.1} s",
            s.photo,
            list.photos.len(),
            p.width,
            p.height,
            if p.is_tall() { "tall" } else { "wide" },
            jpeg.bytes.len() / 1024,
            if jpeg.progressive { "progressive" } else { "baseline" },
            s.area.top_left.x,
            t.elapsed().as_secs_f32()
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
    let img = if (img.width(), img.height()) == (w, h) {
        img
    } else {
        img.resize_to_fill(w, h, image::imageops::FilterType::Triangle)
    };
    Ok(img.to_rgb8().into_raw())
}

/// Reads a saved screen PNG back into panel inks.
fn load_screen(path: &Path) -> Result<FrameBuffer> {
    let img = image::open(path).with_context(|| format!("read {}", path.display()))?.to_rgb8();
    ensure!(img.dimensions() == (WIDTH, HEIGHT), "{} is not {WIDTH}x{HEIGHT}", path.display());
    let mut fb = FrameBuffer::new(Ink::White);
    for (x, y, px) in img.enumerate_pixels() {
        let [r, g, b] = px.0;
        fb.set(x, y, nearest_ink(r.into(), g.into(), b.into()));
    }
    Ok(fb)
}

/// What the events box shows: today's remaining events, or tomorrow's if
/// none are left; `events: None` if no calendar could be read.
fn fetch_calendars(agent: &ureq::Agent, cfg: &Config, today: Date, now: (u8, u8)) -> calendar::Agenda {
    if cfg.calendars.is_empty() {
        println!("      no calendars in config");
        return calendar::Agenda { events: None, tomorrow: false };
    }
    let (mut all_today, mut all_tomorrow) = (Vec::new(), Vec::new());
    let mut ok = 0;
    for (i, url) in cfg.calendars.iter().enumerate() {
        let t = Instant::now();
        match read_calendar(agent, url, today) {
            Ok((today_ev, tomorrow_ev, bytes)) => {
                ok += 1;
                println!(
                    "      calendar {}: {} events today, {} tomorrow, {} KB, {:.1} s",
                    i + 1,
                    today_ev.len(),
                    tomorrow_ev.len(),
                    bytes / 1024,
                    t.elapsed().as_secs_f32()
                );
                all_today.extend(today_ev);
                all_tomorrow.extend(tomorrow_ev);
            }
            Err(e) => println!("      calendar {}: {e:#}", i + 1),
        }
    }
    if ok == 0 {
        return calendar::Agenda { events: None, tomorrow: false };
    }
    calendar::sort_events(&mut all_today);
    calendar::sort_events(&mut all_tomorrow);
    let agenda = calendar::agenda(Some(all_today), Some(all_tomorrow), now);
    if agenda.tomorrow {
        println!("      nothing left today, showing tomorrow:");
    }
    for e in agenda.events.iter().flatten() {
        let time = e.start.map_or("all day".to_string(), |(h, m)| format!("{h:02}:{m:02}"));
        println!("      {time:>7}  {}", e.title);
    }
    agenda
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
        Some(c) => {
            println!("      place: {} at {:.4}, {:.4} (from config)", cfg.label(), c.0, c.1);
            c
        }
        None => {
            let place = weather::parse_geocode(&net::get_text(agent, &weather::geocode_url(&cfg.city))?)
                .with_context(|| format!("looking up city {:?}", cfg.city))?;
            println!("      place: {} at {:.4}, {:.4} (looked up)", place.name, place.latitude, place.longitude);
            (place.latitude, place.longitude)
        }
    };
    let f = weather::parse_forecast(&net::get_text(agent, &weather::forecast_url(lat, lon))?)?;
    let line: Vec<String> = f
        .slots
        .iter()
        .take(weather::SLOTS)
        .map(|s| format!("{:02}h {:.0}° {}% {:?}", s.hour, s.temp_c, s.precip_pct, s.icon))
        .collect();
    println!("      {}", line.join(" | "));
    Ok(f)
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
}
