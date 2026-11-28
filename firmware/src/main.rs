//! reTerminal E1002 photo + weather + calendar frame.

mod app;
mod board;
mod epd;
mod net;
mod ota;
mod portal;
mod rtc;
mod settings;
mod sleep;
mod store;

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    stacks_in_psram();
    app::run()
}

/// New threads (and the threads they start, e.g. jpeg-decoder's workers) get
/// their stacks from PSRAM; internal RAM stays free for Wi-Fi and drivers.
/// Such threads must not touch flash (see `spawn_with_internal_stack`).
fn stacks_in_psram() {
    set_stack_caps(esp_idf_svc::sys::MALLOC_CAP_SPIRAM | esp_idf_svc::sys::MALLOC_CAP_8BIT);
}

fn set_stack_caps(caps: u32) {
    use esp_idf_svc::sys;
    unsafe {
        let mut cfg = sys::esp_pthread_get_default_config();
        cfg.stack_alloc_caps = caps;
        cfg.inherit_cfg = true;
        if sys::esp_pthread_set_cfg(&cfg) != sys::ESP_OK {
            log::warn!("could not set thread stack memory");
        }
    }
}

/// A thread with its stack in internal RAM: needed for flash access, which
/// turns the cache (and so PSRAM) off while it runs.
pub fn spawn_with_internal_stack<T: Send + 'static>(
    name: &str,
    stack: usize,
    f: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<T>> {
    use esp_idf_svc::sys;
    set_stack_caps(sys::MALLOC_CAP_INTERNAL | sys::MALLOC_CAP_8BIT);
    let handle = std::thread::Builder::new().name(name.into()).stack_size(stack).spawn(f);
    stacks_in_psram();
    handle
}
