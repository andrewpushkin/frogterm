//! Cached list of the album's photo URLs (the PC twin of the firmware's flash list).
//!
//! Reading the whole album takes ~29 requests, so the list is kept and only
//! refreshed when it is older than `MAX_AGE_SECS`, belongs to another album,
//! or a cached URL stopped working.

use crate::album::Photo;
use crate::date::Date;

/// One week.
pub const MAX_AGE_SECS: u64 = 7 * 24 * 3600;

const HEADER: &str = "frogterm-photo-list v3";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoList {
    pub album_url: String,
    /// Unix time of the refresh.
    pub fetched_at: u64,
    /// Base URLs (without size options) with original sizes.
    pub photos: Vec<Photo>,
}

impl PhotoList {
    pub fn to_text(&self) -> String {
        let mut s = format!("{HEADER}\nalbum {}\nfetched {}\n", self.album_url, self.fetched_at);
        for p in &self.photos {
            let taken = p.taken.map_or("-".to_string(), Date::iso);
            s.push_str(&format!("{} {} {} {}\n", p.width, p.height, taken, p.url));
        }
        s
    }

    pub fn from_text(text: &str) -> Option<PhotoList> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let album_url = lines.next()?.strip_prefix("album ")?.to_string();
        let fetched_at = lines.next()?.strip_prefix("fetched ")?.parse().ok()?;
        let mut photos = Vec::new();
        for line in lines.filter(|l| !l.is_empty()) {
            // `<width> <height> <taken date or -> <url>`
            let mut f = line.splitn(4, ' ');
            let width = f.next()?.parse().ok()?;
            let height = f.next()?.parse().ok()?;
            let taken = match f.next()? {
                "-" => None,
                d => Some(Date::parse_iso(d)?),
            };
            let url = f.next()?.to_string();
            photos.push(Photo { url, width, height, taken });
        }
        Some(PhotoList { album_url, fetched_at, photos })
    }

    /// Usable without refreshing: same album, not empty, younger than a week.
    pub fn is_fresh_for(&self, album_url: &str, now: u64) -> bool {
        self.album_url == album_url && !self.photos.is_empty() && now.saturating_sub(self.fetched_at) < MAX_AGE_SECS
    }

    pub fn age_days(&self, now: u64) -> f64 {
        now.saturating_sub(self.fetched_at) as f64 / 86_400.0
    }
}

/// The list to keep after an album refresh. `complete` = every page was read.
/// A failed or partial refresh keeps the old list if there is one; with no old
/// list, a partial one is better than nothing.
pub fn after_refresh(
    old: Option<PhotoList>,
    fetched: Vec<Photo>,
    complete: bool,
    album_url: &str,
    now: u64,
) -> Option<PhotoList> {
    let fresh = PhotoList { album_url: album_url.to_string(), fetched_at: now, photos: fetched };
    match (complete && !fresh.photos.is_empty(), old) {
        (true, _) => Some(fresh),
        (false, Some(old)) => Some(old),
        // stale on purpose (fetched_at 0): the whole album is read again next wake
        (false, None) => (!fresh.photos.is_empty()).then_some(PhotoList { fetched_at: 0, ..fresh }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> PhotoList {
        let p = |id: &str, w, h, taken| Photo { url: format!("https://lh3.googleusercontent.com/pw/{id}"), width: w, height: h, taken };
        PhotoList {
            album_url: "https://photos.app.goo.gl/abc".into(),
            fetched_at: 1_795_600_000,
            photos: vec![p("A", 3000, 4000, Some(Date { year: 2026, month: 3, day: 7 })), p("B", 1920, 835, None)],
        }
    }

    #[test]
    fn round_trips_through_text_with_sizes_and_dates() {
        assert_eq!(PhotoList::from_text(&list().to_text()), Some(list()));
    }

    #[test]
    fn old_format_without_sizes_is_ignored() {
        let v1 = "frogterm-photo-list v1\nalbum https://photos.app.goo.gl/abc\nfetched 1\nhttps://lh3.googleusercontent.com/pw/A\n";
        assert_eq!(PhotoList::from_text(v1), None);
    }

    #[test]
    fn list_without_dates_is_read_again() {
        // v2 had no dates: ignoring it makes the next batch re-read the album
        let v2 = "frogterm-photo-list v2\nalbum https://photos.app.goo.gl/abc\nfetched 1\n3000 4000 https://lh3.googleusercontent.com/pw/A\n";
        assert_eq!(PhotoList::from_text(v2), None);
    }

    #[test]
    fn fresh_within_a_week_for_the_same_album() {
        let l = list();
        assert!(l.is_fresh_for(&l.album_url, l.fetched_at));
        assert!(l.is_fresh_for(&l.album_url, l.fetched_at + MAX_AGE_SECS - 1));
        assert!(!l.is_fresh_for(&l.album_url, l.fetched_at + MAX_AGE_SECS));
    }

    #[test]
    fn another_album_or_empty_list_is_not_fresh() {
        let l = list();
        assert!(!l.is_fresh_for("https://photos.app.goo.gl/other", l.fetched_at));
        let empty = PhotoList { photos: vec![], ..list() };
        assert!(!empty.is_fresh_for(&l.album_url, l.fetched_at));
    }

    #[test]
    fn garbage_is_not_a_list() {
        assert_eq!(PhotoList::from_text(""), None);
        assert_eq!(PhotoList::from_text("hello\nworld\n"), None);
        assert_eq!(PhotoList::from_text("frogterm-photo-list v3\nalbum x\nfetched soon\n"), None);
        assert_eq!(PhotoList::from_text("frogterm-photo-list v3\nalbum x\nfetched 1\n12 https://x\n"), None);
        assert_eq!(PhotoList::from_text("frogterm-photo-list v3\nalbum x\nfetched 1\n1 2 yesterday https://x\n"), None);
    }

    #[test]
    fn partial_refresh_policy() {
        let url = "https://photos.app.goo.gl/abc";
        let new = || list().photos[..1].to_vec();
        let old = || Some(list());
        // complete refresh replaces the old list
        let got = after_refresh(old(), new(), true, url, 9).unwrap();
        assert_eq!((got.photos.len(), got.fetched_at), (1, 9));
        // partial refresh keeps the old list
        assert_eq!(after_refresh(old(), new(), false, url, 9), old());
        // no old list: partial is better than nothing, but re-read soon (stale)
        let now = 1_795_600_000;
        let partial = after_refresh(None, new(), false, url, now).unwrap();
        assert_eq!(partial.photos.len(), 1);
        assert!(!partial.is_fresh_for(url, now), "a partial list must be re-read on the next wake");
        // nothing at all
        assert_eq!(after_refresh(None, vec![], false, url, 9), None);
        // an empty "complete" album never replaces a good list
        assert_eq!(after_refresh(old(), vec![], true, url, 9), old());
    }

    #[test]
    fn age_is_reported_in_days() {
        let l = list();
        assert_eq!(l.age_days(l.fetched_at + 36 * 3600), 1.5);
        assert_eq!(l.age_days(l.fetched_at - 10), 0.0);
    }
}
