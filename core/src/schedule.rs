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

/// Local times of the daily screen updates (owner: 06:00, 13:30, 18:15).
pub const DEFAULT_TIMES: &[(u8, u8)] = &[(6, 0), (13, 30), (18, 15)];

/// `"13:30"` (or `"6:15"`) → `(13, 30)`.
pub fn parse_time(s: &str) -> Option<(u8, u8)> {
    let (h, m) = s.trim().split_once(':')?;
    let ok = |p: &str, max: u8| (1..=2).contains(&p.len()).then(|| p.parse::<u8>().ok()).flatten().filter(|&v| v <= max);
    if m.len() != 2 {
        return None;
    }
    Some((ok(h, 23)?, ok(m, 59)?))
}

/// `"06:00,13:30,18:15"` → the times; `None` if empty or one is bad.
pub fn parse_times(s: &str) -> Option<Vec<(u8, u8)>> {
    let times: Option<Vec<_>> = s.split(',').map(parse_time).collect();
    times.filter(|t| !t.is_empty())
}

/// Unix time of the next of the local `times` that is more than 60 s after `now`.
pub fn next_update(now: i64, times: &[(u8, u8)]) -> i64 {
    let (today, _) = tz::to_local(now);
    let times = if times.is_empty() { DEFAULT_TIMES } else { times };
    (0..=1)
        .flat_map(|days| {
            let d = Date::from_days(today.days_since_epoch() + days);
            times.iter().map(move |&(h, m)| tz::from_local(d, u32::from(h) * 3600 + u32::from(m) * 60))
        })
        .filter(|&t| t > now + 60)
        .min()
        .expect("tomorrow's times are always later")
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
/// the next scheduled update; with an unknown time, try again in an hour.
pub fn after_failure(now: Option<i64>, attempt: u8, times: &[(u8, u8)]) -> Retry {
    let Some(now) = now else {
        return Retry { target: 0, secs: when_time_unknown() };
    };
    match retry_after_failure(attempt) {
        Some(secs) => Retry { target: now + secs as i64, secs },
        None => {
            let target = next_update(now, times);
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

/// E-paper care (Waveshare, Spectra 6 family): at least 180 s between
/// refreshes, for scheduled updates and button presses alike.
pub const PANEL_REST_SECS: i64 = 180;

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

    const SIX: &[(u8, u8)] = &[(6, 0)];

    #[test]
    fn next_update_is_today_or_tomorrow_at_6_local() {
        assert_eq!(next_update(local(2026, 11, 25, 5, 0, 0), SIX), local(2026, 11, 25, 6, 0, 0));
        assert_eq!(next_update(local(2026, 11, 25, 5, 59, 30), SIX), local(2026, 11, 26, 6, 0, 0), "within 60 s");
        assert_eq!(next_update(local(2026, 11, 25, 14, 0, 0), SIX), local(2026, 11, 26, 6, 0, 0));
    }

    #[test]
    fn next_update_is_the_next_of_three_daily_times() {
        let at = |h, m| local(2026, 11, 25, h, m, 0);
        assert_eq!(next_update(at(5, 0), DEFAULT_TIMES), at(6, 0));
        assert_eq!(next_update(at(6, 0), DEFAULT_TIMES), at(13, 30), "just updated at 06:00");
        assert_eq!(next_update(at(13, 0), DEFAULT_TIMES), at(13, 30));
        assert_eq!(next_update(at(13, 30), DEFAULT_TIMES), at(18, 15));
        assert_eq!(next_update(at(18, 15), DEFAULT_TIMES), local(2026, 11, 26, 6, 0, 0));
        assert_eq!(next_update(at(23, 0), &[(18, 15), (6, 0)]), local(2026, 11, 26, 6, 0, 0), "order does not matter");
    }

    #[test]
    fn update_times_parse_from_text() {
        assert_eq!(parse_time("06:00"), Some((6, 0)));
        assert_eq!(parse_time("13:30"), Some((13, 30)));
        assert_eq!(parse_time("6:15"), Some((6, 15)));
        for bad in ["24:00", "12:60", "12", "ab:cd", "", "12:3"] {
            assert_eq!(parse_time(bad), None, "{bad:?}");
        }
        assert_eq!(parse_times("06:00,13:30,18:15"), Some(DEFAULT_TIMES.to_vec()));
        assert_eq!(parse_times("06:00,25:00"), None);
        assert_eq!(parse_times(""), None, "at least one time");
    }

    #[test]
    fn next_update_across_dst() {
        let utc = |y: i32, m: u8, d: u8, h: i64| Date { year: y, month: m, day: d }.days_since_epoch() * 86_400 + h * 3600;
        assert_eq!(next_update(local(2026, 3, 26, 12, 0, 0), SIX), utc(2026, 3, 27, 3), "06:00 IDT");
        assert_eq!(next_update(local(2026, 10, 24, 12, 0, 0), SIX), utc(2026, 10, 25, 4), "06:00 IST");
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
        let r = after_failure(Some(now), 1, SIX);
        assert_eq!(r.secs, 1800);
        assert_eq!(on_timer_wake(Some(now + r.secs as i64), r.target), WakePlan::Update);
        // even if the RC clock wakes the chip a bit early
        assert_eq!(on_timer_wake(Some(now + r.secs as i64 - 60), r.target), WakePlan::Update);
    }

    #[test]
    fn after_three_failures_wait_for_the_daily_update() {
        let now = local(2026, 11, 25, 7, 30, 0);
        let r = after_failure(Some(now), 4, SIX);
        assert_eq!(r.target, local(2026, 11, 26, 6, 0, 0));
        assert_eq!(r.secs as i64, r.target - now);
        let r = after_failure(Some(now), 4, DEFAULT_TIMES);
        assert_eq!(r.target, local(2026, 11, 25, 13, 30, 0), "or the next of the daily updates");
    }

    #[test]
    fn failure_with_unknown_time_retries_hourly() {
        let r = after_failure(None, 1, SIX);
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
    fn held_button_is_not_armed() {
        assert!(!arm_button(true));
        assert!(arm_button(false));
    }
}
