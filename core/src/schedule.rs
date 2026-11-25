//! When to wake up, retry and sleep (spec 7.2, 7.6).

use crate::date::Date;
use crate::tz;

/// What to do after a timer wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakePlan {
    Update,
    /// Woke early (RC clock drift): sleep this many more seconds.
    Sleep(u64),
}

/// Unix time of the next local `hour:00` that is more than 60 s after `now`.
pub fn next_update(now: i64, hour: u8) -> i64 {
    let (today, _) = tz::to_local(now);
    let at = |d: Date| tz::from_local(d, u32::from(hour) * 3600);
    let candidate = at(today);
    if candidate > now + 60 {
        candidate
    } else {
        at(Date::from_days(today.days_since_epoch() + 1))
    }
}

/// Waking up to this much early still counts as on time.
const EARLY_SLACK_SECS: i64 = 120;

/// After a timer wake: `None` = time unknown → update now.
pub fn on_timer_wake(now: Option<i64>, target: i64) -> WakePlan {
    match now {
        Some(now) if target - now > EARLY_SLACK_SECS => WakePlan::Sleep((target - now) as u64),
        _ => WakePlan::Update,
    }
}

/// Seconds until the next try after the `attempt`-th failed update in a row,
/// or `None` to fall back to the daily schedule.
pub fn retry_after_failure(attempt: u8) -> Option<u64> {
    (attempt <= 3).then_some(30 * 60)
}

/// Where to go after a failed update (or before trying one, in case it hangs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    /// Wake target to store (what `on_timer_wake` compares against).
    pub target: i64,
    pub secs: u64,
}

/// After the `attempt`-th failure in a row: retry in 30 min (≤ 3 times), else
/// the next daily update; with an unknown time, try again in an hour.
pub fn after_failure(now: Option<i64>, attempt: u8, hour: u8) -> Retry {
    let Some(now) = now else {
        return Retry { target: 0, secs: when_time_unknown() };
    };
    match retry_after_failure(attempt) {
        Some(secs) => Retry { target: now + secs as i64, secs },
        None => {
            let target = next_update(now, hour);
            Retry { target, secs: (target - now) as u64 }
        }
    }
}

/// What one wake has to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePlan {
    /// Stored photo + stored weather/calendar: no Wi-Fi at all.
    Offline,
    /// Fresh weather/calendar, stored photo.
    RefreshInfo,
    /// Fresh weather/calendar and a new batch of photos (every stored one was seen).
    RefreshAll,
}

/// `info` = (fetch time, local date of its events) of the stored weather/calendar;
/// `photos_left` = stored photos not seen yet.
pub fn plan_update(now: i64, info: Option<(i64, Date)>, photos_left: usize, max_age_secs: i64) -> UpdatePlan {
    if photos_left == 0 {
        return UpdatePlan::RefreshAll;
    }
    let today = tz::to_local(now).0;
    match info {
        Some((fetched, date)) if now - fetched < max_age_secs && date == today => UpdatePlan::Offline,
        _ => UpdatePlan::RefreshInfo,
    }
}

/// Whether this update was asked for by a person (progress ticks are played):
/// a button wake, or a timer wake that carries out a press postponed for
/// panel rest.
pub fn started_by_person(button_wake: bool, timer_wake: bool, postponed_press: bool) -> bool {
    button_wake || (timer_wake && postponed_press)
}

/// E-paper care (Waveshare, Spectra 6 family): at least 180 s between refreshes.
pub const PANEL_REST_SECS: i64 = 180;
/// For a person pressing a button the owner accepts a shorter rest: the
/// refresh itself takes ~33 s, so this leaves ~30 s between refreshes.
pub const BUTTON_REST_SECS: i64 = 60;

/// Seconds to wait before the panel may refresh again (after `rest` seconds),
/// or `None` if it may now.
pub fn panel_rest(now: i64, last_refresh: Option<i64>, rest: i64) -> Option<u64> {
    let since = now - last_refresh?;
    (0..rest).contains(&since).then(|| (rest - since) as u64)
}

/// Sleep when the time is unknown (no RTC, no SNTP).
pub fn when_time_unknown() -> u64 {
    3600
}

/// Arm the button wake only when the button is not held (else ext1 would
/// wake the chip immediately, forever).
pub fn arm_button(button_held: bool) -> bool {
    !button_held
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(y: i32, m: u8, d: u8, h: u32, min: u32, s: u32) -> i64 {
        tz::from_local(Date { year: y, month: m, day: d }, h * 3600 + min * 60 + s)
    }

    #[test]
    fn next_update_is_today_or_tomorrow_at_6_local() {
        assert_eq!(next_update(local(2026, 11, 25, 5, 0, 0), 6), local(2026, 11, 25, 6, 0, 0));
        assert_eq!(next_update(local(2026, 11, 25, 5, 59, 30), 6), local(2026, 11, 26, 6, 0, 0), "within 60 s");
        assert_eq!(next_update(local(2026, 11, 25, 14, 0, 0), 6), local(2026, 11, 26, 6, 0, 0));
    }

    #[test]
    fn next_update_across_dst() {
        let utc = |y: i32, m: u8, d: u8, h: i64| Date { year: y, month: m, day: d }.days_since_epoch() * 86_400 + h * 3600;
        assert_eq!(next_update(local(2026, 3, 26, 12, 0, 0), 6), utc(2026, 3, 27, 3), "06:00 IDT");
        assert_eq!(next_update(local(2026, 10, 24, 12, 0, 0), 6), utc(2026, 10, 25, 4), "06:00 IST");
    }

    #[test]
    fn early_timer_wake_sleeps_again() {
        let target = local(2026, 11, 26, 6, 0, 0);
        assert_eq!(on_timer_wake(Some(target - 600), target), WakePlan::Sleep(600));
        assert_eq!(on_timer_wake(Some(target - 60), target), WakePlan::Update);
        assert_eq!(on_timer_wake(Some(target + 300), target), WakePlan::Update);
    }

    #[test]
    fn unknown_time_retries_hourly() {
        assert_eq!(on_timer_wake(None, 0), WakePlan::Update);
        assert_eq!(when_time_unknown(), 3600);
    }

    #[test]
    fn retry_after_failure_three_times_then_daily() {
        assert_eq!(retry_after_failure(1), Some(1800));
        assert_eq!(retry_after_failure(3), Some(1800));
        assert_eq!(retry_after_failure(4), None);
    }

    #[test]
    fn retry_wake_is_not_mistaken_for_an_early_wake() {
        let now = local(2026, 11, 25, 6, 0, 30);
        let r = after_failure(Some(now), 1, 6);
        assert_eq!(r.secs, 1800);
        assert_eq!(on_timer_wake(Some(now + r.secs as i64), r.target), WakePlan::Update);
        // even if the RC clock wakes the chip a bit early
        assert_eq!(on_timer_wake(Some(now + r.secs as i64 - 60), r.target), WakePlan::Update);
    }

    #[test]
    fn after_three_failures_wait_for_the_daily_update() {
        let now = local(2026, 11, 25, 7, 30, 0);
        let r = after_failure(Some(now), 4, 6);
        assert_eq!(r.target, local(2026, 11, 26, 6, 0, 0));
        assert_eq!(r.secs as i64, r.target - now);
    }

    #[test]
    fn failure_with_unknown_time_retries_hourly() {
        let r = after_failure(None, 1, 6);
        assert_eq!(r.secs, 3600);
        assert_eq!(on_timer_wake(None, r.target), WakePlan::Update);
    }

    #[test]
    fn empty_photo_stock_downloads_a_new_batch() {
        let now = local(2026, 11, 25, 9, 0, 0);
        let today = Date { year: 2026, month: 11, day: 25 };
        assert_eq!(plan_update(now, Some((now - 60, today)), 0, 3600), UpdatePlan::RefreshAll);
        assert_eq!(plan_update(now, None, 0, 3600), UpdatePlan::RefreshAll);
    }

    #[test]
    fn recent_info_needs_no_wifi() {
        let now = local(2026, 11, 25, 9, 0, 0);
        let today = Date { year: 2026, month: 11, day: 25 };
        assert_eq!(plan_update(now, Some((now - 59 * 60, today)), 3, 3600), UpdatePlan::Offline);
    }

    #[test]
    fn old_or_missing_info_is_refreshed() {
        let now = local(2026, 11, 25, 9, 0, 0);
        let today = Date { year: 2026, month: 11, day: 25 };
        assert_eq!(plan_update(now, Some((now - 3600, today)), 3, 3600), UpdatePlan::RefreshInfo, "an hour old");
        assert_eq!(plan_update(now, None, 3, 3600), UpdatePlan::RefreshInfo, "never fetched");
        // fetched 23:50 yesterday, now 00:05: recent, but the events are yesterday's
        let just_after_midnight = local(2026, 11, 26, 0, 5, 0);
        assert_eq!(
            plan_update(just_after_midnight, Some((just_after_midnight - 15 * 60, today)), 3, 3600),
            UpdatePlan::RefreshInfo
        );
    }

    #[test]
    fn postponed_press_is_still_a_persons_update() {
        assert!(started_by_person(true, false, false), "button wake");
        assert!(started_by_person(false, true, true), "timer wake carrying out a postponed press");
        assert!(!started_by_person(false, true, false), "06:00 timer");
        assert!(!started_by_person(false, false, true), "power-on is not a press");
    }

    #[test]
    fn panel_rests_180_s_between_refreshes() {
        let t = 1_795_600_000;
        assert_eq!(panel_rest(t, None, PANEL_REST_SECS), None, "never refreshed");
        assert_eq!(panel_rest(t, Some(t - 60), PANEL_REST_SECS), Some(120));
        assert_eq!(panel_rest(t, Some(t - 180), PANEL_REST_SECS), None);
        assert_eq!(panel_rest(t, Some(t - 3600), PANEL_REST_SECS), None);
        // clock moved backwards (RTC reset): don't wait forever
        assert_eq!(panel_rest(t, Some(t + 7200), PANEL_REST_SECS), None);
    }

    #[test]
    fn button_presses_rest_only_60_s() {
        let t = 1_795_600_000;
        assert_eq!(BUTTON_REST_SECS, 60);
        assert_eq!(panel_rest(t, Some(t - 40), BUTTON_REST_SECS), Some(20));
        assert_eq!(panel_rest(t, Some(t - 60), BUTTON_REST_SECS), None);
    }

    #[test]
    fn held_button_is_not_armed() {
        assert!(!arm_button(true));
        assert!(arm_button(false));
    }
}
