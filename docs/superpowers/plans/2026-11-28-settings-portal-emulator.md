# Settings portal (emulator first) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** settings in a runtime `config.toml` edited from a web page the device serves; a 10-s hold of Right (home Wi-Fi) or Green (own access point) opens it; everything else on release. This plan: shared `core/` parts and the emulator. The firmware side gets its own plan.

**Architecture:** `core/` owns the config file (flat-TOML reader + in-place writer + validator), the page (`portal.html` + form handling as one pure `handle` function), hold/press detection (pure state machine) and the settings screen. The emulator adds a `std::net` HTTP server that calls `core::portal::handle`, and routes window gestures to its device thread.

**Tech Stack:** Rust 2021, `embedded-graphics`/`u8g2-fonts` (screen), `std::net` (emulator server). No new crates.

**Spec:** `docs/superpowers/specs/2026-11-28-settings-portal-design.md`

## Global Constraints

- `core/` stays small-crate: no `serde`/`toml` there; nothing new in `core/Cargo.toml`.
- Hold time 10 s (`HOLD_MS = 10_000`); Left stuck after 60 s counts as released (`STUCK_MS = 60_000`).
- Access point: SSID `frogterm-setup`, page `http://192.168.4.1/`. Settings mode times out after 15 min without a page request.
- Every button acts on release; the only action while down is settings mode at the 10-s mark (Green/Right only).
- Saving never writes a file `Config::parse` refuses; comments and key order of the file are kept.
- Secrets (Wi-Fi password, `/private-` calendar links) never in logs and never sent back in the page.
- Commit after every green step; no Claude co-author line.
- Firmware must keep building (`make firmware`) though its behaviour is unchanged in this plan.

## Review Focus

- A list value split over several lines with comments inside (as in `config.example.toml`) — must parse and be replaced in place as one unit. Test in Task 1/2.
- Form text with `"` `\` or non-ASCII (Hebrew/Cyrillic place names) — must round-trip through write + parse. Test in Task 2.
- An empty optional field (`place_name`, `latitude`) — the key goes away, not `place_name = ""`. Test in Task 2/4.
- A broken or missing config file when the page opens — the page still loads (empty fields + message) so the owner can fix it. Test in Task 4.
- A request larger than the limit or a slow/half request to the emulator server — refused, the server keeps serving. Test in Task 6.

---

### Task 1: `core::config` — the file and its validator

**Files:** Create `core/src/config.rs`; modify `core/src/lib.rs`, `pc/src/config.rs` (becomes `pub use frogterm_core::config::Config;`), `pc/src/*.rs` users, `pc/Cargo.toml` (drop `serde`, `toml`), `firmware/build.rs` (use `frogterm_core::config::Config`, drop the `#[path]` include), `firmware/Cargo.toml` (drop `serde`, `toml` build-deps).

**Interfaces — Produces:**
- `pub enum Value { Str(String), Num(String), List(Vec<String>) }` (numbers kept as written)
- `pub fn parse_flat(text: &str) -> Result<Vec<(String, Value)>>` — `key = "str"` (escapes `\" \\ \n \t \uXXXX`), numbers, `[ "a", "b" ]` over several lines with `#` comments and a trailing comma; `#` comments; anything else (tables, bare words) → error with the line number.
- `pub struct Config` — same fields, defaults and checks as today's `pc/src/config.rs`; `Config::parse(&str)`, `Config::load(&Path)`, `label()`, `zone()`, `update_times()`, `coordinates()`. Unknown keys are ignored.

- [ ] Move today's `pc/src/config.rs` tests into `core/src/config.rs` unchanged; add `multi_line_list_with_comments_parses`, `escapes_and_unicode_parse` (`place_name = "תל אביב \"x\""`), `tables_are_refused_with_the_line`.
- [ ] Run `cargo test -p frogterm-core config` — FAIL (no module).
- [ ] Implement; switch `pc` and `firmware/build.rs` to it.
- [ ] `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` — PASS; `make firmware` builds.
- [ ] Commit.

### Task 2: `core::config` — in-place writer

**Interfaces — Produces:**
- `pub fn set(text: &str, key: &str, value: Option<&Value>) -> String` — replaces the key's whole span (all lines of a multi-line list) with `key = <value>`; `None` removes the span; a missing key goes right after a commented-out `# key = …` line if there is one, else at the end. Other lines untouched byte for byte.
- `pub fn format_value(v: &Value) -> String` — strings quoted and escaped, lists one item per line when more than one.

- [ ] Tests: `set_replaces_in_place_and_keeps_comments`, `set_replaces_a_multi_line_list_as_one`, `none_removes_the_key`, `missing_key_goes_after_its_commented_example`, `written_text_parses_back` (quotes, backslash, Hebrew), `example_config_round_trips` (every key of `config.example.toml` set to its own value → same `Config`).
- [ ] FAIL → implement → PASS → commit.

### Task 3: `core::buttons` — press on release, hold at 10 s

**Interfaces — Produces:**
- `pub const HOLD_MS: u64 = 10_000; pub const STUCK_MS: u64 = 60_000;`
- `pub enum Gesture { Press(Press), Hold(Press) }` — `Hold` only for `Update` and `Next`.
- `pub struct Holds` — `Holds::new(down: [bool; 3], now_ms: u64)` (buttons down now count as pressed now: the press that woke the chip); `poll(&mut self, down: [bool; 3], now_ms: u64) -> Option<Gesture>` (levels in `ALL` order).

- [ ] Tests: `a_press_counts_on_release`, `green_or_right_held_10_s_is_a_hold_at_once`, `the_release_after_a_hold_does_nothing`, `left_held_long_is_a_press_on_release`, `left_stuck_60_s_counts_as_released`, `the_waking_button_is_pressed_from_the_start`.
- [ ] FAIL → implement → PASS → commit.

### Task 4: `core::portal` — the page

**Files:** Create `core/src/portal.rs`, `core/src/portal.html`.

**Interfaces — Produces:**
- `pub struct Response { pub status: u16, pub content_type: &'static str, pub body: String }`
- `pub fn handle(method: &str, path: &str, body: &[u8], config_text: &str) -> (Response, Option<String>)` — `GET /` form; `POST /save` → new file text when valid; else 404. Every field of `config.example.toml`; calendars and update times one per line; empty optional fields remove their key; Wi-Fi password and `/private-` calendar links never in the page — the password field is empty ("unchanged"), a private link shows as `private calendar N (unchanged)` and that line keeps the stored link.
- `pub fn decode_form(body: &[u8]) -> Vec<(String, String)>` (`+` = space, `%XX`, via `url::percent_decode`).
- `pub fn html_escape(s: &str) -> String`.

- [ ] Tests: `the_page_shows_the_settings_but_no_secrets`, `saving_valid_form_writes_the_file_keeping_comments`, `an_invalid_form_saves_nothing_and_says_why` (e.g. `photo_batch=0`), `empty_password_keeps_the_stored_one`, `private_calendar_placeholders_keep_their_links`, `a_broken_file_still_opens_the_page`, `values_are_html_escaped`, `unknown_path_is_404`.
- [ ] FAIL → implement → PASS → commit.

### Task 5: settings screen and the shared settings-mode steps

**Interfaces — Produces:**
- `layout::draw_settings_screen(fb: &mut FrameBuffer, network: &str, address: &str)` — white screen, title "Settings", "Wi-Fi network", "Open in a browser", "Press the green button to finish".
- `pub enum Via { AccessPoint, HomeWifi }` in `app`; `pub const AP_SSID: &str = "frogterm-setup"; pub const AP_ADDRESS: &str = "http://192.168.4.1/"; pub const SETTINGS_IDLE_SECS: u64 = 15 * 60;`
- `app::via_for(p: Press) -> Option<Via>` (Update → AccessPoint, Next → HomeWifi).
- `app::open_settings(dev: &mut impl Device, state: &mut State, network: &str, address: &str, now: Option<i64>) -> Result<()>` — chirp, LED on, shows the settings screen (even while the panel rests), `last_refresh = now`, saves the state.
- `app::settings_failed(dev, state, now) -> u64` — error beep, LED as `led_while_asleep`, seconds to sleep until the next update.
- After leaving settings mode the device runs `app::run(.., Wake::Button(Press::Update), ..)` with the reloaded settings.

- [ ] Tests: `settings_screen_shows_network_and_address` (black pixels inside text rows, nothing red), `open_settings_draws_at_once_even_while_the_panel_rests`, `holds_choose_the_network`.
- [ ] FAIL → implement → PASS → commit.

### Task 6: emulator HTTP server

**Files:** Create `pc/src/portal_server.rs`.

**Interfaces — Produces:**
- `pub struct Server` — `Server::bind(ports: RangeInclusive<u16>) -> Result<Server>` (first free of 8080..=8089, all interfaces), `port()`, `serve_once(&mut self, config_path: &Path, wait: Duration) -> Result<Served>` where `pub enum Served { Nothing, Request, Saved }`: accepts at most one connection within `wait`, reads it (headers + `Content-Length` body, 16 KB limit, 5-s read timeout), calls `portal::handle` with the file's text (missing file = empty), writes the new text when saved, answers.
- `pub fn lan_address() -> Option<IpAddr>` (UDP connect trick, no packets).

- [ ] Tests (real sockets on 127.0.0.1): `get_returns_the_page`, `post_save_writes_the_config`, `oversized_request_is_refused_and_server_keeps_serving`, `nothing_within_wait_is_nothing`.
- [ ] FAIL → implement → PASS → commit.

### Task 7: emulator — gestures, settings mode, reload

**Files:** Modify `pc/src/window.rs`, `pc/src/emulator.rs`.

- Window: button levels from the mouse (held over a button) and keys (Left; Right; Space/Enter/G) fed to `Holds` every frame; gestures replace presses on the channel (`Sender<Gesture>`). While the device is awake: `Press` → busy pips (`app::press_while_awake`), `Hold` likewise — except in settings mode (`Shared::set_settings(bool)`), where a green `Press` goes to the device and others get the pips. The bar says "settings mode" then.
- Device thread: `Press(p)` → `Wake::Button(p)` as today. `Hold(p)` → `via_for(p)`: AccessPoint shows `AP_SSID` and `http://localhost:<port>/`; HomeWifi shows `wifi_ssid` (or "this PC's network") and `http://<lan_address>:<port>/`; `open_settings`, then `serve_once` in a loop until a green press or `SETTINGS_IDLE_SECS` without a request; then reload `Config` (and `tz::set_zone`) and run the green wake.
- `--once hold-green` / `--once hold-next`: open settings, save `out/screen.png` + `out/window.png`, exit without serving.
- [ ] Unit test the window's gesture routing as a pure fn `route(g: Gesture, busy: bool, settings: bool) -> Route { Send, BusyPips }`.
- [ ] Manual: `make emulator`, hold Green 10 s → screen, open the page, change `overlay_opacity`, Save, press Green → sync with the new look; `config.toml` comments still there.
- [ ] Commit.

### Task 8: docs

- README: buttons table (on release, 10-s holds), settings page section, "config.toml is read at runtime by the emulator; the firmware still bakes it in until its own step". `config.example.toml` header likewise.
- [ ] Commit.
