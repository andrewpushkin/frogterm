# reTerminal E1002 Firmware Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An ESP32-S3 firmware that does on the reTerminal E1002 exactly what `make emulator` does on the PC, wakes at 06:00 or on the green button, beeps, and deep-sleeps in between.

**Architecture:** All logic that can be tested on the host stays in `core/` (already used by the emulator) and grows the few pure pieces the device still needs (panel protocol table, BCD clock, schedule, beeps, battery, JPEG, streaming share-page reader, photo list). A new `firmware/` crate (outside the cargo workspace, Xtensa target) only wires `core` to hardware through `esp-idf-svc`: SPI panel, I2C RTC, LEDC buzzer, ADC, Wi-Fi/SNTP/HTTPS, flash partitions, deep sleep.

**Tech Stack:** Rust; `esp-idf-svc 0.54` / `esp-idf-hal 0.48` / `esp-idf-sys 0.39` / `embuild 0.33` on ESP-IDF v5.x; `espup 0.19` (Xtensa toolchain), `espflash 4.7`, `ldproxy 0.3`; `jpeg-decoder 0.3` (pure Rust) in `core`; Docker `ubuntu:26.04`.

**Spec:** `docs/superpowers/specs/2026-11-25-reterminal-photo-frame-design.md`

## Global Constraints

- Everything builds and runs inside the Docker image; host needs only Docker, make, usbipd-win. Base image `ubuntu:26.04`.
- `core/` may only depend on small embedded-friendly crates; heavy crates stay in `pc/`. `core` must keep building for both host and `xtensa-esp32s3-espidf`.
- Secrets (album link, calendar links, Wi-Fi) only in gitignored `config.toml`; never in git, logs show links redacted.
- Overlay boxes together ≤ 20 % of the screen (existing test); default `overlay_opacity = 50`.
- Panel: SPI2, SCK 7, MOSI 9, CS 10, DC 11, RST 12, BUSY 13 (LOW = busy), 4 MHz. Buzzer GPIO45. LED GPIO6 (active low). KEY0 GPIO3 (active low, wake). RTC PCF8563 @0x51 on I2C SDA 19 / SCL 20. Battery ADC GPIO1, enable GPIO21 (HIGH), divider ×2. SD_EN GPIO16 low, SD_CS GPIO14 high.
- Time zone Israel via `core::tz`; daily update at `update_hour` (default 6) local.
- Commit after every green step (owner's rule).

## Review Focus

1. **Wi-Fi down at 06:00** → screen untouched, error beep, retry after 30 min up to 3 times, then wait for the next 06:00 (never stay awake). Test: Task 5 `retry_after_failure`.
2. **RTC invalid (first flash, flat coin cell) and SNTP also fails** → no tight wake loop; retry in 1 h. Test: Task 5 `unknown_time_retries_hourly`.
3. **Green button held (or stuck) when going to sleep** → `ext1` would wake immediately forever; must not arm the button wake while it is held. Test: Task 5 `held_button_is_not_armed`.
4. **Album refresh fails half-way (e.g. page 17 of 29)** → keep the old list if there is one, otherwise use the pages read so far. Test: Task 7 `partial_refresh_policy`.
5. **Battery nearly empty** → an e-paper refresh at brown-out voltage can leave a half-drawn screen; below 3.45 V skip the update, error beep, sleep to next 06:00. Test: Task 4 `too_low_to_update`.

---

### Task 1: ESP toolchain in the Docker image

**Files:**
- Modify: `Dockerfile`, `Makefile`, `.gitignore`

**Interfaces:**
- Produces: image `frogterm-dev` with `cargo +esp`, target `xtensa-esp32s3-espidf`, `espflash`, `ldproxy`, ESP-IDF installed at `/opt/esp-idf` (tools in `/opt/espressif`), and env so `esp-idf-sys` uses it (`ESP_IDF_TOOLS_INSTALL_DIR=fromenv`, `IDF_PATH=/opt/esp-idf`, tools exported via an entrypoint that sources `/opt/esp-idf/export.sh`).

- [ ] **Step 1:** Extend `Dockerfile`: apt packages `git python3 python3-venv python3-pip cmake ninja-build pkg-config libudev-dev libusb-1.0-0 flex bison gperf ccache dfu-util wget xz-utils unzip`; `cargo install espup@0.19.0 espflash@4.7.0 ldproxy@0.3.5 --locked`; `espup install --targets esp32s3 --export-file /opt/export-esp.sh`; clone ESP-IDF at the tag that `esp-idf-sys 0.39` documents as its default `ESP_IDF_VERSION` (ARG `ESP_IDF_VERSION`, `--depth 1 --recursive --shallow-submodules`) into `/opt/esp-idf`, run `IDF_TOOLS_PATH=/opt/espressif /opt/esp-idf/install.sh esp32s3`; `chmod -R a+rX /opt`; `ENTRYPOINT ["/opt/entrypoint.sh"]` which sources `/opt/export-esp.sh` and `/opt/esp-idf/export.sh` (quietly) then `exec "$@"`. Keep the stable host toolchain as default so `make tests`/`emulator` still work.
- [ ] **Step 2:** Verify: `docker build -t frogterm-dev .` then `docker run --rm frogterm-dev sh -c 'cargo +esp --version && espflash --version && idf.py --version && rustc +esp --print target-list | grep xtensa-esp32s3-espidf'` → all print, exit 0. Then `make tests` and `make emulator` still pass.
- [ ] **Step 3:** Commit `docker: ESP32-S3 Rust toolchain, ESP-IDF, espflash`.

### Task 2: Firmware crate that boots, with config baked in

**Files:**
- Create: `firmware/Cargo.toml`, `firmware/build.rs`, `firmware/rust-toolchain.toml` (`channel = "esp"`), `firmware/.cargo/config.toml`, `firmware/sdkconfig.defaults`, `firmware/partitions.csv`, `firmware/src/main.rs`, `firmware/src/settings.rs`
- Modify: `pc/src/config.rs` (+ tests), `config.example.toml`, `Makefile`, `Cargo.toml` (workspace `exclude = ["firmware"]`)

**Interfaces:**
- Produces: `pc::config::Config` gains `wifi_ssid: Option<String>`, `wifi_password: Option<String>`, `update_hour: u8` (default 6, `0..=23`). `firmware/build.rs` includes `pc/src/config.rs` via `#[path]`, validates `config.toml`, requires Wi-Fi fields, and emits `cargo:rustc-env=FT_ALBUM_URL / FT_CITY / FT_PLACE / FT_LAT / FT_LON / FT_CALENDARS` (newline-joined) `/ FT_OPACITY / FT_WIFI_SSID / FT_WIFI_PASSWORD / FT_UPDATE_HOUR`, plus `cargo:rerun-if-changed=../config.toml`. `firmware/src/settings.rs`: `pub struct Settings { album_url: &'static str, …, calendars: Vec<&'static str> }` and `pub fn settings() -> Settings` reading `env!`.
- `make` → `build/frogterm.bin` (`espflash save-image --chip esp32s3 --flash-size 8mb --partition-table firmware/partitions.csv`).

- [ ] **Step 1: Failing tests** in `pc/src/config.rs`: `firmware_settings_parse` (`wifi_ssid = "home"`, `wifi_password = "pw"`, `update_hour = 7` → fields set), `update_hour_defaults_to_6_and_is_validated` (missing → 6; `24` → error).
- [ ] **Step 2:** `cargo test -p frogterm-pc config` → FAIL (no field).
- [ ] **Step 3:** Implement the fields/validation; run → PASS; commit `config: firmware settings`.
- [ ] **Step 4:** Create the firmware crate. `.cargo/config.toml`: `target = "xtensa-esp32s3-espidf"`, `linker = "ldproxy"`, `rustflags = ["--cfg", "espidf_time64"]`, `[unstable] build-std = ["std", "panic_abort"]`, env `MCU = "esp32s3"`, `ESP_IDF_SDKCONFIG_DEFAULTS = "sdkconfig.defaults"`. Dependencies: `esp-idf-svc = "0.54"`, `esp-idf-hal = "0.48"`, `log`, `anyhow`, `frogterm-core = { path = "../core" }`; build-deps `embuild = "0.33"`, `serde`, `toml`, `anyhow`. `sdkconfig.defaults`: `CONFIG_ESPTOOLPY_FLASHSIZE_8MB=y`, `CONFIG_PARTITION_TABLE_CUSTOM=y`, `CONFIG_PARTITION_TABLE_CUSTOM_FILENAME="partitions.csv"`, `CONFIG_SPIRAM=y`, `CONFIG_SPIRAM_MODE_OCT=y`, `CONFIG_SPIRAM_USE_MALLOC=y`, `CONFIG_ESP_MAIN_TASK_STACK_SIZE=32768`, `CONFIG_MBEDTLS_EXTERNAL_MEM_ALLOC=y`, `CONFIG_MBEDTLS_CERTIFICATE_BUNDLE=y`, `CONFIG_ESP_TASK_WDT_TIMEOUT_S=30`, `CONFIG_FREERTOS_HZ=1000`. `partitions.csv` (8 MB layout, works whether flash is 8 or 32 MB):
  ```
  nvs,     data, nvs,     0x9000,   0x6000
  phy_init,data, phy,     0xf000,   0x1000
  factory, app,  factory, 0x10000,  0x300000
  photos,  data, 0x40,    0x310000, 0x300000
  lastscr, data, 0x41,    0x610000, 0x40000
  ```
  `main.rs`: `esp_idf_svc::sys::link_patches(); EspLogger::initialize_default();` log `frogterm <version>, album <redacted>, place <place>`, then deep sleep 60 s (`esp_deep_sleep`).
- [ ] **Step 5:** `Makefile`: `all` builds the firmware in Docker (`cd firmware && cargo build --release`), then `espflash save-image` to `build/frogterm.bin`; `clean` also removes `firmware/target`. Run `make` → `build/frogterm.bin` exists (< 3 MB). Missing Wi-Fi in `config.toml` → build fails with "wifi_ssid missing in config.toml".
- [ ] **Step 6:** Commit `firmware: bootable skeleton, config baked in at build time`.

### Task 3: Panel driver and colour-bar test

**Files:**
- Create: `core/src/epd.rs` (+ tests), `firmware/src/epd.rs`
- Modify: `core/src/lib.rs`, `core/src/framebuf.rs` (+ test), `firmware/src/main.rs`, `firmware/Cargo.toml` (feature `bars`)

**Interfaces:**
- Produces (core): `pub const INIT: &[(u8, &[u8])]` — the Zephyr `display_ed2208_gca.c` table in order: `(0xAA,[49 55 20 08 09 18])`, `(0x01,[3F 00 32 2A 0E 2A])`, `(0x00,[5F 69])`, `(0x03,[00 54 00 44])`, `(0x05,[40 1F 1F 2C])`, `(0x06,[6F 1F 16 25])`, `(0x08,[6F 1F 1F 22])`, `(0x13,[00 04])`, `(0x30,[02])`, `(0x41,[00])`, `(0x50,[3F])`, `(0x60,[02 00])`, `(0x61,[03 20 01 E0])`, `(0x82,[1E])`, `(0x84,[01])`, `(0x86,[00])`, `(0xE3,[2F])`, `(0xE0,[00])`, `(0xE6,[00])`, `(0x04,[])` (power on; wait BUSY ≤ 5 s). Constants `DATA_START = 0x10`, `REFRESH = (0x12, [0x00])` (BUSY ≤ 40 s), `POWER_OFF = (0x02, [0x00])` (≤ 5 s), `DEEP_SLEEP = (0x07, [0xA5])`, `RESET_LOW_MS = 20`, `RESET_WAIT_MS = 10`, `SPI_HZ = 4_000_000`. `FrameBuffer::colour_bars() -> FrameBuffer` (six vertical bars in `Ink::ALL` order).
- Produces (firmware): `pub struct Epd<'d>` with `pub fn new(spi2, sck, mosi, cs, dc, rst, busy) -> Result<Self>` and `pub fn show(&mut self, fb: &FrameBuffer) -> Result<()>` (reset, INIT, `0x10` + `fb.as_bytes()` in ≤ 4 KB chunks with DC high, refresh, power off, deep sleep; BUSY timeout → `Err`).

- [ ] **Step 1: Failing tests** in `core/src/epd.rs`: `resolution_is_800_by_480` (the `0x61` entry data == `[0x03,0x20,0x01,0xE0]`), `init_ends_with_power_on` (last command `0x04`, no data), `commands_match_the_reference_order` (command bytes == `[0xAA,0x01,0x00,0x03,0x05,0x06,0x08,0x13,0x30,0x41,0x50,0x60,0x61,0x82,0x84,0x86,0xE3,0xE0,0xE6,0x04]`); in `framebuf.rs` `colour_bars_show_every_ink` (pixel at x=66+133·i is `Ink::ALL[i]`).
- [ ] **Step 2:** `cargo test -p frogterm-core epd colour_bars` → FAIL.
- [ ] **Step 3:** Implement; run → PASS; commit `core: ED2208 panel protocol table, colour bars`.
- [ ] **Step 4:** Implement `firmware/src/epd.rs` (esp-idf-hal `SpiDriver` on SPI2, `SpiDeviceDriver` with CS 10 at `SPI_HZ`, DC/RST outputs, BUSY input pull-up; set GPIO16 low and GPIO14 high first so the SD card stays off the bus). With `--features bars`, `main` shows `colour_bars()` then deep-sleeps. `make FEATURES=bars` builds.
- [ ] **Step 5 (hardware, owner):** `make flash FEATURES=bars` → six bars black, white, yellow, red, blue, green left→right; serial log shows refresh time. Record the result in the commit message.
- [ ] **Step 6:** Commit `firmware: ED2208 e-paper driver (colour bars verified on device)`.

### Task 4: Buzzer, LED, battery

**Files:**
- Create: `core/src/beep.rs`, `core/src/battery.rs`, `firmware/src/board.rs`
- Modify: `core/src/lib.rs`

**Interfaces:**
- Produces (core): `pub struct Tone { pub hz: u32, pub ms: u32 }` (hz 0 = silence); `pub const SUCCESS: &[Tone] = &[Tone{hz:2000,ms:150}]`; `pub const ERROR: &[Tone] = &[1000/100, 0/80, 1000/100, 0/80, 1000/100]`. `battery::percent(mv: u32) -> u8` (piece-wise linear Li-ion: 4200→100, 4000→80, 3800→55, 3700→35, 3600→15, 3450→0, clamp), `battery::too_low_to_update(mv: u32) -> bool` (`mv < 3450`).
- Produces (firmware): `Board { pub fn beep(&mut self, tones: &[Tone]); pub fn led(&mut self, on: bool); pub fn battery_mv(&mut self) -> Option<u32> }` — buzzer via LEDC on GPIO45 (50 % duty), battery: GPIO21 high, 10 ms, ADC1 GPIO1 with 11 dB attenuation calibrated, ×2, GPIO21 low.

- [ ] **Step 1: Failing tests:** `success_is_one_short_high_beep`, `error_is_three_low_beeps` (3 tones with hz 1000), `battery_percent_curve` (4200→100, 3450→0, 3700→35, 5000→100, 3000→0, monotonic over 3000..=4300 step 10), `too_low_to_update` (3449 → true, 3450 → false).
- [ ] **Step 2:** run → FAIL. **Step 3:** implement → PASS; commit `core: beep patterns, battery curve`.
- [ ] **Step 4:** Implement `board.rs`; `main` (without `bars`) beeps `SUCCESS`, logs battery mV/%, sleeps. Hardware check: one beep, plausible voltage (3.5–4.2 V). Commit `firmware: buzzer, LED, battery`.

### Task 5: Clock, schedule and sleep

**Files:**
- Create: `core/src/schedule.rs`, `core/src/pcf8563.rs`, `firmware/src/rtc.rs`, `firmware/src/sleep.rs`
- Modify: `core/src/lib.rs`

**Interfaces:**
- Produces (core):
  - `pcf8563::encode(unix: i64) -> [u8; 7]` (registers 0x02..=0x08: sec, min, hour, day, weekday, month|century, year, BCD, UTC) and `pcf8563::decode(regs: [u8; 7]) -> Option<i64>` (`None` if VL bit 0x80 of seconds is set or fields invalid).
  - `schedule::next_update(now: i64, hour: u8) -> i64` — Unix time of the next local `hour:00` strictly more than 60 s after `now`.
  - `enum WakePlan { Update, Sleep(u64 /*secs*/) }`; `schedule::on_timer_wake(now: Option<i64>, target: i64) -> WakePlan` — `None` (time unknown) → `Update`; more than 120 s before `target` → `Sleep(target - now)`; else `Update`.
  - `schedule::retry_after_failure(attempt: u8) -> Option<u64>` — attempts 1..=3 → `Some(1800)`, then `None` (go to the daily schedule).
  - `schedule::when_time_unknown() -> u64` = 3600.
  - `schedule::arm_button(button_held: bool) -> bool` = `!button_held`.
- Produces (firmware): `Rtc { fn read(&mut self) -> Result<Option<i64>>; fn write(&mut self, unix: i64) -> Result<()> }` (I2C0 SDA 19 / SCL 20, 100 kHz, addr 0x51); `sleep::deep_sleep(secs: u64, arm_button: bool) -> !` (timer + `esp_sleep_enable_ext1_wakeup(1<<3, ESP_EXT1_WAKEUP_ANY_LOW)` with RTC pull-up on GPIO3 when armed); `sleep::wake_cause() -> Wake { Timer, Button, PowerOn }`; RTC-memory (`#[link_section = ".rtc.data"]`) state `{ target: i64, attempt: u8 }`.

- [ ] **Step 1: Failing tests:** `bcd_round_trip` (2026-11-25 04:00:00 UTC encodes to `[0x00,0x00,0x04,0x25,<weekday 3>,0x11,0x26]` and decodes back), `low_voltage_flag_means_unknown` (sec reg `0x80` → `None`), `next_update_is_tomorrow_6_local` (now = 25 Nov 05:59:30 local → 26 Nov 06:00 local, since within 60 s; now = 25 Nov 05:00 → 25 Nov 06:00), `next_update_across_dst` (now = 2026-03-26 12:00 local → 27 Mar 06:00 IDT = 03:00 UTC; now = 2026-10-24 12:00 → 25 Oct 06:00 IST = 04:00 UTC), `early_timer_wake_sleeps_again` (target − 600 s → `Sleep(600)`; target − 60 s → `Update`), `unknown_time_retries_hourly` (`on_timer_wake(None, …)` → `Update`, `when_time_unknown()` == 3600), `retry_after_failure` (1,2,3 → `Some(1800)`, 4 → `None`), `held_button_is_not_armed`.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement → PASS; commit `core: PCF8563 BCD, schedule, wake and retry policy`.
- [ ] **Step 4:** Implement `rtc.rs`, `sleep.rs`. `main`: log wake cause and RTC time; sleep 120 s with button armed. Hardware check: pressing KEY0 wakes immediately; holding it does not cause a wake loop. Commit `firmware: RTC, deep sleep, button wake`.

### Task 6: Wi-Fi, time sync and HTTPS streaming

**Files:**
- Create: `firmware/src/net.rs`
- Modify: `core/src/album.rs` (+ tests), `pc/src/net.rs` (use the new core pieces)

**Interfaces:**
- Produces (core): `album::ShareScanner { fn new() -> Self; fn feed(&mut self, chunk: &[u8]); fn finish(self) -> Result<AlbumPage> }` — keeps only the bytes between `AF_initDataCallback({key: 'ds:1'` … `, sideChannel:` (≤ 400 KB), then reuses `parse_share_page` logic; `album::rpc_body(album_id, key, token) -> String` = `"f.req=" + percent-encoded rpc_form(...)`.
- Produces (firmware): `Net::connect(modem, ssid, pw, timeout_s: 20) -> Result<Net>`; `net.sync_time(timeout_s: 15) -> Result<i64>` (SNTP `pool.ntp.org`); `net.get(url, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<Response>` and `net.post_form(url, body, sink) -> Result<Response>` where `Response { status: u16, final_url: String }`; TLS via `crt_bundle_attach`, redirects followed (≤ 10), one `EspHttpConnection` reused, buffers 4 KB, User-Agent `frogterm/0.1`.

- [ ] **Step 1: Failing tests:** `share_scanner_matches_whole_page_parse` (synthetic page from the existing tests, fed in chunks of 1, 7, 4096 → same `AlbumPage` as `parse_share_page`), `share_scanner_keeps_little_memory` (page with 1 MB of filler before the block → internal buffer ≤ block length + needle length), `rpc_body_is_form_encoded` (no raw `[`, `"`, `,`; decoding it gives `rpc_form(...)`).
- [ ] **Step 2:** run → FAIL. **Step 3:** implement → PASS; switch `pc/src/net.rs::fetch_album` to `ShareScanner` + `rpc_body`; `make emulator` still works; commit `core: streaming share-page reader, form body`.
- [ ] **Step 4:** Implement `net.rs`. `main`: connect, sync time, write RTC, GET the share link, log photo count of page 1. Hardware check: log shows ~300 photos and the RTC time. Commit `firmware: Wi-Fi, SNTP, HTTPS`.

### Task 7: Photo list in flash and the last screen cache

**Files:**
- Create: `core/src/photo_list.rs` (moved from `pc/src/photo_list.rs` with its tests), `firmware/src/store.rs`
- Modify: `pc/src/emulator.rs`, `pc/src/main.rs`, `core/src/lib.rs`

**Interfaces:**
- Produces (core): `PhotoList` (as today, text format v2); `photo_list::after_refresh(old: Option<PhotoList>, fetched: Vec<Photo>, complete: bool, album_url: &str, now: i64) -> Option<PhotoList>` — complete → new list; incomplete → old list if any, else the partial list if non-empty, else `None`.
- Produces (firmware): `Store { fn load_list(&self) -> Option<PhotoList>; fn save_list(&mut self, l: &PhotoList) -> Result<()>; fn load_screen(&self) -> Option<FrameBuffer>; fn save_screen(&mut self, fb: &FrameBuffer) -> Result<()> }` on partitions `photos` / `lastscr` via `esp_partition_find_first`, `esp_partition_erase_range` (4 KB aligned), `esp_partition_write`, `esp_partition_read`; record = magic `b"FTL1"` + u32 length + bytes. `FrameBuffer::from_bytes(&[u8]) -> Option<FrameBuffer>` added in core.

- [ ] **Step 1: Failing tests:** `partial_refresh_policy` (four cases above), `framebuffer_bytes_round_trip` (`from_bytes(fb.as_bytes())` equal; wrong length → `None`; byte with invalid nibble 0x4 → `None`).
- [ ] **Step 2:** run → FAIL. **Step 3:** move + implement → PASS (`make tests`); commit `core: photo list + refresh policy shared with the device`.
- [ ] **Step 4:** Implement `store.rs`; `main`: refresh list (all pages via `net` + core) if missing/stale, save, reload, log count and age. Hardware check: first boot logs ~29 pages and total time; second boot reads the list from flash in < 1 s. Commit `firmware: photo list and last screen in flash`.

### Task 8: JPEG decoding in core

**Files:**
- Create: `core/src/jpeg.rs`
- Modify: `core/Cargo.toml` (`jpeg-decoder = { version = "0.3", default-features = false }`; dev-dep `image` with `jpeg`), `pc/src/emulator.rs` (use it)

**Interfaces:**
- Produces: `jpeg::decode_rgb(bytes: &[u8], w: u32, h: u32) -> Result<Vec<u8>>` — RGB888 of exactly `w×h`; grayscale is expanded to RGB; other sizes → `Err` (Google always sends the requested size).

- [ ] **Step 1: Failing tests:** `decodes_a_colour_jpeg` (test encodes a 16×8 solid (200,40,40) image with `image`, decodes, every pixel within ±6), `grayscale_becomes_rgb`, `wrong_size_is_an_error`, `garbage_is_an_error`.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement → PASS; emulator uses it (keep its resize fallback only for wrong sizes); `cargo build --release` of firmware still links (check binary < 3 MB); commit `core: JPEG decoding for the device`.

### Task 9: The update cycle on the device

**Files:**
- Create: `firmware/src/app.rs`
- Modify: `firmware/src/main.rs`

**Interfaces:**
- Consumes everything above. `app::run(peripherals) -> !` implements spec 7.1/7.2 in this order: wake cause → (timer) `on_timer_wake` → board init (LED on, SD off) → battery (`too_low_to_update` → error beep, sleep to `next_update`) → `Net::connect` (fail → error beep, `retry_after_failure`) → SNTP + RTC write (fail and RTC unknown → `when_time_unknown`) → photo list (`after_refresh`) → `screen::plan` with `FRESH` → per slot download `sized_url` → `jpeg::decode_rgb` → `veil` (if opacity < 100) → `dither_area` (3 attempts; 403/404 marks list stale) → `store.save_screen` (on total failure: `load_screen`, else leave the panel untouched) → weather (`forecast_url`/`parse_forecast`) → calendars (stream each into `IcsScanner`, `sort_events`, `remaining`) → `draw_events_box` + `draw_weather_box` (status `"{place} HH:MM {pct}%"`) → Wi-Fi off → `Epd::show` → `SUCCESS` beep → `deep_sleep(next_update − now, arm_button(KEY0 low))`. A 240 s task watchdog guards the whole run.

- [ ] **Step 1:** Implement `app.rs`; `make` builds.
- [ ] **Step 2 (hardware):** `make flash && make monitor`: within ~60 s (first boot ~90 s with the album read) the panel shows photo(s), events box and weather box matching `make emulator` for the same moment; one beep; log ends with "sleeping until <next 06:00>".
- [ ] **Step 3 (hardware):** press KEY0 → new photo(s) + beep, schedule unchanged. Disable Wi-Fi at the router → no screen change, three low beeps, log "retry in 1800 s".
- [ ] **Step 4:** Commit `firmware: full update cycle`.

### Task 10: Flashing from WSL and the README

**Files:**
- Create: `README.md`
- Modify: `Makefile`

**Interfaces:**
- Produces: `make flash [PORT=/dev/ttyACM0] [FEATURES=…]` — if no `/dev/ttyACM*`/`/dev/ttyUSB*` exists and `usbipd.exe` is on PATH, run `usbipd.exe attach --wsl --busid $(USBIPD_BUSID)` (from `config.toml`-independent make variable, printed hint if unset), wait ≤ 10 s for the device, then `docker run … --device $(PORT) --group-add $(stat -c %g $(PORT)) frogterm-dev espflash flash --chip esp32s3 --partition-table firmware/partitions.csv build/frogterm.bin` (port, not `--monitor`). `make monitor` = `espflash monitor --port $(PORT)`.

- [ ] **Step 1:** Implement; README covers: one-time `winget install usbipd`, `usbipd list`, `usbipd bind --busid <id>` (admin), `config.toml` fields, `make`, `make flash USBIPD_BUSID=<id>`, `make monitor`, `make emulator`, `make tests`.
- [ ] **Step 2 (hardware):** from a fresh WSL shell, `make flash USBIPD_BUSID=<id>` flashes and the device boots. Commit `make flash/monitor from WSL via usbipd; README`.
