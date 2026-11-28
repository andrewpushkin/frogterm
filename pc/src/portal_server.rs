//! The emulator's web server for the settings page: plain `std::net`, one
//! request at a time, every request answered by `frogterm_core::portal`
//! (the same page the device serves). A good save rewrites the config file.

use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, UdpSocket};
use std::ops::RangeInclusive;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use frogterm_core::{firmware, portal};

/// Requests (headers and body) larger than this are refused.
const MAX_REQUEST: usize = 16 * 1024;
/// The emulator's side of the page: no Wi-Fi needed, no firmware of its own.
const HOST: portal::Host = portal::Host { wifi_needed: false, firmware: "emulator" };
/// A client that sends nothing for this long is dropped.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// What `serve_once` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    /// Nobody came within the wait.
    Nothing,
    /// A request was answered.
    Request,
    /// A request saved new settings to the file.
    Saved,
}

pub struct Server {
    listener: TcpListener,
}

impl Server {
    /// Listens on all interfaces, on the first free port of `ports`
    /// (`0..=0`: any free port).
    pub fn bind(ports: RangeInclusive<u16>) -> Result<Server> {
        let (first, last) = (*ports.start(), *ports.end());
        for port in ports {
            if let Ok(listener) = TcpListener::bind(("0.0.0.0", port)) {
                listener.set_nonblocking(true)?;
                return Ok(Server { listener });
            }
        }
        bail!("no free port in {first}..={last} for the settings page")
    }

    pub fn port(&self) -> u16 {
        self.listener.local_addr().map_or(0, |a| a.port())
    }

    /// Answers at most one request that comes within `wait`.
    pub fn serve_once(&mut self, config_path: &Path, wait: Duration) -> Result<Served> {
        let until = Instant::now() + wait;
        let stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= until {
                        return Ok(Served::Nothing);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e).context("settings page: accept"),
            }
        };
        match answer(stream, config_path) {
            Ok(saved) => Ok(if saved { Served::Saved } else { Served::Request }),
            Err(e) => {
                log::warn!("settings page: {e:#}");
                Ok(Served::Request)
            }
        }
    }
}

/// Reads one request, answers it; true if new settings were saved.
fn answer(mut stream: TcpStream, config_path: &Path) -> Result<bool> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    // the headers
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_REQUEST {
            return refuse(stream, 413, "Request Too Large");
        }
        let n = stream.read(&mut chunk).context("read the request")?;
        if n == 0 {
            bail!("the browser left before sending a request");
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut words = head.split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let length: usize = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    let upload = method == "POST" && target.split('?').next() == Some("/update");
    // a firmware upload may be as large as a firmware; everything else is a form
    let limit = if upload { head_end + firmware::MAX_IMAGE } else { MAX_REQUEST };
    if head_end + length > limit {
        return refuse(stream, 413, "Request Too Large");
    }
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk).context("read the form")?;
        if n == 0 {
            bail!("the form was cut off");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = &buf[head_end..head_end + length];
    if upload {
        // checked as the device checks it; the PC has no firmware to replace
        let (status, text) = match firmware::check_image(body) {
            Ok(v) => (200, format!("frogterm {v}: a good firmware for the device. The emulator installs nothing.")),
            Err(e) => (400, format!("Not installed: {e:#}.")),
        };
        log::info!("settings page: POST /update ({} KB) -> {status}", body.len() / 1024);
        send(&mut stream, status, "text/plain; charset=utf-8", text.as_bytes())?;
        return Ok(false);
    }
    let current = fs::read_to_string(config_path).unwrap_or_default();
    let (response, saved) = portal::handle(method, target, body, &current, &HOST);
    log::info!("settings page: {method} {} -> {}", target.split('?').next().unwrap_or(""), response.status);
    if let Some(text) = &saved {
        fs::write(config_path, text).with_context(|| format!("save {}", config_path.display()))?;
        log::info!("settings saved to {}", config_path.display());
    }
    send(&mut stream, response.status, response.content_type, response.body.as_bytes())?;
    Ok(saved.is_some())
}

/// Answers with an error and drops the rest of the request.
fn refuse(mut stream: TcpStream, status: u16, text: &str) -> Result<bool> {
    send(&mut stream, status, "text/plain; charset=utf-8", text.as_bytes())?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    // read what the browser still sends, so closing does not reset the answer
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut sink = [0u8; 4096];
    let mut drained = 0;
    while drained < 1 << 20 {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
    log::warn!("settings page: refused ({status} {text})");
    Ok(false)
}

fn send(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// This PC's address on its network (no packet is sent).
pub fn lan_address() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    Some(s.local_addr().ok()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    const FILE: &str = "# mine\nalbum_url = \"https://photos.app.goo.gl/abc\"\ncity = \"Tel Aviv\"\n";

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("frogterm-portal-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, FILE).unwrap();
        path
    }

    /// Sends `request` raw; returns the whole answer.
    fn ask(port: u16, request: Vec<u8>) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let _ = s.write_all(&request);
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    /// One request in a thread while the server answers it.
    fn exchange(path: &Path, request: Vec<u8>) -> (String, Served) {
        let mut server = Server::bind(0..=0).unwrap();
        let port = server.port();
        let client = thread::spawn(move || ask(port, request));
        let served = server.serve_once(path, Duration::from_secs(5)).unwrap();
        (client.join().unwrap(), served)
    }

    #[test]
    fn get_returns_the_page() {
        let path = scratch("get");
        let (answer, served) = exchange(&path, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n".to_vec());
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        assert!(answer.contains("Content-Type: text/html; charset=utf-8\r\n"));
        assert!(answer.contains("value=\"Tel Aviv\""));
        assert_eq!(served, Served::Request);
    }

    #[test]
    fn post_save_writes_the_config() {
        let path = scratch("post");
        let body = "album_url=https%3A%2F%2Fphotos.app.goo.gl%2Fabc&city=Haifa";
        let req = format!(
            "POST /save HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (answer, served) = exchange(&path, req.into_bytes());
        assert!(answer.contains("Saved"), "{answer}");
        assert_eq!(served, Served::Saved);
        assert_eq!(fs::read_to_string(&path).unwrap(), FILE.replace("Tel Aviv", "Haifa"));
    }

    /// A frogterm app image of `len` bytes (larger than the form limit).
    fn firmware(len: usize, full_image: bool) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[0] = 0xE9;
        b[12] = 9;
        if !full_image {
            b[32..36].copy_from_slice(&0xABCD_5432u32.to_le_bytes());
        }
        let tag = b"<frogterm-version:85>";
        b[len - 100..len - 100 + tag.len()].copy_from_slice(tag);
        b
    }

    fn upload(path: &Path, body: Vec<u8>) -> (String, Served) {
        let mut req = format!("POST /update HTTP/1.1\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n", body.len())
            .into_bytes();
        req.extend(body);
        exchange(path, req)
    }

    #[test]
    fn an_uploaded_firmware_is_checked_but_not_installed() {
        let path = scratch("update");
        let (answer, served) = upload(&path, firmware(200 * 1024, false));
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(answer.contains("frogterm 85") && answer.contains("installs nothing"), "{answer}");
        assert_eq!(served, Served::Request);
        assert_eq!(fs::read_to_string(&path).unwrap(), FILE, "the settings untouched");
    }

    #[test]
    fn a_wrong_firmware_file_is_refused_with_why() {
        let path = scratch("update-full");
        let (answer, _) = upload(&path, firmware(50 * 1024, true));
        assert!(answer.starts_with("HTTP/1.1 400") && answer.contains("firmware-full.bin"), "{answer}");
    }

    #[test]
    fn a_missing_file_is_an_empty_one() {
        let path = scratch("missing").with_file_name("none.toml");
        let (answer, _) = exchange(&path, b"GET / HTTP/1.1\r\n\r\n".to_vec());
        assert!(answer.starts_with("HTTP/1.1 200 OK") && answer.contains("not complete"), "{answer}");
    }

    #[test]
    fn oversized_request_is_refused_and_server_keeps_serving() {
        let path = scratch("big");
        let mut server = Server::bind(0..=0).unwrap();
        let port = server.port();
        let big = format!("POST /save HTTP/1.1\r\nContent-Length: {}\r\n\r\n{}", MAX_REQUEST * 2, "a".repeat(MAX_REQUEST * 2));
        let client = thread::spawn(move || ask(port, big.into_bytes()));
        assert_eq!(server.serve_once(&path, Duration::from_secs(5)).unwrap(), Served::Request);
        assert!(client.join().unwrap().starts_with("HTTP/1.1 413"));
        assert_eq!(fs::read_to_string(&path).unwrap(), FILE, "nothing saved");
        let client = thread::spawn(move || ask(port, b"GET / HTTP/1.1\r\n\r\n".to_vec()));
        server.serve_once(&path, Duration::from_secs(5)).unwrap();
        assert!(client.join().unwrap().starts_with("HTTP/1.1 200"), "still serving");
    }

    #[test]
    fn a_half_request_is_dropped_and_server_keeps_serving() {
        let path = scratch("half");
        let mut server = Server::bind(0..=0).unwrap();
        let port = server.port();
        let slow = thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"GET / HTT").unwrap();
            thread::sleep(READ_TIMEOUT + Duration::from_secs(1));
        });
        let t = Instant::now();
        assert_eq!(server.serve_once(&path, Duration::from_secs(5)).unwrap(), Served::Request);
        assert!(t.elapsed() < READ_TIMEOUT + Duration::from_secs(1));
        let client = thread::spawn(move || ask(port, b"GET / HTTP/1.1\r\n\r\n".to_vec()));
        server.serve_once(&path, Duration::from_secs(5)).unwrap();
        assert!(client.join().unwrap().starts_with("HTTP/1.1 200"));
        slow.join().unwrap();
    }

    #[test]
    fn nothing_within_wait_is_nothing() {
        let path = scratch("idle");
        let mut server = Server::bind(0..=0).unwrap();
        let t = Instant::now();
        assert_eq!(server.serve_once(&path, Duration::from_millis(200)).unwrap(), Served::Nothing);
        assert!(t.elapsed() >= Duration::from_millis(200));
    }

    #[test]
    fn a_taken_port_moves_to_the_next() {
        let first = Server::bind(0..=0).unwrap();
        let taken = first.port();
        if let Some(next) = taken.checked_add(1) {
            if let Ok(second) = Server::bind(taken..=next) {
                assert_eq!(second.port(), next);
            }
        }
        assert!(Server::bind(taken..=taken).is_err());
    }
}
