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
use crate::layout::{
    draw_events_box, draw_photo_counter, draw_photo_date, draw_weather_box, draw_weather_tab, events_rect, weather_rect, Backdrop, EventsInfo,
};
use crate::schedule::{self, UpdatePlan, WakePlan};
use crate::weather::Forecast;
use crate::{battery, stock, tz};

/// The status LED.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Led {
    /// Ready: a press is done right away.
    Off,
    /// Busy (working, or the panel rests after a refresh): a press is ignored.
    On,
}

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
    /// Stored photo slot on the screen now (for next/previous browsing).
    pub current: u8,
    /// The last sync failed, completely or in part: the screen shows a
    /// crossed-out Wi-Fi icon until a sync gets everything.
    pub sync_failed: bool,
    /// The final beep, played when the device is ready again (the panel's
    /// rest is over): `Some(true)` success, `Some(false)` error.
    pub ready_beep: Option<bool>,
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
    /// Parts that did not come although others did (e.g. one calendar of
    /// two), for the device to report what `forecast`/`events` cannot show.
    pub missing: Vec<&'static str>,
}

/// The hardware: panel, buzzer, LED, flash storage, Wi-Fi.
pub trait Device {
    fn beep(&mut self, tones: &[Tone]);
    /// Sets the LED; it keeps that state through deep sleep.
    fn led(&mut self, led: Led);
    /// System clock, if it was ever set (fallback for a lost RTC).
    fn clock(&self) -> Option<i64>;
    /// Refreshes the panel (takes the screen over: no copy needed to keep it).
    fn show(&mut self, fb: FrameBuffer) -> Result<()>;
    fn load_screen(&mut self, slot: u8) -> Option<FrameBuffer>;
    fn save_screen(&mut self, slot: u8, fb: &FrameBuffer) -> Result<()>;
    fn load_info(&mut self) -> Option<Info>;
    fn save_info(&mut self, info: &Info);
    fn save_state(&mut self, state: &State);
    /// Wi-Fi on, fresh weather and calendars, and `batch` new photo screens
    /// (0 = none), Wi-Fi off. `now` is the RTC time if known.
    fn go_online(&mut self, now: Option<i64>, batch: u8) -> Result<Online>;
}

/// After a timer wake: seconds to sleep on if the RTC woke the chip early.
pub fn sleep_early(wake: Wake, now: Option<i64>, state: &State) -> Option<u64> {
    match (wake, schedule::on_timer_wake(now, state.target)) {
        (Wake::Timer, WakePlan::Sleep(secs)) => Some(secs),
        _ => None,
    }
}

/// A timer wake before the planned update (e.g. the one at the end of the
/// panel's rest): back to sleep with this LED, after this beep, if any.
pub struct EarlyWake {
    pub secs: u64,
    pub led: Led,
    pub beep: Option<&'static [Tone]>,
}

/// For a timer wake before the planned update; once the panel's rest is over
/// the device is ready: LED off and the pending final beep (taken from
/// `state`, which then needs saving).
pub fn early_wake(wake: Wake, now: Option<i64>, state: &mut State) -> Option<EarlyWake> {
    let secs = sleep_early(wake, now, state)?;
    let (led, secs) = led_while_asleep(now, state, secs);
    let beep = if led == Led::Off { state.ready_beep.take().map(final_beep) } else { None };
    Some(EarlyWake { secs, led, beep })
}

fn final_beep(ok: bool) -> &'static [Tone] {
    if ok {
        beep::SUCCESS
    } else {
        beep::ERROR
    }
}

/// How the work of a wake ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Done,
    Failed,
    /// Nothing done yet: the panel still rests, the update comes after it.
    Postponed,
}

/// One wake: does what the wake asks for, saves the state and returns how
/// long to deep-sleep (buttons stay armed). The LED is on until the device is
/// ready for a press again, which the final beep announces.
pub fn run(dev: &mut impl Device, state: &mut State, cfg: &Config, wake: Wake, r: Readings) -> u64 {
    if let Some(e) = early_wake(wake, r.now, state) {
        log::info!("woke {} s early", sleep_early(wake, r.now, state).unwrap_or(e.secs));
        if let Some(tones) = e.beep {
            dev.beep(tones);
            dev.save_state(state);
        }
        dev.led(e.led);
        return e.secs;
    }
    // a press while the panel still rests: busy, the press is ignored
    if let (Wake::Button(p), Some(left)) = (wake, r.now.filter(|_| cfg.panel_rest).and_then(|now| resting(now, state))) {
        log::info!("{p:?} pressed while the panel rests ({left} s left): ignored");
        dev.beep(beep::BUSY);
        dev.led(Led::On);
        return left;
    }
    if matches!(wake, Wake::Button(_)) {
        dev.beep(beep::PRESS); // "I saw the press"
    }
    dev.led(Led::On);
    let (secs, now, outcome) = run_lit(dev, state, cfg, wake, r);
    match outcome {
        Outcome::Done => state.ready_beep = Some(true),
        Outcome::Failed => state.ready_beep = Some(false),
        Outcome::Postponed => {}
    }
    let rest_left = dev.clock().or(now).filter(|_| cfg.panel_rest).and_then(|now| resting(now, state));
    let secs = match rest_left {
        Some(left) => {
            log::info!("busy while the panel rests: LED on, ready in {left} s");
            secs.min(left)
        }
        None => {
            if let Some(ok) = state.ready_beep.take() {
                dev.beep(final_beep(ok));
            }
            dev.led(Led::Off);
            secs
        }
    };
    dev.save_state(state);
    secs
}

/// Seconds the panel still rests after its last refresh, if it does.
fn resting(now: i64, state: &State) -> Option<u64> {
    schedule::panel_rest(now, state.last_refresh, schedule::PANEL_REST_SECS)
}

/// The LED for the coming deep sleep and how long to sleep: on while the
/// panel rests after its last refresh (with a wake when the rest ends, to
/// switch it off), else off.
pub fn led_while_asleep(now: Option<i64>, state: &State, secs: u64) -> (Led, u64) {
    match now.and_then(|now| resting(now, state)) {
        Some(left) => (Led::On, secs.min(left)),
        None => (Led::Off, secs),
    }
}

/// Returns the seconds to sleep and the time as known at the end.
fn run_lit(dev: &mut impl Device, state: &mut State, cfg: &Config, wake: Wake, r: Readings) -> (u64, Option<i64>, Outcome) {
    let press = match wake {
        Wake::Button(p) => Some(p),
        Wake::Timer | Wake::PowerOn => None,
    };
    log::info!("{wake:?}, button: {press:?}");
    let any_now = r.now.or_else(|| dev.clock());

    log::info!("battery {:?} mV = {:?} %", r.battery_mv, r.battery_mv.map(battery::percent));
    if r.battery_mv.is_some_and(battery::too_low_to_update) {
        log::warn!("battery too low for a panel refresh");
        return (sleep_until_next(dev, state, r.now, cfg.update_times), r.now, Outcome::Failed);
    }

    // if this update hangs or crashes, the watchdog's 30-minute sleep must
    // count as a retry, not as an early wake before the daily update
    let provisional = schedule::after_failure(any_now, state.attempt.saturating_add(1), cfg.update_times);
    state.target = provisional.target;
    dev.save_state(state);

    // e-paper care: rest between refreshes (presses during it never get here)
    if let Some(now) = r.now.filter(|_| cfg.panel_rest) {
        if let Some(wait) = resting(now, state) {
            log::info!("panel refreshed {} s ago, updating in {wait} s", schedule::PANEL_REST_SECS - wait as i64);
            state.target = now + wait as i64;
            dev.save_state(state);
            return (wait, Some(now), Outcome::Postponed);
        }
    }

    let stored_info = dev.load_info();

    // side buttons: browse the stored photos, no Wi-Fi
    if let Some(dir @ (Press::Next | Press::Previous)) = press {
        let outcome = match browse(dev, state, cfg, dir, any_now, stored_info, r) {
            Ok(()) => Outcome::Done,
            Err(e) => {
                log::warn!("browse: {e:#}");
                Outcome::Failed
            }
        };
        return (sleep_until_next(dev, state, any_now, cfg.update_times), any_now, outcome);
    }

    // green button: always a full sync (new batch, weather, calendar).
    // Timer: a stored photo not seen yet; once all were seen (also by
    // browsing), a new batch. What this wake has to fetch: nothing,
    // weather+calendar, or also photos
    let unseen_slot = stock::next_unseen(state.seen, state.stock_count, state.current);
    let photos_left = if unseen_slot.is_some() { stock::unseen(state.seen, state.stock_count) } else { 0 };
    let mut plan = match r.now {
        // the green button always syncs: new photos, weather and calendar
        _ if press == Some(Press::Update) => UpdatePlan::RefreshAll,
        Some(now) => schedule::plan_update(
            now,
            stored_info.as_ref().map(|i| (i.fetched_at, i.date)),
            photos_left,
            i64::from(cfg.refresh_minutes) * 60,
        ),
        None if photos_left == 0 => UpdatePlan::RefreshAll,
        None => UpdatePlan::RefreshInfo,
    };
    // the stored photo (192 KB from flash) only when it will be shown;
    // unreadable means a new batch
    let next_photo = match (plan, unseen_slot) {
        (UpdatePlan::RefreshAll, _) | (_, None) => None,
        (_, Some(slot)) => dev.load_screen(slot).map(|fb| (slot, fb)),
    };
    if next_photo.is_none() {
        plan = UpdatePlan::RefreshAll;
    }
    log::info!("plan {plan:?}: {photos_left} of {} stored photos not seen yet", state.stock_count);

    match update(dev, state, cfg, plan, r, stored_info, next_photo) {
        Ok(now) => {
            state.attempt = 0;
            (sleep_until_next(dev, state, Some(now), cfg.update_times), Some(now), Outcome::Done)
        }
        Err(e) => {
            log::error!("update failed: {e:#}");
            state.sync_failed = true;
            show_failed_sync(dev, state, cfg, any_now.or_else(|| dev.clock()), r);
            state.attempt = state.attempt.saturating_add(1);
            let retry = schedule::after_failure(any_now, state.attempt, cfg.update_times);
            if schedule::retry_after_failure(state.attempt).is_none() {
                state.attempt = 0; // back to the daily schedule
            }
            state.target = retry.target;
            dev.save_state(state);
            (retry.secs, any_now, Outcome::Failed)
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
) -> Result<()> {
    let now = now.ok_or_else(|| anyhow!("time unknown"))?;
    let slot = buttons::browse(state.current, state.stock_count, dir).ok_or_else(|| anyhow!("no stored photos"))?;
    let photo = dev.load_screen(slot).ok_or_else(|| anyhow!("stored photo {slot} unreadable"))?;
    log::info!("{dir:?}: stored photo {} of {}", slot + 1, state.stock_count);
    let info = stored_info.unwrap_or(Info { fetched_at: 0, date: tz::to_local(now).0, forecast: None, events: None, tomorrow: None });
    let fb = compose(cfg, photo, &info, now, r, (slot + 1, state.stock_count), state.sync_failed);
    dev.show(fb)?;
    state.current = slot;
    state.seen = stock::mark(state.seen, slot);
    state.last_refresh = Some(now);
    Ok(())
}

/// After a failed update: the photo already on the panel again, with the
/// stored weather and events and the crossed-out Wi-Fi icon, so the failure
/// is visible (nothing is drawn if there is no stored photo or no time).
fn show_failed_sync(dev: &mut impl Device, state: &mut State, cfg: &Config, now: Option<i64>, r: Readings) {
    let Some(now) = now else { return };
    let Some(photo) = dev.load_screen(state.current) else { return };
    let info = dev.load_info().unwrap_or(Info { fetched_at: 0, date: tz::to_local(now).0, forecast: None, events: None, tomorrow: None });
    let fb = compose(cfg, photo, &info, now, r, (state.current + 1, state.stock_count), true);
    match dev.show(fb) {
        Ok(()) => state.last_refresh = Some(now),
        Err(e) => log::warn!("could not show the failed sync: {e:#}"),
    }
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
) -> Result<i64> {
    let (now, info, batch) = match plan {
        UpdatePlan::Offline => {
            let now = r.now.ok_or_else(|| anyhow!("time unknown"))?;
            (now, stored_info.ok_or_else(|| anyhow!("no stored info"))?, None)
        }
        UpdatePlan::RefreshInfo | UpdatePlan::RefreshAll => {
            let all = plan == UpdatePlan::RefreshAll;
            let o = dev.go_online(r.now, if all { cfg.photo_batch } else { 0 })?;
            let mut missing = o.missing;
            if o.forecast.is_none() {
                missing.push("weather");
            }
            if o.events.is_none() {
                missing.push("calendars");
            }
            if all && o.batch.len() < usize::from(cfg.photo_batch) {
                missing.push("photos");
            }
            if !missing.is_empty() {
                log::warn!("sync incomplete, missing: {}", missing.join(", "));
            }
            state.sync_failed = !missing.is_empty();
            let info = fresh_info(o.now, o.forecast, o.events, stored_info);
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
    log::info!("showing stored photo {} of {}", slot + 1, state.stock_count);

    let fb = compose(cfg, photo, &info, now, r, (slot + 1, state.stock_count), state.sync_failed);
    dev.show(fb)?;
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
pub fn fresh_info(now: i64, forecast: Option<Forecast>, events: Option<(Vec<DayEvent>, Vec<DayEvent>)>, stored: Option<Info>) -> Info {
    let today = tz::to_local(now).0;
    let (stored_forecast, stored_events) = match stored {
        Some(i) => {
            let days = today.days_since_epoch() - i.date.days_since_epoch();
            (
                i.forecast,
                match days {
                    0 => (i.events, i.tomorrow),
                    1 => (i.tomorrow, None),
                    _ => (None, None),
                },
            )
        }
        None => (None, (None, None)),
    };
    let (events, tomorrow) = match events {
        Some((a, b)) => (Some(a), Some(b)),
        None => stored_events,
    };
    Info { fetched_at: now, date: today, forecast: forecast.or(stored_forecast), events, tomorrow }
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

/// Draws the events and weather boxes and the photo counter (`n` of `total`
/// stored photos) over a stored photo screen; `sync_failed` adds the
/// crossed-out Wi-Fi icon.
pub fn compose(
    cfg: &Config,
    mut fb: FrameBuffer,
    info: &Info,
    now: i64,
    r: Readings,
    (n, total): (u8, u8),
    sync_failed: bool,
) -> FrameBuffer {
    draw_photo_counter(&mut fb, n, total);
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
    draw_weather_box(&mut fb, info.forecast.as_ref().and_then(|f| f.window(now)), backdrop);
    // humidity and wind of the third column (+4 h), in a tab on the box
    if let Some(ahead) = info.forecast.as_ref().and_then(|f| f.window(now)).and_then(|mut w| w.nth(2)) {
        draw_weather_tab(&mut fb, ahead);
    }
    draw_events_box(
        &mut fb,
        &EventsInfo {
            date: &today.label(),
            status: &status,
            room_c: r.room_c,
            battery_pct: r.battery_mv.map(battery::percent),
            events: agenda.events.as_deref(),
            tomorrow: agenda.tomorrow,
            sun,
            sync_failed,
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
        /// Per refresh: was the crossed-out Wi-Fi icon on it?
        no_wifi: Vec<bool>,
        beeps: Vec<&'static str>,
        online_calls: Vec<u8>,
        offline: bool,
        /// Online, but the weather did not come.
        weather_down: bool,
        /// Online, but this many photos of the batch did not come.
        photos_lost: u8,
        /// Online, but the device reports these parts missing.
        missing: Vec<&'static str>,
        /// Marker of the first screen of the next batch.
        next_batch: u8,
        saved: Option<State>,
        /// LED changes.
        leds: Vec<Led>,
    }

    impl Device for Fake {
        fn beep(&mut self, tones: &[Tone]) {
            let name = [(beep::PRESS, "press"), (beep::BUSY, "busy"), (beep::SUCCESS, "ok"), (beep::ERROR, "error")]
                .iter()
                .find(|(t, _)| *t == tones)
                .map_or("?", |(_, n)| n);
            self.beeps.push(name);
        }
        fn led(&mut self, led: Led) {
            if self.leds.last() != Some(&led) {
                self.leds.push(led);
            }
        }
        fn clock(&self) -> Option<i64> {
            None
        }
        fn show(&mut self, fb: FrameBuffer) -> Result<()> {
            self.shown.push(marker(&fb));
            self.no_wifi.push(no_wifi_icon(&fb));
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
        fn go_online(&mut self, now: Option<i64>, batch: u8) -> Result<Online> {
            self.online_calls.push(batch);
            if self.offline {
                anyhow::bail!("no Wi-Fi");
            }
            let first = self.next_batch;
            Ok(Online {
                now: now.unwrap_or(T0),
                forecast: (!self.weather_down).then(forecast),
                events: Some((vec![], vec![])),
                batch: (0..batch.saturating_sub(self.photos_lost)).map(|i| screen(first + i)).collect(),
                missing: self.missing.clone(),
            })
        }
    }

    /// A forecast without hours or sun times (nothing red on the screen).
    fn forecast() -> Forecast {
        Forecast { now: "10:00".into(), utc_offset_secs: 7200, start_unix: T0, slots: vec![], sun: vec![] }
    }

    /// The crossed-out Wi-Fi icon is the only red in the events box header here.
    fn no_wifi_icon(fb: &FrameBuffer) -> bool {
        let r = events_rect();
        let (x0, y0) = (r.top_left.x as u32, r.top_left.y as u32);
        (y0..y0 + 22).any(|y| (x0..x0 + r.size.width).any(|x| fb.get(x, y) == Ink::Red))
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
        assert!(dev.beeps.is_empty(), "no press chirp on power-on; the final beep waits for the panel's rest");
        assert_eq!(st.ready_beep, Some(true));
        assert_eq!(st.target, schedule::next_update(T0, &[(6, 0)]));
        assert_eq!(secs as i64, schedule::PANEL_REST_SECS, "a short sleep: the LED goes off when the rest ends");
        assert_eq!(dev.saved, Some(st));
    }

    #[test]
    fn green_button_always_syncs_photos_weather_and_calendar() {
        let (mut dev, mut st) = stocked(3);
        st.seen = stock::mark(0, 0);
        dev.next_batch = 10;
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(T0));
        assert_eq!(dev.online_calls, vec![3], "fresh info and unseen photos, still a full sync");
        assert_eq!(dev.shown, vec![Some(10)], "the first photo of the new batch");
        assert_eq!(dev.info.as_ref().map(|i| i.fetched_at), Some(T0));
        assert_eq!(dev.beeps, vec!["press"], "the update itself is silent, the final beep comes when the rest ends");
    }

    #[test]
    fn scheduled_update_with_fresh_info_needs_no_wifi() {
        let (mut dev, mut st) = stocked(3);
        st.seen = stock::mark(0, 0);
        run(&mut dev, &mut st, &cfg(), Wake::Timer, at(T0));
        assert!(dev.online_calls.is_empty(), "fresh info and unseen photos: offline");
        assert_eq!(dev.shown, vec![Some(1)]);
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
        run(&mut dev, &mut st, &cfg(), Wake::Timer, at(later));
        assert_eq!(dev.online_calls, vec![0], "weather + calendar only");
        assert_eq!(dev.shown, vec![Some(1)]);
        assert_eq!(dev.info.as_ref().map(|i| i.fetched_at), Some(later));
    }

    #[test]
    fn side_buttons_browse_and_wrap_around() {
        let (mut dev, mut st) = stocked(3);
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Previous), at(T0));
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Next), at(T0 + schedule::PANEL_REST_SECS));
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
    fn a_press_while_the_panel_rests_is_ignored_with_a_triple_beep() {
        let (mut dev, mut st) = stocked(3);
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Next), at(T0));
        let target = st.target;
        dev.beeps.clear();
        dev.leds.clear();
        let secs = run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Next), at(T0 + 20));
        assert_eq!(dev.beeps, vec!["busy"]);
        assert_eq!(dev.shown.len(), 1, "no refresh");
        assert_eq!(dev.leds, vec![Led::On], "still busy");
        assert_eq!(secs, 160, "back to sleep until the rest ends");
        assert_eq!((st.current, st.target, st.last_refresh), (1, target, Some(T0)), "nothing changed");
    }

    #[test]
    fn the_final_beep_comes_when_the_rest_ends() {
        let (mut dev, mut st) = stocked(3);
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Next), at(T0));
        assert_eq!(dev.beeps, vec!["press"]);
        run(&mut dev, &mut st, &cfg(), Wake::Timer, at(T0 + schedule::PANEL_REST_SECS));
        assert_eq!(dev.beeps, vec!["press", "ok"]);
        assert_eq!(st.ready_beep, None, "played once");
        assert_eq!(dev.saved.as_ref().map(|s| s.ready_beep), Some(None));
        run(&mut dev, &mut st, &cfg(), Wake::Timer, at(T0 + schedule::PANEL_REST_SECS + 60));
        assert_eq!(dev.beeps.len(), 2, "not again");
    }

    #[test]
    fn a_failed_update_ends_with_the_error_beep_when_the_rest_ends() {
        let (mut dev, mut st) = stocked(3);
        dev.offline = true;
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(T0));
        assert_eq!(dev.beeps, vec!["press"], "the failure screen is on the panel, which now rests");
        run(&mut dev, &mut st, &cfg(), Wake::Timer, at(T0 + schedule::PANEL_REST_SECS));
        assert_eq!(dev.beeps, vec!["press", "error"]);
    }

    #[test]
    fn timer_wake_during_panel_rest_sleeps_quietly() {
        let (mut dev, mut st) = stocked(3);
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Next), at(T0));
        dev.beeps.clear();
        let secs = run(&mut dev, &mut st, &cfg(), Wake::PowerOn, at(T0 + 30));
        assert_eq!(secs as i64, schedule::PANEL_REST_SECS - 30);
        assert!(dev.beeps.is_empty());
        assert_eq!(dev.shown.len(), 1);
    }

    #[test]
    fn led_is_on_while_working_and_while_the_panel_rests() {
        let (mut dev, mut st) = stocked(3);
        let secs = run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(T0));
        assert_eq!(dev.leds, vec![Led::On]);
        assert_eq!(secs as i64, schedule::PANEL_REST_SECS, "wake when the rest ends");
        assert_eq!(st.target, schedule::next_update(T0, &[(6, 0)]), "the scheduled update stays planned");
    }

    #[test]
    fn the_wake_at_the_end_of_the_rest_switches_the_led_off() {
        let (mut dev, mut st) = stocked(3);
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(T0));
        dev.leds.clear();
        let now = T0 + schedule::PANEL_REST_SECS;
        let secs = run(&mut dev, &mut st, &cfg(), Wake::Timer, at(now));
        assert_eq!(dev.leds, vec![Led::Off]);
        assert_eq!(dev.beeps, vec!["press", "ok"], "ready: the final beep");
        assert_eq!(secs as i64, st.target - now, "back to sleep until the scheduled update");
        assert_eq!(dev.shown.len(), 1, "no refresh");
    }

    #[test]
    fn led_is_off_after_a_failed_update() {
        let mut dev = Fake { offline: true, ..Fake::default() };
        let mut st = State::default();
        run(&mut dev, &mut st, &cfg(), Wake::PowerOn, at(T0));
        assert_eq!(dev.leds, vec![Led::On, Led::Off]);
        assert_eq!(dev.beeps, vec!["error"], "nothing to show, so ready at once: the final beep now");
    }

    #[test]
    fn led_while_asleep_follows_the_panel_rest() {
        let st = State { last_refresh: Some(T0), ..State::default() };
        assert_eq!(led_while_asleep(Some(T0 + 30), &st, 3600), (Led::On, 150));
        assert_eq!(led_while_asleep(Some(T0 + 30), &st, 60), (Led::On, 60), "an earlier wake stays");
        assert_eq!(led_while_asleep(Some(T0 + 180), &st, 3600), (Led::Off, 3600));
        assert_eq!(led_while_asleep(None, &st, 3600), (Led::Off, 3600), "time unknown");
    }

    #[test]
    fn without_panel_rest_every_press_refreshes() {
        let (mut dev, mut st) = stocked(3);
        let cfg = Config { panel_rest: false, ..cfg() };
        run(&mut dev, &mut st, &cfg, Wake::Button(Press::Next), at(T0));
        run(&mut dev, &mut st, &cfg, Wake::Button(Press::Next), at(T0 + 1));
        assert_eq!(dev.shown, vec![Some(1), Some(2)]);
        assert_eq!(dev.leds.last(), Some(&Led::Off), "no rest, nothing to wait for");
        assert_eq!(dev.beeps, vec!["press", "ok", "press", "ok"], "ready at once: the final beep right away");
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
        let i = fresh_info(T0, None, None, Some(stored.clone()));
        assert_eq!((i.fetched_at, i.events), (T0, Some(ev)));
        let next_day = fresh_info(T0 + 86_400, None, None, Some(stored.clone()));
        assert_eq!(next_day.events, Some(vec![]), "yesterday's events are not today's: its tomorrow is");
        assert_eq!(next_day.tomorrow, None);
        let later = fresh_info(T0 + 2 * 86_400, None, None, Some(stored));
        assert_eq!(later.events, None, "nothing known about this day");
    }

    #[test]
    fn failed_sync_redraws_the_current_photo_with_a_crossed_out_wifi_icon() {
        let (mut dev, mut st) = stocked(3);
        st.current = 1;
        dev.offline = true;
        // stored info is old: the timer wake needs Wi-Fi
        run(&mut dev, &mut st, &cfg(), Wake::Timer, at(T0 + 9 * 3600));
        assert_eq!(dev.shown, vec![Some(1)], "the photo that was on the panel");
        assert_eq!(dev.no_wifi, vec![true]);
        assert!(st.sync_failed);
        assert_eq!((st.attempt, st.last_refresh), (1, Some(T0 + 9 * 3600)));
        assert!(dev.beeps.is_empty(), "the error beep comes when the panel's rest ends");
        assert_eq!(st.ready_beep, Some(false));
    }

    #[test]
    fn failed_sync_with_nothing_stored_shows_nothing() {
        let mut dev = Fake { offline: true, ..Fake::default() };
        let mut st = State::default();
        run(&mut dev, &mut st, &cfg(), Wake::PowerOn, at(T0));
        assert!(dev.shown.is_empty());
        assert!(st.sync_failed);
    }

    #[test]
    fn the_icon_stays_while_browsing_and_goes_after_a_good_sync() {
        let (mut dev, mut st) = stocked(3);
        dev.offline = true;
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(T0));
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Next), at(T0 + 600));
        dev.offline = false;
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(T0 + 1200));
        assert_eq!(dev.no_wifi, vec![true, true, false]);
        assert!(!st.sync_failed);
    }

    #[test]
    fn a_good_sync_shows_no_icon() {
        let (mut dev, mut st) = stocked(3);
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(T0));
        assert_eq!(dev.no_wifi, vec![false]);
        assert!(!st.sync_failed);
    }

    #[test]
    fn a_partly_failed_sync_shows_the_new_photo_with_the_icon() {
        for (weather_down, missing) in [(true, vec![]), (false, vec!["calendar 2 of 2"])] {
            let (mut dev, mut st) = stocked(3);
            dev.next_batch = 10;
            dev.weather_down = weather_down;
            dev.missing = missing;
            run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(T0));
            assert_eq!(dev.shown, vec![Some(10)]);
            assert_eq!(dev.no_wifi, vec![true]);
            assert!(st.sync_failed);
            assert_eq!(st.attempt, 0, "not a failed update: no retry");
        }
    }

    #[test]
    fn fewer_photos_than_asked_for_count_as_a_partly_failed_sync() {
        let (mut dev, mut st) = stocked(3);
        dev.next_batch = 10;
        dev.photos_lost = 1;
        run(&mut dev, &mut st, &cfg(), Wake::Button(Press::Update), at(T0));
        assert_eq!(dev.shown, vec![Some(10)]);
        assert_eq!(dev.no_wifi, vec![true]);
        assert_eq!(st.stock_count, 2);
    }
}
