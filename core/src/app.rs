//! One wake of the device (spec 7.1 / 7.2), shared by the firmware and the
//! PC emulator: which button does what, stored photos, panel rest, offline or
//! Wi-Fi update, retries. Everything hardware-specific goes through `Device`.

use anyhow::{anyhow, Result};
use embedded_graphics::primitives::Rectangle;

use crate::beep::{self, Tone};
use crate::buttons::{self, Press};
use crate::calendar::{self, DayEvent};
use crate::date::Date;
use crate::dither::{dither_area, veil};
use crate::framebuf::FrameBuffer;
use crate::info::Info;
use crate::layout::{draw_events_box, draw_photo_date, draw_weather_box, events_rect, weather_rect, Backdrop, EventsInfo};
use crate::schedule::{self, UpdatePlan, WakePlan};
use crate::weather::Forecast;
use crate::{battery, stock, tz};

/// What woke the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    Timer,
    Button(Press),
    PowerOn,
}

/// Kept between wakes (NVS on the device, a file in the emulator).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// Unix time of the planned timer wake.
    pub target: i64,
    /// Failed updates in a row.
    pub attempt: u8,
    /// Photo screens stored in flash, and which of them were on the panel.
    pub stock_count: u8,
    pub seen: stock::Seen,
    /// Unix time of the last panel refresh.
    pub last_refresh: Option<i64>,
    /// A button press postponed for panel rest (the timer wake carries it out).
    pub postponed: Option<Press>,
    /// Stored photo slot on the screen now (for next/previous browsing).
    pub current: u8,
}

/// Settings the wake logic needs.
#[derive(Debug, Clone)]
pub struct Config<'a> {
    /// Local times `(hour, minute)` of the daily screen updates.
    pub update_times: &'a [(u8, u8)],
    /// Weather/calendar younger than this are reused without Wi-Fi.
    pub refresh_minutes: u32,
    /// Photo screens downloaded per batch.
    pub photo_batch: u8,
    /// Box opacity 0..=100 (100 = solid white boxes).
    pub opacity: u8,
    /// Shown in the events box header when there are no sun times.
    pub place: &'a str,
    /// E-paper care: wait between refreshes (the emulator can turn it off).
    pub panel_rest: bool,
}

/// Sensor readings taken at the start of the wake.
#[derive(Debug, Clone, Copy, Default)]
pub struct Readings {
    /// RTC time; `None` if the RTC lost it.
    pub now: Option<i64>,
    pub battery_mv: Option<u32>,
    pub room_c: Option<f32>,
}

/// What one Wi-Fi session brought.
pub struct Online {
    /// Unix time of the fetch (from NTP if the RTC had lost it).
    pub now: i64,
    pub forecast: Option<Forecast>,
    /// Today's and tomorrow's events of all calendars; `None` if none could be read.
    pub events: Option<(Vec<DayEvent>, Vec<DayEvent>)>,
    /// New ready-to-show photo screens (empty unless asked for).
    pub batch: Vec<FrameBuffer>,
}

/// The hardware: panel, buzzer, LED, flash storage, Wi-Fi.
pub trait Device {
    fn beep(&mut self, tones: &[Tone]);
    fn led(&mut self, on: bool);
    /// System clock, if it was ever set (fallback for a lost RTC).
    fn clock(&self) -> Option<i64>;
    /// Refreshes the panel.
    fn show(&mut self, fb: &FrameBuffer) -> Result<()>;
    fn load_screen(&mut self, slot: u8) -> Option<FrameBuffer>;
    fn save_screen(&mut self, slot: u8, fb: &FrameBuffer) -> Result<()>;
    fn load_info(&mut self) -> Option<Info>;
    fn save_info(&mut self, info: &Info);
    fn save_state(&mut self, state: &State);
    /// Wi-Fi on, fresh weather and calendars, and `batch` new photo screens
    /// (0 = none), Wi-Fi off. `now` is the RTC time if known. With `ticks`,
    /// plays `beep::TICK` at each step (a person is waiting).
    fn go_online(&mut self, now: Option<i64>, batch: u8, ticks: bool) -> Result<Online>;
}

/// After a timer wake: seconds to sleep on if the RTC woke the chip early.
pub fn sleep_early(wake: Wake, now: Option<i64>, state: &State) -> Option<u64> {
    match (wake, schedule::on_timer_wake(now, state.target)) {
        (Wake::Timer, WakePlan::Sleep(secs)) => Some(secs),
        _ => None,
    }
}

/// One wake: does what the wake asks for, saves the state and returns how
/// long to deep-sleep (buttons stay armed).
pub fn run(dev: &mut impl Device, state: &mut State, cfg: &Config, wake: Wake, r: Readings) -> u64 {
    if let Some(secs) = sleep_early(wake, r.now, state) {
        log::info!("woke {secs} s early");
        return secs;
    }
    if matches!(wake, Wake::Button(_)) {
        dev.beep(beep::PRESS); // "I saw the press"
    }
    dev.led(true);
    let secs = run_lit(dev, state, cfg, wake, r);
    dev.led(false);
    secs
}

fn run_lit(dev: &mut impl Device, state: &mut State, cfg: &Config, wake: Wake, r: Readings) -> u64 {
    // which button this update is for: this wake's, or one postponed for panel rest
    let press = match wake {
        Wake::Button(p) => Some(p),
        Wake::Timer => state.postponed,
        Wake::PowerOn => None,
    };
    log::info!("{wake:?}, button: {press:?}");
    // audible progress, only when a person pressed a button (06:00 stays quiet)
    let by_person = schedule::started_by_person(matches!(wake, Wake::Button(_)), wake == Wake::Timer, state.postponed.is_some());
    let any_now = r.now.or_else(|| dev.clock());

    log::info!("battery {:?} mV = {:?} %", r.battery_mv, r.battery_mv.map(battery::percent));
    if r.battery_mv.is_some_and(battery::too_low_to_update) {
        log::warn!("battery too low for a panel refresh");
        dev.beep(beep::ERROR);
        return sleep_until_next(dev, state, r.now, cfg.update_times);
    }

    // if this update hangs or crashes, the watchdog's 30-minute sleep must
    // count as a retry, not as an early wake before the daily update
    let provisional = schedule::after_failure(any_now, state.attempt.saturating_add(1), cfg.update_times);
    state.target = provisional.target;
    dev.save_state(state);

    // e-paper care: rest between refreshes; a quick second press is done
    // automatically when the panel may refresh again
    if let Some(now) = r.now.filter(|_| cfg.panel_rest) {
        // a person pressing a button waits less than the daily update
        let rest = if press.is_some() { schedule::BUTTON_REST_SECS } else { schedule::PANEL_REST_SECS };
        if let Some(wait) = schedule::panel_rest(now, state.last_refresh, rest) {
            log::info!("panel refreshed {} s ago, updating in {wait} s", rest - wait as i64);
            state.target = now + wait as i64;
            // remember which button a person pressed, for the postponed update
            if let Wake::Button(p) = wake {
                state.postponed = Some(p);
            }
            dev.save_state(state);
            return wait;
        }
    }

    state.postponed = None; // carried out now
    let stored_info = dev.load_info();

    // side buttons: browse the stored photos, no Wi-Fi
    if let Some(dir @ (Press::Next | Press::Previous)) = press {
        match browse(dev, state, cfg, dir, any_now, stored_info, r, by_person) {
            Ok(()) => dev.beep(beep::SUCCESS),
            Err(e) => {
                log::warn!("browse: {e:#}");
                dev.beep(beep::ERROR);
            }
        }
        return sleep_until_next(dev, state, any_now, cfg.update_times);
    }

    // green button / timer: a stored photo not seen yet; once all were seen
    // (also by browsing), a new batch. What this wake has to fetch: nothing,
    // weather+calendar, or also photos
    let unseen_slot = stock::next_unseen(state.seen, state.stock_count, state.current);
    let next_photo = unseen_slot.and_then(|slot| dev.load_screen(slot).map(|fb| (slot, fb)));
    let photos_left = if next_photo.is_some() { stock::unseen(state.seen, state.stock_count) } else { 0 };
    let plan = match r.now {
        Some(now) => schedule::plan_update(
            now,
            stored_info.as_ref().map(|i| (i.fetched_at, i.date)),
            photos_left,
            i64::from(cfg.refresh_minutes) * 60,
        ),
        None if photos_left == 0 => UpdatePlan::RefreshAll,
        None => UpdatePlan::RefreshInfo,
    };
    log::info!("plan {plan:?}: {photos_left} of {} stored photos not seen yet", state.stock_count);

    match update(dev, state, cfg, plan, r, stored_info, next_photo, by_person) {
        Ok(now) => {
            dev.beep(beep::SUCCESS);
            state.attempt = 0;
            sleep_until_next(dev, state, Some(now), cfg.update_times)
        }
        Err(e) => {
            log::error!("update failed: {e:#}");
            dev.beep(beep::ERROR);
            state.attempt = state.attempt.saturating_add(1);
            let retry = schedule::after_failure(any_now, state.attempt, cfg.update_times);
            if schedule::retry_after_failure(state.attempt).is_none() {
                state.attempt = 0; // back to the daily schedule
            }
            state.target = retry.target;
            dev.save_state(state);
            retry.secs
        }
    }
}

/// Next/previous stored photo with the stored weather and calendar.
#[allow(clippy::too_many_arguments)]
fn browse(
    dev: &mut impl Device,
    state: &mut State,
    cfg: &Config,
    dir: Press,
    now: Option<i64>,
    stored_info: Option<Info>,
    r: Readings,
    by_person: bool,
) -> Result<()> {
    let now = now.ok_or_else(|| anyhow!("time unknown"))?;
    let slot = buttons::browse(state.current, state.stock_count, dir).ok_or_else(|| anyhow!("no stored photos"))?;
    let photo = dev.load_screen(slot).ok_or_else(|| anyhow!("stored photo {slot} unreadable"))?;
    log::info!("{dir:?}: stored photo {slot} of {}", state.stock_count);
    let info = stored_info.unwrap_or(Info { fetched_at: 0, date: tz::to_local(now).0, forecast: None, events: None, tomorrow: None });
    let fb = compose(cfg, photo, &info, now, r.battery_mv, r.room_c);
    if by_person {
        dev.beep(beep::TICK);
    }
    dev.show(&fb)?;
    state.current = slot;
    state.seen = stock::mark(state.seen, slot);
    state.last_refresh = Some(now);
    Ok(())
}

/// Green button / timer: fetch what the plan says, show a photo; returns the time.
#[allow(clippy::too_many_arguments)]
fn update(
    dev: &mut impl Device,
    state: &mut State,
    cfg: &Config,
    plan: UpdatePlan,
    r: Readings,
    stored_info: Option<Info>,
    next_photo: Option<(u8, FrameBuffer)>,
    by_person: bool,
) -> Result<i64> {
    let (now, info, batch) = match plan {
        UpdatePlan::Offline => {
            let now = r.now.ok_or_else(|| anyhow!("time unknown"))?;
            (now, stored_info.ok_or_else(|| anyhow!("no stored info"))?, None)
        }
        UpdatePlan::RefreshInfo | UpdatePlan::RefreshAll => {
            let all = plan == UpdatePlan::RefreshAll;
            let o = dev.go_online(r.now, if all { cfg.photo_batch } else { 0 }, by_person)?;
            let info = fresh_info(o.now, o.forecast, o.events, stored_info.as_ref());
            dev.save_info(&info);
            (o.now, info, all.then_some(o.batch))
        }
    };

    let photo = match batch {
        Some(batch) => {
            let mut saved = 0u8;
            for (i, fb) in batch.iter().enumerate() {
                match dev.save_screen(i as u8, fb) {
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
            } else if state.stock_count > 0 {
                log::warn!("no new photos, showing the stored ones again");
            }
            state.seen = 0;
            batch.into_iter().next().or_else(|| dev.load_screen(0)).map(|fb| (0, fb))
        }
        None => next_photo,
    };
    let (slot, photo) = photo.ok_or_else(|| anyhow!("no photo to show"))?;
    log::info!("showing stored photo {slot} of {}", state.stock_count);

    let fb = compose(cfg, photo, &info, now, r.battery_mv, r.room_c);
    if by_person {
        dev.beep(beep::TICK);
    }
    dev.show(&fb)?;
    state.current = slot;
    state.seen = stock::mark(state.seen, slot);
    state.last_refresh = Some(now);
    Ok(now)
}

/// Sleep until the next scheduled update (an hour if the time is unknown).
fn sleep_until_next(dev: &mut impl Device, state: &mut State, now: Option<i64>, times: &[(u8, u8)]) -> u64 {
    let Some(now) = now else {
        log::warn!("time unknown, retrying in an hour");
        dev.save_state(state);
        return schedule::when_time_unknown();
    };
    state.target = schedule::next_update(now, times);
    dev.save_state(state);
    let (d, s) = tz::to_local(state.target);
    log::info!("sleeping until {} {:02}:{:02} local", d.label(), s / 3600, s % 3600 / 60);
    (state.target - now).max(1) as u64
}

/// Weather and calendar of a fetch; whatever failed falls back to the stored
/// ones (events only of today and tomorrow).
pub fn fresh_info(
    now: i64,
    forecast: Option<Forecast>,
    events: Option<(Vec<DayEvent>, Vec<DayEvent>)>,
    stored: Option<&Info>,
) -> Info {
    let today = tz::to_local(now).0;
    let (events, tomorrow) = match events {
        Some((a, b)) => (Some(a), Some(b)),
        None => stored.map_or((None, None), |i| i.events_for(today)),
    };
    Info {
        fetched_at: now,
        date: today,
        forecast: forecast.or_else(|| stored.and_then(|i| i.forecast.clone())),
        events,
        tomorrow,
    }
}

/// Puts one downloaded photo (`rgb`, sized to `area`) into a screen: lightened
/// under the boxes when they are see-through, dithered to the panel's inks,
/// with the date it was taken.
pub fn place_photo(fb: &mut FrameBuffer, area: &Rectangle, mut rgb: Vec<u8>, opacity: u8, taken: Option<Date>) -> Result<()> {
    if opacity < 100 {
        for boxed in [events_rect(), weather_rect()] {
            veil(&mut rgb, area, &boxed, opacity);
        }
    }
    dither_area(&rgb, area, fb)?;
    if let Some(taken) = taken {
        draw_photo_date(fb, area, taken);
    }
    Ok(())
}

/// Draws the events and weather boxes over a stored photo screen.
pub fn compose(cfg: &Config, mut fb: FrameBuffer, info: &Info, now: i64, battery_mv: Option<u32>, room_c: Option<f32>) -> FrameBuffer {
    let (today, secs) = tz::to_local(now);
    let now_hm = ((secs / 3600) as u8, (secs % 3600 / 60) as u8);
    let backdrop = if cfg.opacity >= 100 { Backdrop::Solid } else { Backdrop::Clear };
    // sunrise/sunset replace the (static) place name when the weather is known
    let sun = info.forecast.as_ref().and_then(|f| f.sun_on(today)).map(|s| (s.rise, s.set));
    let status = match sun {
        Some(_) => format!("{:02}:{:02}", now_hm.0, now_hm.1),
        None => format!("{} {:02}:{:02}", cfg.place, now_hm.0, now_hm.1),
    };
    // only what is still ahead: today's events not yet over, else tomorrow's
    let (events, tomorrow) = info.events_for(today);
    let agenda = calendar::agenda(events, tomorrow, now_hm);
    let window = info.forecast.as_ref().and_then(|f| f.window(now));
    draw_weather_box(&mut fb, window.as_deref(), backdrop);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuf::Ink;
    use std::collections::HashMap;

    /// A screen marked with its stored slot number.
    fn screen(n: u8) -> FrameBuffer {
        let mut fb = FrameBuffer::new(Ink::White);
        fb.set(100 + u32::from(n), 200, Ink::Black);
        fb
    }

    /// Which marked screen `fb` shows.
    fn marker(fb: &FrameBuffer) -> Option<u8> {
        (0..20).find(|&n| fb.get(100 + u32::from(n), 200) == Ink::Black)
    }

    #[derive(Default)]
    struct Fake {
        screens: HashMap<u8, FrameBuffer>,
        info: Option<Info>,
        shown: Vec<Option<u8>>,
        beeps: Vec<&'static str>,
        online_calls: Vec<u8>,
        offline: bool,
        /// Marker of the first screen of the next batch.
        next_batch: u8,
        saved: Option<State>,
    }

    impl Device for Fake {
        fn beep(&mut self, tones: &[Tone]) {
            let name = [(beep::PRESS, "press"), (beep::TICK, "tick"), (beep::SUCCESS, "ok"), (beep::ERROR, "error")]
                .iter()
                .find(|(t, _)| *t == tones)
                .map_or("?", |(_, n)| n);
            self.beeps.push(name);
        }
        fn led(&mut self, _on: bool) {}
        fn clock(&self) -> Option<i64> {
            None
        }
        fn show(&mut self, fb: &FrameBuffer) -> Result<()> {
            self.shown.push(marker(fb));
            Ok(())
        }
        fn load_screen(&mut self, slot: u8) -> Option<FrameBuffer> {
            self.screens.get(&slot).cloned()
        }
        fn save_screen(&mut self, slot: u8, fb: &FrameBuffer) -> Result<()> {
            self.screens.insert(slot, fb.clone());
            Ok(())
        }
        fn load_info(&mut self) -> Option<Info> {
            self.info.clone()
        }
        fn save_info(&mut self, info: &Info) {
            self.info = Some(info.clone());
        }
        fn save_state(&mut self, state: &State) {
            self.saved = Some(state.clone());
        }
        fn go_online(&mut self, now: Option<i64>, batch: u8, _ticks: bool) -> Result<Online> {
            self.online_calls.push(batch);
            if self.offline {
                anyhow::bail!("no Wi-Fi");
            }
            let first = self.next_batch;
            Ok(Online {
                now: now.unwrap_or(T0),
                forecast: None,
                events: Some((vec![], vec![])),
                batch: (0..batch).map(|i| screen(first + i)).collect(),
            })
        }
    }

    /// 2026-11-25 10:00 Israel time.
    const T0: i64 = 1_795_593_600;

    fn cfg() -> Config<'static> {
        Config { update_times: &[(6, 0)], refresh_minutes: 480, photo_batch: 3, opacity: 50, place: "Home", panel_rest: true }
    }

    fn at(now: i64) -> Readings {
        Readings { now: Some(now), battery_mv: Some(4000), room_c: Some(22.0) }
    }

    /// A device with stored screens 0..n and fresh info.
    fn stocked(n: u8) -> (Fake, State) {
        let mut dev = Fake::default();
        for i in 0..n {
            dev.screens.insert(i, screen(i));
        }
        dev.info = Some(fresh_info(T0 - 60, None, Some((vec![], vec![])), None));
        (dev, State { stock_count: n, ..State::default() })
    }

    #[test]
    fn first_power_on_downloads_a_batch_and_shows_its_first_photo() {
        let mut dev = Fake::default();
        let mut st = State::default();
        let secs = run(&mut dev, &mut st, &cfg(), Wake::PowerOn, at(T0));
        assert_eq!(dev.online_calls, vec![3]);
        assert_eq!(dev.shown, vec![Some(0)]);
        assert_eq!((st.stock_count, st.current, st.seen), (3, 0, 1));
        assert_eq!(dev.beeps, vec!["ok"], "no press chirp or ticks on power-on");
        assert_eq!(st.target, schedule::next_update(T0, &[(6, 0)]));
        assert_eq!(secs as i64, st.target - T0);
        assert_eq!(dev.saved, Some(st));
    }

    #[test]
    fn green_button_shows_the_next_unseen_photo_without_wifi() {
        let (mut dev, mut st) = stocked(3);
        st.seen = stock::mark(0, 0);
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(T0));
        assert!(dev.online_calls.is_empty(), "fresh info and unseen photos: offline");
        assert_eq!(dev.shown, vec![Some(1)]);
        assert_eq!(dev.beeps, vec!["press", "tick", "ok"]);
    }

    #[test]
    fn green_button_after_every_photo_was_seen_downloads_a_new_batch() {
        let (mut dev, mut st) = stocked(3);
        st.seen = (0..3).fold(0, stock::mark);
        dev.next_batch = 10;
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(T0));
        assert_eq!(dev.online_calls, vec![3]);
        assert_eq!(dev.shown, vec![Some(10)]);
        assert_eq!(marker(&dev.screens[&2]), Some(12), "the new batch is stored");
        assert_eq!(st.seen, 1);
    }

    #[test]
    fn old_info_is_fetched_again_but_the_photo_is_a_stored_one() {
        let (mut dev, mut st) = stocked(3);
        let later = T0 + 9 * 3600;
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(later));
        assert_eq!(dev.online_calls, vec![0], "weather + calendar only");
        assert_eq!(dev.shown, vec![Some(1)]);
        assert_eq!(dev.info.as_ref().map(|i| i.fetched_at), Some(later));
    }

    #[test]
    fn side_buttons_browse_and_wrap_around() {
        let (mut dev, mut st) = stocked(3);
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Previous), at(T0));
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Next), at(T0 + 120));
        assert_eq!(dev.shown, vec![Some(2), Some(0)]);
        assert!(dev.online_calls.is_empty());
        assert_eq!(st.seen, stock::mark(stock::mark(0, 2), 0), "browsing counts as seen");
    }

    #[test]
    fn browsing_with_nothing_stored_beeps_an_error() {
        let mut dev = Fake::default();
        let mut st = State::default();
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Next), at(T0));
        assert!(dev.shown.is_empty());
        assert_eq!(dev.beeps, vec!["press", "error"]);
    }

    #[test]
    fn quick_second_press_is_postponed_and_done_by_the_timer() {
        let (mut dev, mut st) = stocked(3);
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Next), at(T0));
        let wait = run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Next), at(T0 + 20));
        assert_eq!(wait, 40, "buttons rest the panel 60 s");
        assert_eq!(dev.shown.len(), 1);
        assert_eq!(st.postponed, Some(Press::Next));
        run(&mut dev, &mut st, &cfg(), Wake::Timer, at(T0 + 60));
        assert_eq!(dev.shown, vec![Some(1), Some(2)]);
        assert_eq!(st.postponed, None);
        assert_eq!(dev.beeps.last(), Some(&"ok"));
        assert!(dev.beeps.contains(&"tick"), "the postponed press is a person's update");
    }

    #[test]
    fn without_panel_rest_every_press_refreshes() {
        let (mut dev, mut st) = stocked(3);
        let cfg = Config { panel_rest: false, ..cfg() };
        run(&mut dev, &mut st, &cfg, Wake::Button(Press::Next), at(T0));
        run(&mut dev, &mut st, &cfg, Wake::Button(Press::Next), at(T0 + 1));
        assert_eq!(dev.shown, vec![Some(1), Some(2)]);
    }

    #[test]
    fn early_timer_wake_sleeps_on() {
        let (mut dev, mut st) = stocked(3);
        st.target = T0 + 600;
        assert_eq!(run(&mut dev, &mut st, &cfg(), Wake::Timer, at(T0)), 600);
        assert!(dev.shown.is_empty() && dev.beeps.is_empty());
    }

    #[test]
    fn failed_update_retries_in_30_minutes() {
        let mut dev = Fake { offline: true, ..Fake::default() };
        let mut st = State::default();
        let secs = run(&mut dev, &mut st, &cfg(), Wake::PowerOn, at(T0));
        assert_eq!(secs, 1800);
        assert_eq!((st.attempt, st.target), (1, T0 + 1800));
        assert_eq!(dev.beeps, vec!["error"]);
    }

    #[test]
    fn low_battery_skips_the_refresh() {
        let (mut dev, mut st) = stocked(3);
        let r = Readings { battery_mv: Some(3300), ..at(T0) };
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), r);
        assert!(dev.shown.is_empty());
        assert_eq!(dev.beeps, vec!["press", "error"]);
    }

    #[test]
    fn failed_fetches_keep_the_stored_weather_and_same_day_events() {
        let ev = vec![DayEvent { start: Some((9, 0)), end: None, title: "Swim".into() }];
        let stored = fresh_info(T0 - 3600, None, Some((ev.clone(), vec![])), None);
        let i = fresh_info(T0, None, None, Some(&stored));
        assert_eq!((i.fetched_at, i.events), (T0, Some(ev)));
        let next_day = fresh_info(T0 + 86_400, None, None, Some(&stored));
        assert_eq!(next_day.events, Some(vec![]), "yesterday's events are not today's: its tomorrow is");
        assert_eq!(next_day.tomorrow, None);
        let later = fresh_info(T0 + 2 * 86_400, None, None, Some(&stored));
        assert_eq!(later.events, None, "nothing known about this day");
    }
}
