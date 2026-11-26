//! Reads a public Google Photos shared album ("anyone with the link").
//!
//! How Google serves it (verified 2026-11-25):
//!
//! * The share link redirects to `https://photos.google.com/share/<album_id>?key=<key>`.
//!   That HTML page embeds the first page of the album as JSON in
//!   `AF_initDataCallback({key: 'ds:1', hash: '…', data:<PAYLOAD>, sideChannel: {}});`.
//! * Further pages come from the same internal request the website makes while
//!   scrolling: `POST https://photos.google.com/_/PhotosUi/data/batchexecute?rpcids=snAcKc`
//!   with form field `f.req=[[["snAcKc","[album_id,token,null,key]",null,"generic"]]]`.
//!   The response is `)]}'` followed by length-prefixed JSON chunks; the one starting with
//!   `[["wrb.fr","snAcKc","<PAYLOAD as escaped JSON string>",…]]` holds the page.
//!   No login, cookies or session values are needed. This is undocumented and may change.
//!
//! PAYLOAD (same shape in both places):
//! `[null, [ITEM, …up to 300], "<next page token>" | null, [album_id, title, …, [21]=total, …], …]`
//! ITEM: `["<media id>", ["https://lh3.googleusercontent.com/pw/…", width, height, …], taken, "…", utc_offset, added, …]`
//! `taken` = when the photo was taken (Unix ms, UTC), `utc_offset` = the camera's
//! offset then (ms, e.g. 7200000), `added` = when it was put in the album.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::date::Date;

/// One photo of the album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Photo {
    /// Base URL without size options, e.g. `https://lh3.googleusercontent.com/pw/AP1G...`.
    pub url: String,
    /// Original width in pixels.
    pub width: u32,
    /// Original height in pixels.
    pub height: u32,
    /// Local date the photo was taken (where it was taken), if Google says.
    pub taken: Option<Date>,
}

impl Photo {
    /// URL that makes Google return a JPEG resized and cropped to exactly `w`×`h`.
    /// `-p` is Google's smart crop (keeps the subject, better than centre crop `-c`
    /// on the owner's mostly-portrait photos). `-rj` forces JPEG; without it a PNG
    /// original (e.g. a screenshot) comes back as PNG.
    pub fn sized_url(&self, w: u32, h: u32) -> String {
        format!("{}=w{}-h{}-p-rj", self.url, w, h)
    }

    /// True if the photo loses less by cropping into a half-screen slot (≈5:6)
    /// than into the full screen (5:3), i.e. portrait or square photos.
    pub fn is_tall(&self) -> bool {
        // w/h below the geometric mean of 5:6 and 5:3, i.e. (w/h)² < 25/18
        let (w, h) = (u64::from(self.width), u64::from(self.height));
        18 * w * w < 25 * h * h
    }
}

/// One page of the album (the share page or one `snAcKc` response).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumPage {
    pub album_id: Option<String>,
    pub photos: Vec<Photo>,
    pub next_token: Option<String>,
    /// Number of items Google says the album has (live albums grow over time).
    pub total: Option<u64>,
}

const DS1_START: &str = "AF_initDataCallback({key: 'ds:1'";
const DS1_END: &str = ", sideChannel:";
const RPC_ID: &str = "snAcKc";
pub const RPC_URL: &str = "https://photos.google.com/_/PhotosUi/data/batchexecute?rpcids=snAcKc";

/// Album id and key from the URL the share link redirects to:
/// `https://photos.google.com/share/<album_id>?key=<key>`.
pub fn ids_from_share_url(url: &str) -> Result<(String, String)> {
    let rest = url
        .split_once("/share/")
        .map(|(_, r)| r)
        .ok_or_else(|| anyhow!("not a photos.google.com/share/ URL"))?;
    let (id, query) = rest.split_once('?').ok_or_else(|| anyhow!("share URL has no ?key="))?;
    let key = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("key="))
        .filter(|k| !k.is_empty())
        .ok_or_else(|| anyhow!("share URL has no key="))?;
    Ok((id.to_string(), key.to_string()))
}

/// First page of the album from the share page HTML.
pub fn parse_share_page(html: &str) -> Result<AlbumPage> {
    let start = html
        .find(DS1_START)
        .ok_or_else(|| anyhow!("album data block (ds:1) not found on the share page"))?;
    let block = &html[start..];
    let data = block.find("data:").ok_or_else(|| anyhow!("ds:1 block has no data"))? + "data:".len();
    let end = block.find(DS1_END).ok_or_else(|| anyhow!("ds:1 block is not terminated"))?;
    if end <= data {
        bail!("ds:1 block is malformed");
    }
    let payload: Value = serde_json::from_str(&block[data..end]).context("ds:1 data is not JSON")?;
    parse_payload(&payload)
}

/// Reads the share page as a stream and keeps only the album data block
/// (the page is ~1.4 MB, the block ~200 KB).
pub struct ShareScanner {
    /// Before the block: the last few bytes (marker may span chunks);
    /// inside: the block captured so far.
    buf: Vec<u8>,
    state: ScanState,
    peak: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Searching,
    Capturing,
    Done,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

impl Default for ShareScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl ShareScanner {
    pub fn new() -> Self {
        Self { buf: Vec::new(), state: ScanState::Searching, peak: 0 }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        let (start, end) = (DS1_START.as_bytes(), DS1_END.as_bytes());
        match self.state {
            ScanState::Done => return,
            ScanState::Searching => {
                self.buf.extend_from_slice(chunk);
                self.peak = self.peak.max(self.buf.len());
                match find(&self.buf, start) {
                    Some(i) => {
                        self.buf.drain(..i);
                        self.state = ScanState::Capturing;
                    }
                    None => {
                        let keep = start.len() - 1;
                        if self.buf.len() > keep {
                            self.buf.drain(..self.buf.len() - keep);
                        }
                        return;
                    }
                }
            }
            ScanState::Capturing => {
                self.buf.extend_from_slice(chunk);
                self.peak = self.peak.max(self.buf.len());
            }
        }
        // look for the end only in the new bytes (plus an overlap for split markers)
        let from = self.buf.len().saturating_sub(chunk.len() + end.len());
        if let Some(i) = find(&self.buf[from..], end) {
            self.buf.truncate(from + i + end.len());
            self.state = ScanState::Done;
        }
    }

    /// Largest number of bytes held at once (for memory checks).
    pub fn peak(&self) -> usize {
        self.peak
    }

    pub fn finish(self) -> Result<AlbumPage> {
        if self.state != ScanState::Done {
            bail!("album data block (ds:1) not found on the share page");
        }
        parse_share_page(&String::from_utf8_lossy(&self.buf))
    }
}

/// `application/x-www-form-urlencoded` body for the next-page request.
pub fn rpc_body(album_id: &str, key: &str, token: &str) -> String {
    format!("f.req={}", crate::url::percent_encode(&rpc_form(album_id, key, token)))
}

/// Form body for the next-page request.
pub fn rpc_form(album_id: &str, key: &str, token: &str) -> String {
    let inner = serde_json::json!([album_id, token, null, key]).to_string();
    serde_json::json!([[[RPC_ID, inner, null, "generic"]]]).to_string()
}

/// One page from a `snAcKc` batchexecute response body.
pub fn parse_rpc_response(body: &str) -> Result<AlbumPage> {
    let prefix = format!("[[\"wrb.fr\",\"{RPC_ID}\"");
    let line = body
        .lines()
        .find(|l| l.starts_with(&prefix))
        .ok_or_else(|| anyhow!("no {RPC_ID} result in response (format changed or request rejected)"))?;
    let outer: Value = serde_json::from_str(line).context("response chunk is not JSON")?;
    let inner = outer[0][2]
        .as_str()
        .ok_or_else(|| anyhow!("{RPC_ID} result has no payload (album key wrong or link sharing off?)"))?;
    let payload: Value = serde_json::from_str(inner).context("payload is not JSON")?;
    parse_payload(&payload)
}

fn parse_payload(p: &Value) -> Result<AlbumPage> {
    let items = p[1].as_array().ok_or_else(|| anyhow!("payload has no item list"))?;
    let photos = items.iter().filter_map(parse_item).collect();
    Ok(AlbumPage {
        album_id: p[3][0].as_str().map(str::to_string),
        photos,
        next_token: p[2].as_str().filter(|t| !t.is_empty()).map(str::to_string),
        total: p[3][21].as_u64(),
    })
}

fn parse_item(item: &Value) -> Option<Photo> {
    let d = &item[1];
    let url = d[0].as_str()?;
    if !url.starts_with("https://lh3.googleusercontent.com/") {
        return None;
    }
    Some(Photo {
        url: url.to_string(),
        width: u32::try_from(d[1].as_u64()?).ok()?,
        height: u32::try_from(d[2].as_u64()?).ok()?,
        taken: taken_date(item),
    })
}

/// Local date of `taken` + `utc_offset` (offset 0 when missing).
fn taken_date(item: &Value) -> Option<Date> {
    let ms = item[2].as_i64().filter(|&ms| ms > 0)?;
    let offset_ms = item[4].as_i64().filter(|o| o.abs() <= 14 * 3_600_000).unwrap_or(0);
    Some(Date::from_unix(ms.div_euclid(1000), (offset_ms / 1000) as i32))
}

/// 64-bit fingerprint of a photo URL: skips photos repeated across album
/// pages without keeping a copy of every URL.
pub fn url_key(url: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_key_tells_urls_apart() {
        let a = "https://lh3.googleusercontent.com/pw/A";
        assert_eq!(url_key(a), url_key(&a.to_string()));
        assert_ne!(url_key(a), url_key("https://lh3.googleusercontent.com/pw/B"));
    }

    fn item(n: u32, w: u32, h: u32) -> Value {
        serde_json::json!([
            format!("AF1Qip{n}"),
            [format!("https://lh3.googleusercontent.com/pw/P{n}"), w, h, null, [null, null, 1]],
            1766156380000u64,
            "x",
            7200000,
            1795604269656u64,
            {"15": 1}
        ])
    }

    fn payload(items: Vec<Value>, token: Option<&str>, total: u64) -> Value {
        let mut meta = vec![Value::Null; 22];
        meta[0] = "ALBUM1".into();
        meta[1] = "frogterm".into();
        meta[21] = total.into();
        serde_json::json!([null, items, token, meta, null, 0])
    }

    fn share_page(p: &Value) -> String {
        format!(
            "<html><script>AF_initDataCallback({{key: 'ds:0', hash: '1', data:[], sideChannel: {{}}}});</script>\
             <script>AF_initDataCallback({{key: 'ds:1', hash: '2', data:{p}, sideChannel: {{}}}});</script></html>"
        )
    }

    fn fake_rpc_response(p: &Value) -> String {
        let chunk = serde_json::json!([["wrb.fr", "snAcKc", p.to_string(), null, null, null, "generic"]]).to_string();
        format!(")]}}'\n\n{}\n{chunk}\n57\n[[\"di\",538],[\"af.httprm\",537,\"-1\",9]]\n", chunk.len())
    }

    #[test]
    fn share_page_gives_first_page_token_id_and_total() {
        let p = payload(vec![item(1, 3000, 4000), item(2, 4000, 3000)], Some("TOKEN1"), 6797);
        let page = parse_share_page(&share_page(&p)).unwrap();
        // 1766156380000 ms = 2025-12-19 15:00 UTC, 17:00 at the camera's +2 h
        let taken = Some(Date { year: 2025, month: 12, day: 19 });
        assert_eq!(page.album_id.as_deref(), Some("ALBUM1"));
        assert_eq!(page.next_token.as_deref(), Some("TOKEN1"));
        assert_eq!(page.total, Some(6797));
        assert_eq!(
            page.photos,
            vec![
                Photo { url: "https://lh3.googleusercontent.com/pw/P1".into(), width: 3000, height: 4000, taken },
                Photo { url: "https://lh3.googleusercontent.com/pw/P2".into(), width: 4000, height: 3000, taken },
            ]
        );
    }

    #[test]
    fn rpc_response_gives_page_and_next_token() {
        let p = payload(vec![item(3, 1080, 1920)], Some("TOKEN2"), 4321);
        let page = parse_rpc_response(&fake_rpc_response(&p)).unwrap();
        assert_eq!(page.photos.len(), 1);
        assert_eq!(page.photos[0].url, "https://lh3.googleusercontent.com/pw/P3");
        assert_eq!(page.next_token.as_deref(), Some("TOKEN2"));
    }

    #[test]
    fn taken_date_is_local_to_the_camera() {
        // 2025-12-19 23:30 UTC: still the 19th in London, already the 20th in Israel
        let ms = Date { year: 2025, month: 12, day: 19 }.days_since_epoch() * 86_400_000 + 23 * 3_600_000 + 1_800_000;
        let at = |offset: Value| serde_json::json!(["id", ["https://lh3.googleusercontent.com/pw/P", 1, 1], ms, "x", offset]);
        assert_eq!(parse_item(&at(0.into())).unwrap().taken, Some(Date { year: 2025, month: 12, day: 19 }));
        assert_eq!(parse_item(&at(7_200_000.into())).unwrap().taken, Some(Date { year: 2025, month: 12, day: 20 }));
        assert_eq!(parse_item(&at(Value::Null)).unwrap().taken, Some(Date { year: 2025, month: 12, day: 19 }), "no offset: UTC");
        let no_time = serde_json::json!(["id", ["https://lh3.googleusercontent.com/pw/P", 1, 1], null]);
        assert_eq!(parse_item(&no_time).unwrap().taken, None);
    }

    #[test]
    fn last_page_has_no_token() {
        let p = payload(vec![item(4, 10, 10)], None, 1);
        assert_eq!(parse_rpc_response(&fake_rpc_response(&p)).unwrap().next_token, None);
    }

    #[test]
    fn items_without_url_or_size_are_skipped() {
        let bad = vec![
            serde_json::json!(["a", ["https://lh3.googleusercontent.com/pw/X"]]),
            serde_json::json!(["b", ["https://example.com/evil", 1, 1]]),
            serde_json::json!(null),
            item(5, 1, 2),
        ];
        let page = parse_share_page(&share_page(&payload(bad, None, 4))).unwrap();
        assert_eq!(page.photos.len(), 1);
    }

    #[test]
    fn share_scanner_matches_whole_page_parse() {
        let page = share_page(&payload(vec![item(1, 3000, 4000), item(2, 4000, 3000)], Some("TOKEN1"), 6797));
        let want = parse_share_page(&page).unwrap();
        for size in [1, 7, 4096] {
            let mut s = ShareScanner::new();
            for c in page.as_bytes().chunks(size) {
                s.feed(c);
            }
            assert_eq!(s.finish().unwrap(), want, "chunk size {size}");
        }
    }

    #[test]
    fn share_scanner_keeps_little_memory() {
        let block = share_page(&payload(vec![item(1, 3000, 4000)], None, 1));
        let page = format!("<html>{}{block}{}", "x".repeat(1 << 20), "y".repeat(1 << 20));
        let mut s = ShareScanner::new();
        for c in page.as_bytes().chunks(4096) {
            s.feed(c);
        }
        assert!(s.peak() <= block.len() + 4096 + DS1_START.len(), "peak {}", s.peak());
        assert_eq!(s.finish().unwrap().photos.len(), 1);
    }

    #[test]
    fn share_scanner_without_block_is_an_error() {
        let mut s = ShareScanner::new();
        s.feed(b"<html>sign in</html>");
        assert!(s.finish().is_err());
    }

    #[test]
    fn rpc_body_is_form_encoded() {
        let body = rpc_body("ALBUM1", "KEY", "TOK");
        let enc = body.strip_prefix("f.req=").expect("field name");
        assert!(!enc.contains(['[', '"', ',', ' ']), "{enc}");
        let decoded = crate::url::percent_decode(enc);
        assert_eq!(decoded, rpc_form("ALBUM1", "KEY", "TOK"));
    }

    #[test]
    fn page_without_album_block_is_an_error() {
        assert!(parse_share_page("<html>sign in</html>").is_err());
    }

    #[test]
    fn rejected_rpc_is_an_error() {
        let body = ")]}'\n\n60\n[[\"wrb.fr\",\"snAcKc\",null,null,null,[3],\"generic\"]]\n";
        assert!(parse_rpc_response(body).is_err());
        assert!(parse_rpc_response("<html>error</html>").is_err());
    }

    #[test]
    fn rpc_form_nests_request_as_json_string() {
        let f: Value = serde_json::from_str(&rpc_form("ALBUM1", "KEY", "TOK")).unwrap();
        assert_eq!(f[0][0][0], "snAcKc");
        assert_eq!(f[0][0][3], "generic");
        let inner: Value = serde_json::from_str(f[0][0][1].as_str().unwrap()).unwrap();
        assert_eq!(inner, serde_json::json!(["ALBUM1", "TOK", null, "KEY"]));
    }

    #[test]
    fn ids_come_from_share_url() {
        let (id, key) = ids_from_share_url("https://photos.google.com/share/AF1QipX?key=K123&hl=en").unwrap();
        assert_eq!((id.as_str(), key.as_str()), ("AF1QipX", "K123"));
        assert!(ids_from_share_url("https://photos.google.com/share/AF1QipX").is_err());
        assert!(ids_from_share_url("https://accounts.google.com/login").is_err());
    }

    #[test]
    fn portrait_and_square_photos_are_tall() {
        let p = |w, h| Photo { url: String::new(), width: w, height: h, taken: None };
        assert!(p(3000, 4000).is_tall());
        assert!(p(1080, 1920).is_tall());
        assert!(p(128, 128).is_tall(), "a square loses less in a half slot");
        assert!(!p(4000, 3000).is_tall());
        assert!(!p(1920, 835).is_tall());
        assert!(!p(0, 0).is_tall(), "unknown size is treated as landscape");
    }

    #[test]
    fn sized_url_asks_for_smart_crop_jpeg() {
        let p = Photo { url: "https://lh3.googleusercontent.com/pw/P1".into(), width: 1, height: 1, taken: None };
        assert_eq!(p.sized_url(800, 480), "https://lh3.googleusercontent.com/pw/P1=w800-h480-p-rj");
    }
}
