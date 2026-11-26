//! HTTP side of the PC tools (the firmware does the same with esp-idf's HTTP client).

use std::io::Read;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use frogterm_core::album::{self, Photo};

const USER_AGENT: &str = "frogterm/0.1";

/// HTTP status error, kept as a type so callers can tell "photo gone" (403/404) apart.
#[derive(Debug)]
pub struct HttpStatus(pub u16);

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}", self.0)
    }
}

impl std::error::Error for HttpStatus {}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .redirects(10)
        .timeout(Duration::from_secs(30))
        .build()
}

pub fn get_text(agent: &ureq::Agent, url: &str) -> Result<String> {
    Ok(agent.get(url).call().map_err(http_err)?.into_string()?)
}

/// Progress of one album page: page number (1-based), photos added, bytes, time, album total.
pub struct PageInfo {
    pub page: usize,
    pub added: usize,
    pub bytes: usize,
    pub took: Duration,
    pub total: Option<u64>,
}

/// Reads the whole album (share page + every next page), de-duplicated, in album order.
pub fn fetch_album(
    agent: &ureq::Agent,
    share_link: &str,
    max_pages: usize,
    mut on_page: impl FnMut(&PageInfo),
) -> Result<Vec<Photo>> {
    let t = Instant::now();
    let resp = agent.get(share_link).call().map_err(http_err).context("album link")?;
    let final_url = resp.get_url().to_string();
    check_landing(&final_url)?;
    let (album_id, key) = album::ids_from_share_url(&final_url)?;
    // stream the ~1.4 MB page through the same scanner the device uses
    let mut scanner = album::ShareScanner::new();
    let mut reader = resp.into_reader();
    let mut buf = [0u8; 4096];
    let mut page_bytes = 0;
    loop {
        let n = reader.read(&mut buf).context("reading share page")?;
        if n == 0 {
            break;
        }
        page_bytes += n;
        scanner.feed(&buf[..n]);
    }
    let first = scanner.finish().context(
        "share page has no album data: link sharing off, album empty, or Google changed the page \
         (run `make pc-check ARGS=--save-page` and look at out/album.html)",
    )?;

    let mut photos = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |page: album::AlbumPage, n: usize, bytes: usize, took: Duration| {
        let before = photos.len();
        for p in page.photos {
            if seen.insert(album::url_key(&p.url)) {
                photos.push(p);
            }
        }
        on_page(&PageInfo { page: n, added: photos.len() - before, bytes, took, total: page.total });
        page.next_token
    };

    let mut token = add(first, 1, page_bytes, t.elapsed());
    let mut n = 1;
    while let Some(tok) = token {
        if n >= max_pages {
            break;
        }
        n += 1;
        let t = Instant::now();
        let body = agent
            .post(album::RPC_URL)
            .set("Content-Type", "application/x-www-form-urlencoded;charset=UTF-8")
            .send_string(&album::rpc_body(&album_id, &key, &tok))
            .map_err(http_err)
            .with_context(|| format!("album page {n}"))?
            .into_string()?;
        let page = album::parse_rpc_response(&body).with_context(|| format!("album page {n}"))?;
        token = add(page, n, body.len(), t.elapsed());
    }
    Ok(photos)
}

/// The share link must land on photos.google.com, not on a consent or login page.
fn check_landing(final_url: &str) -> Result<()> {
    let host = final_url.split('/').nth(2).unwrap_or("");
    match host {
        "photos.google.com" => Ok(()),
        h if h.starts_with("consent.") => bail!(
            "Google redirected to a cookie-consent page ({h}); the device cannot click through it"
        ),
        h if h.starts_with("accounts.") => {
            bail!("Google asked for a login ({h}): the album is not shared as 'anyone with the link'")
        }
        h => bail!("unexpected host {h}; is this a Google Photos album share link?"),
    }
}

pub struct Jpeg {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub progressive: bool,
}

/// Downloads `photo` already converted by Google to a `w`×`h` JPEG.
pub fn download_photo(agent: &ureq::Agent, photo: &Photo, w: u32, h: u32) -> Result<Jpeg> {
    let resp = agent
        .get(&photo.sized_url(w, h))
        .set("Accept", "image/jpeg")
        .call()
        .map_err(http_err)
        .context("download photo")?;
    let content_type = resp.header("content-type").unwrap_or("?").to_string();
    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes)?;
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        bail!("not a JPEG ({content_type}, first bytes {:02X?})", &bytes[..bytes.len().min(4)]);
    }
    let progressive = sof_marker(&bytes).ok_or_else(|| anyhow!("JPEG has no SOF marker"))? == 0xC2;
    Ok(Jpeg { bytes, content_type, progressive })
}

/// Walk JPEG segments until the first start-of-frame marker.
fn sof_marker(b: &[u8]) -> Option<u8> {
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let m = b[i + 1];
        if matches!(m, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            return Some(m);
        }
        let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
        i += 2 + len;
    }
    None
}

pub fn http_err(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, r) => {
            anyhow::Error::new(HttpStatus(code)).context(format!("from {}", redact_key(r.get_url())))
        }
        other => anyhow!(other),
    }
}

/// True if the error chain contains an HTTP status in `codes`.
pub fn is_status(e: &anyhow::Error, codes: &[u16]) -> bool {
    e.downcast_ref::<HttpStatus>().is_some_and(|s| codes.contains(&s.0))
}

/// Hide the album key / calendar secret when printing URLs.
pub fn redact_key(url: &str) -> String {
    if let Some(i) = url.find("key=") {
        return format!("{}key=<hidden>", &url[..i]);
    }
    if let Some(i) = url.find("/private-") {
        let end = url[i + 1..].find('/').map_or(url.len(), |j| i + 1 + j);
        return format!("{}/private-<hidden>{}", &url[..i], &url[end..]);
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_hidden_in_printed_urls() {
        assert_eq!(redact_key("https://photos.google.com/share/X?key=SECRET"), "https://photos.google.com/share/X?key=<hidden>");
        assert_eq!(
            redact_key("https://calendar.google.com/calendar/ical/fam%40group/private-abc123/basic.ics"),
            "https://calendar.google.com/calendar/ical/fam%40group/private-<hidden>/basic.ics"
        );
        assert_eq!(redact_key("https://example.com/a"), "https://example.com/a");
    }
}
