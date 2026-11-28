//! Settings mode's network and web server: the device's own open Wi-Fi
//! (`frogterm-setup`, 192.168.4.1) or the home Wi-Fi, and the settings page
//! of `frogterm_core::portal` on port 80, with NVS as its settings file.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::http::server::{Configuration as ServerConfig, EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use esp_idf_svc::wifi::{AccessPointConfiguration, AuthMethod, BlockingWifi, Configuration, EspWifi};
use frogterm_core::app::{self, Via};
use frogterm_core::config::Config;
use frogterm_core::portal;

use crate::net::Net;

/// Requests (the form) larger than this are refused.
const MAX_BODY: usize = 16 * 1024;
/// The device's side of the page: it needs its Wi-Fi settings.
const HOST: portal::Host = portal::Host { wifi_needed: true, firmware: env!("FT_VERSION") };

enum Network {
    AccessPoint(BlockingWifi<EspWifi<'static>>),
    Home(Net),
}

/// Settings mode's network and page, up while this lives.
pub struct Portal {
    // the server goes before the network
    _server: EspHttpServer<'static>,
    network: Network,
    /// Milliseconds since boot (`now_ms`) of the last page request (or of the start).
    last_request: Arc<AtomicU32>,
    /// A new firmware was installed: it boots at the next restart.
    installed: Arc<AtomicBool>,
    /// The Wi-Fi name and the page's address, for the screen.
    pub network_name: String,
    pub address: String,
}

impl Portal {
    /// The network `via` (the access point if there are no usable settings
    /// to join the home Wi-Fi with) and the page on it.
    pub fn open(
        via: Via,
        modem: Modem<'static>,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        cfg: Option<&Config>,
        last_ap: Option<([u8; 6], u8)>,
    ) -> Result<Portal> {
        let home = cfg.and_then(|c| c.wifi_ssid.as_deref().map(|s| (s, c.wifi_password.as_deref().unwrap_or(""))));
        let (network, network_name, address) = match (via, home) {
            (Via::HomeWifi, Some((ssid, password))) => {
                let net = Net::connect(modem, sysloop, nvs.clone(), ssid, password, last_ap)?;
                let ip = net.ip().ok_or_else(|| anyhow!("no address on the home Wi-Fi"))?;
                (Network::Home(net), ssid.to_string(), format!("http://{ip}/"))
            }
            _ => (Network::AccessPoint(access_point(modem, sysloop, nvs.clone())?), app::AP_SSID.to_string(), app::AP_ADDRESS.to_string()),
        };
        // a server answers at once: no Wi-Fi power saving (the radio would
        // doze between beacons and miss ARP requests and connections)
        // SAFETY: plain ESP-IDF call on the running Wi-Fi
        if unsafe { esp_idf_svc::sys::esp_wifi_set_ps(esp_idf_svc::sys::wifi_ps_type_t_WIFI_PS_NONE) } != esp_idf_svc::sys::ESP_OK {
            log::warn!("settings mode: Wi-Fi power saving stays on");
        }
        let last_request = Arc::new(AtomicU32::new(now_ms()));
        let installed = Arc::new(AtomicBool::new(false));
        let server = serve(nvs, last_request.clone(), installed.clone())?;
        log::info!("settings page at {address} (Wi-Fi {network_name})");
        Ok(Portal { _server: server, network, last_request, installed, network_name, address })
    }

    /// Time since the last page request (or since the start).
    pub fn idle(&self) -> Duration {
        Duration::from_millis(u64::from(now_ms().wrapping_sub(self.last_request.load(Ordering::Relaxed))))
    }

    /// A new firmware was installed from the page (it boots at the next restart).
    pub fn installed(&self) -> bool {
        self.installed.load(Ordering::Relaxed)
    }

    /// The server and the Wi-Fi off.
    pub fn close(self) {
        let Portal { _server, network, .. } = self;
        drop(_server);
        match network {
            Network::AccessPoint(mut wifi) => {
                let _ = wifi.stop();
            }
            Network::Home(net) => net.stop(),
        }
        log::info!("settings page closed");
    }
}

/// Milliseconds since boot, wrapping (settings mode lasts an hour at most).
fn now_ms() -> u32 {
    crate::sleep::ms() as u32
}

/// The device's own open network.
fn access_point(modem: Modem<'static>, sysloop: EspSystemEventLoop, nvs: EspDefaultNvsPartition) -> Result<BlockingWifi<EspWifi<'static>>> {
    let mut wifi = BlockingWifi::wrap(EspWifi::new(modem, sysloop.clone(), Some(nvs))?, sysloop)?;
    wifi.set_configuration(&Configuration::AccessPoint(AccessPointConfiguration {
        ssid: app::AP_SSID.try_into().map_err(|_| anyhow!("access point name too long"))?,
        auth_method: AuthMethod::None,
        channel: 1,
        max_connections: 4,
        ..Default::default()
    }))?;
    wifi.start().context("start the access point")?;
    wifi.wait_netif_up().context("access point address")?;
    Ok(wifi)
}

/// The page on port 80: a firmware upload goes to `ota::install`, every
/// other request to `portal::handle`.
fn serve(nvs: EspDefaultNvsPartition, last_request: Arc<AtomicU32>, installed: Arc<AtomicBool>) -> Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&ServerConfig { stack_size: 16 * 1024, uri_match_wildcard: true, ..Default::default() })?;
    {
        // registered first: handlers are tried in order, "/*" would take it
        let last_request = last_request.clone();
        server.fn_handler("/update", Method::Post, move |req| {
            last_request.store(now_ms(), Ordering::Relaxed);
            update(req, &last_request, &installed).map_err(|e| log::warn!("firmware upload: {e:#}"))
        })?;
    }
    for method in [Method::Get, Method::Post] {
        let (nvs, last_request) = (nvs.clone(), last_request.clone());
        server.fn_handler("/*", method, move |req| {
            last_request.store(now_ms(), Ordering::Relaxed);
            answer(req, &nvs).map_err(|e| log::warn!("settings page: {e:#}"))
        })?;
    }
    Ok(server)
}

/// A firmware upload: installed if it is a whole frogterm firmware.
fn update(mut req: Request<&mut EspHttpConnection<'_>>, last_request: &AtomicU32, installed: &AtomicBool) -> Result<()> {
    let len: usize = req.header("Content-Length").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    log::info!("firmware upload: {} KB", len / 1024);
    let result = crate::ota::install(len, &mut |buf| Ok(req.read(buf)?), &mut || last_request.store(now_ms(), Ordering::Relaxed));
    let (status, text) = match result {
        Ok(version) => {
            log::info!("firmware {version} installed, it boots at the next restart");
            installed.store(true, Ordering::Relaxed);
            (200, format!("Installed frogterm {version}. The frame restarts with it now (the panel updates when it has synced)."))
        }
        Err(e) => {
            log::warn!("firmware upload refused: {e:#}");
            (400, format!("Not installed: {e:#}. The running firmware stays."))
        }
    };
    req.into_response(status, None, &[("Content-Type", "text/plain; charset=utf-8"), ("Cache-Control", "no-store")])?
        .write_all(text.as_bytes())?;
    Ok(())
}

/// One request; a good save is written to NVS.
fn answer(mut req: Request<&mut EspHttpConnection<'_>>, nvs_part: &EspDefaultNvsPartition) -> Result<()> {
    let method = match req.method() {
        Method::Get => "GET",
        Method::Post => "POST",
        _ => "OTHER",
    };
    let uri = req.uri().to_string();
    let path = uri.split('?').next().unwrap_or("").to_string();
    let len: usize = req.header("Content-Length").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        log::warn!("settings page: {method} {path} refused ({len} bytes)");
        req.into_response(413, Some("Payload Too Large"), &[("Content-Type", "text/plain; charset=utf-8")])?
            .write_all(b"Request Too Large")?;
        return Ok(());
    }
    let mut body = vec![0u8; len];
    let mut filled = 0;
    while filled < len {
        let n = req.read(&mut body[filled..])?;
        if n == 0 {
            anyhow::bail!("the form was cut off");
        }
        filled += n;
    }
    let nvs = EspNvs::new(nvs_part.clone(), "frogterm", true)?;
    let current = crate::settings::load_text(&nvs).unwrap_or_default();
    let (mut response, saved) = portal::handle(method, &uri, &body, &current, &HOST);
    if let Some(text) = saved {
        match crate::settings::save_text(&nvs, &text) {
            Ok(()) => log::info!("settings saved"),
            Err(e) => {
                log::warn!("{e:#}");
                response = portal::store_failed(&text, &format!("{:#}", e.root_cause()), &HOST);
            }
        }
    }
    log::info!("settings page: {method} {path} -> {}", response.status);
    req.into_response(response.status, None, &[("Content-Type", response.content_type), ("Cache-Control", "no-store")])?
        .write_all(response.body.as_bytes())?;
    Ok(())
}
