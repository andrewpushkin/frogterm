//! Wi-Fi, SNTP and streaming HTTPS (TLS via the Mozilla CA bundle).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection, FollowRedirectsPolicy};
use esp_idf_svc::http::Method;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::{EspSntp, SyncStatus};
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi, ScanMethod};

const USER_AGENT: &str = "frogterm/0.1";
const MAX_REDIRECTS: usize = 10;

pub struct Net {
    wifi: BlockingWifi<EspWifi<'static>>,
    sntp: Option<EspSntp<'static>>,
    /// Access point we joined (BSSID, channel): remembered for a faster next join.
    pub joined: Option<([u8; 6], u8)>,
}

/// HTTP status and the URL after redirects.
pub struct Response {
    pub status: u16,
    pub final_url: String,
}

impl Net {
    pub fn connect(
        modem: Modem<'static>,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        ssid: &str,
        password: &str,
        last_ap: Option<([u8; 6], u8)>,
    ) -> Result<Net> {
        let t = Instant::now();
        let mut wifi = BlockingWifi::wrap(EspWifi::new(modem, sysloop.clone(), Some(nvs))?, sysloop)?;
        let config = |ap: Option<([u8; 6], u8)>| -> Result<Configuration> {
            Ok(Configuration::Client(ClientConfiguration {
                ssid: ssid.try_into().map_err(|_| anyhow!("wifi_ssid too long"))?,
                password: password.try_into().map_err(|_| anyhow!("wifi_password too long"))?,
                auth_method: if password.is_empty() { AuthMethod::None } else { AuthMethod::WPA2Personal },
                // known channel: probe only that channel. (Giving the BSSID too
                // makes the driver skip the probe; the router then ignores the
                // auth request: "auth -> init (0x200)".)
                channel: ap.map(|a| a.1),
                // the default (complete scan) scans every channel even when the
                // channel is known; fast scan joins the first match
                scan_method: if ap.is_some() { ScanMethod::FastScan } else { ScanMethod::default() },
                ..Default::default()
            }))
        };
        log::info!("joining Wi-Fi {}", if last_ap.is_some() { "on the remembered channel" } else { "with a full scan" });
        wifi.set_configuration(&config(last_ap)?)?;
        allow_wpa3_h2e()?;
        wifi.start()?;
        if let Err(e) = wifi.connect() {
            if last_ap.is_none() {
                log_visible_aps(&mut wifi, ssid);
                return Err(e).context("Wi-Fi connect");
            }
            log::warn!("quick join failed ({e}), scanning all channels");
            wifi.set_configuration(&config(None)?)?;
            allow_wpa3_h2e()?;
            if let Err(e) = wifi.connect() {
                log_visible_aps(&mut wifi, ssid);
                return Err(e).context("Wi-Fi connect");
            }
        }
        wifi.wait_netif_up().context("Wi-Fi DHCP")?;
        let joined = wifi.wifi().get_ap_info().ok().map(|ap| (ap.bssid, ap.channel));
        log::info!("Wi-Fi up in {:.1} s", t.elapsed().as_secs_f32());
        // time sync runs in the background; see `synced_time`
        let sntp = EspSntp::new_default().map_err(|e| log::warn!("SNTP: {e}")).ok();
        Ok(Net { wifi, sntp, joined })
    }

    /// Unix time once NTP has set the clock, waiting at most `wait`.
    pub fn synced_time(&self, wait: Duration) -> Option<i64> {
        let sntp = self.sntp.as_ref()?;
        let deadline = Instant::now() + wait;
        while sntp.get_sync_status() != SyncStatus::Completed {
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
    }

    pub fn get(&mut self, url: &str, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<Response> {
        get(url, sink)
    }

    pub fn post_form(&mut self, url: &str, body: &str, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<Response> {
        request(Method::Post, url, Some(body), sink)
    }

    pub fn stop(mut self) {
        drop(self.sntp.take());
        let _ = self.wifi.disconnect();
        let _ = self.wifi.stop();
    }
}

/// WPA3 routers may demand the hash-to-element (H2E) SAE handshake.
/// esp-idf-svc zero-fills the station config, i.e. `WPA3_SAE_PWE_UNSPECIFIED`,
/// so set "both" (H2E or hunt-and-peck, as the router offers) on the raw config.
fn allow_wpa3_h2e() -> Result<()> {
    use esp_idf_svc::sys::{esp, esp_wifi_get_config, esp_wifi_set_config, wifi_config_t, wifi_interface_t_WIFI_IF_STA};
    let mut c: wifi_config_t = Default::default();
    // SAFETY: the driver fills/reads a plain C struct; the STA variant is the one in use
    unsafe {
        esp!(esp_wifi_get_config(wifi_interface_t_WIFI_IF_STA, &mut c))?;
        c.sta.sae_pwe_h2e = esp_idf_svc::sys::wifi_sae_pwe_method_t_WPA3_SAE_PWE_BOTH;
        esp!(esp_wifi_set_config(wifi_interface_t_WIFI_IF_STA, &mut c))?;
    }
    Ok(())
}

/// After a failed join: what the radio can see, to tell "no signal" from
/// "router found but refused" (e.g. a security mode the device lacks).
fn log_visible_aps(wifi: &mut BlockingWifi<EspWifi<'static>>, ssid: &str) {
    match wifi.scan() {
        Ok(mut aps) => {
            aps.sort_by_key(|a| std::cmp::Reverse(a.signal_strength));
            let ours = aps.iter().filter(|a| a.ssid.as_str() == ssid).count();
            log::warn!("Wi-Fi scan: {} networks, {ours} named {ssid:?}", aps.len());
            for a in aps.iter().take(12) {
                log::warn!(
                    "  {:<24} ch {:>2} {:>4} dBm {:?}{}",
                    a.ssid.as_str(),
                    a.channel,
                    a.signal_strength,
                    a.auth_method,
                    if a.ssid.as_str() == ssid { "  <- ours" } else { "" }
                );
            }
        }
        Err(e) => log::warn!("Wi-Fi scan failed: {e}"),
    }
}

/// GET with redirects; the body is streamed to `sink`. Needs Wi-Fi up;
/// safe to call from several threads at once (one connection each).
pub fn get(url: &str, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<Response> {
    request(Method::Get, url, None, sink)
}

/// GET into memory, failing on non-2xx.
pub fn get_bytes(url: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let r = get(url, &mut |c| {
        out.extend_from_slice(c);
        Ok(())
    })?;
    if !(200..300).contains(&r.status) {
        bail!(HttpStatus(r.status));
    }
    Ok(out)
}

fn request(method: Method, url: &str, body: Option<&str>, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<Response> {
    {
        let mut url = url.to_string();
        for _ in 0..=MAX_REDIRECTS {
            let mut conn = EspHttpConnection::new(&HttpConfig {
                buffer_size: Some(4096),
                buffer_size_tx: Some(2048),
                timeout: Some(Duration::from_secs(30)),
                follow_redirects_policy: FollowRedirectsPolicy::FollowNone,
                crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
                ..Default::default()
            })?;
            let len = body.map(|b| b.len().to_string()).unwrap_or_default();
            let mut headers = vec![("User-Agent", USER_AGENT), ("Accept", "*/*")];
            if body.is_some() {
                headers.push(("Content-Type", "application/x-www-form-urlencoded;charset=UTF-8"));
                headers.push(("Content-Length", &len));
            }
            conn.initiate_request(method, &url, &headers)?;
            if let Some(b) = body {
                let mut rest = b.as_bytes();
                while !rest.is_empty() {
                    let n = conn.write(rest)?;
                    rest = &rest[n..];
                }
            }
            conn.initiate_response()?;
            let status = conn.status();
            if (300..400).contains(&status) {
                let location = conn.header("Location").ok_or_else(|| anyhow!("redirect without Location"))?.to_string();
                url = if location.starts_with("http") { location } else { join(&url, &location) };
                continue;
            }
            let mut buf = vec![0u8; 4096];
            loop {
                let n = conn.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                sink(&buf[..n])?;
            }
            return Ok(Response { status, final_url: url });
        }
        bail!("too many redirects")
    }
}

/// Absolute URL for a relative `Location`.
fn join(base: &str, location: &str) -> String {
    let origin_end = base.find("://").map(|i| i + 3).and_then(|i| base[i..].find('/').map(|j| i + j)).unwrap_or(base.len());
    format!("{}{}", &base[..origin_end], if location.starts_with('/') { location.to_string() } else { format!("/{location}") })
}

/// HTTP status error, so callers can tell "photo gone" (403/404) apart.
#[derive(Debug)]
pub struct HttpStatus(pub u16);

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}", self.0)
    }
}

impl std::error::Error for HttpStatus {}
