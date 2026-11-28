# Settings portal (firmware) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the reTerminal reads its settings from NVS (no more build-time values), opens settings mode on a 10-s hold (green: own access point, right: home Wi-Fi), serves the same page as the emulator, and acts on every button on release.

**Architecture:** the shared parts are in `core/` already (config, portal, `Holds`, settings screen, `app::start_settings/show_settings/settings_failed`). This plan adds the few shared pieces the device still needs (a Wi-Fi requirement in the validator, `Holds` that ignore a button still down, the end-of-settings step), then the firmware side: config in NVS, hold detection after a button wake, a gesture listener, Wi-Fi AP / client and `esp-idf-svc`'s HTTP server calling `core::portal::handle`. Leaving settings mode saves `pending_sync` and deep-sleeps until the panel's rest is over; the timer wake then runs the sync with fresh peripherals and the new settings.

**Tech Stack:** Rust, esp-idf-svc 0.54 (`EspWifi` AP + STA, `EspHttpServer`, `EspNvs`). No new crates.

**Spec:** `docs/superpowers/specs/2026-11-28-settings-portal-design.md`

## Global Constraints

- `core/` stays small-crate; nothing new in any `Cargo.toml` dependency list.
- Settings text lives in NVS namespace `frogterm`, blob `config`; nothing else stores it. `build.rs` no longer reads `config.toml` (except for the opt-in seed).
- Access point: open network `frogterm-setup`, page `http://192.168.4.1/` (port 80). Settings mode ends after 15 min without a page request.
- Every button acts on release; the only action while down is settings mode at 10 s (green/right). Left held 60 s counts as released.
- On the device, settings without `wifi_ssid` are not usable (the page refuses to save them; at start they open settings mode).
- Secrets (Wi-Fi password, calendar links) never in logs.
- `make firmware` builds without a `config.toml`. Never flash unless asked.
- Commit after every green step; no Claude co-author line.

## Review Focus

- The button that opened settings mode is still down when the listener starts: its release must not count as a press (it would finish settings mode at once).
- A save while the NVS is short of space: the page must say it was not saved, not "Saved".
- Settings mode that nobody uses with settings still unusable: the device must not stay awake (battery); it sleeps with the buttons armed.
- A body larger than 16 KB or a client that stops sending: refused, the server keeps serving.
- Leaving settings mode during the panel's rest: the sync still comes, after the rest.

---

### Task 1: core — device rules and the end of settings mode

**Interfaces — Produces:**
- `config::Config::usable(text: &str, wifi_needed: bool) -> Result<Config>` — `parse`, plus "wifi_ssid is missing (the device needs its Wi-Fi)" when `wifi_needed` and the SSID is absent or blank.
- `portal::handle(method, path, body, config_text, wifi_needed: bool)` — the page's messages and the save check use `Config::usable`.
- `buttons::Holds::ignoring(down: [bool; 3]) -> Holds` — buttons down now give nothing, not even on release.
- `app::ends_settings(g: Gesture) -> bool` — green press or green hold.
- `app::end_settings(dev, state, cfg, now) -> u64` — `pending_sync = true`, `target` = end of the panel's rest (or now), state saved; returns the seconds to sleep. `leave_settings` = `end_settings` + `run(Wake::Timer)`.

- [ ] Tests: `the_device_needs_its_wifi`, `the_page_refuses_settings_without_wifi_on_the_device`, `a_button_down_at_the_start_is_ignored_until_let_go`, `green_ends_settings`, `ending_settings_sleeps_until_the_rest_is_over`.
- [ ] FAIL → implement (emulator passes `wifi_needed = false`) → PASS → commit.

### Task 2: firmware — settings from NVS

**Files:** `firmware/src/settings.rs` (rewritten: NVS load/save of the text, optional seed), `firmware/src/app.rs` (`Settings` → `core::config::Config`), `firmware/build.rs` (no `config.toml`; with feature `seed-config` it validates `../config.toml` and copies it to `OUT_DIR/seed.toml`), `firmware/Cargo.toml` (feature `seed-config`), `Makefile` (no `config.toml` check).

- Load: blob `config` → `Config::usable(text, true)`; none or invalid → `None` (logged without the text).
- Seed (feature only): NVS has no `config` blob → the seed text is written once.
- `None` at start → access-point settings mode (Task 4); until then `app::settings_failed`-style sleep.
- [ ] `make firmware` builds, Docker `cargo clippy -- -D warnings` clean → commit.

### Task 3: firmware — gestures

**Files:** `firmware/src/sleep.rs`.

- `pub fn gesture_after_wake(p: Press) -> Gesture` — the waking button already up → `Press(p)`; else poll `Holds` every 20 ms until its gesture (at most `STUCK_MS` + a little).
- `pub fn spawn_gesture_listener() -> Receiver<Gesture>` — `Holds::ignoring(levels now)`; while `SETTINGS_MODE` (an `AtomicBool`) is set, gestures for which `app::ends_settings` is true go to the receiver; all others play `app::press_while_awake`.
- `app.rs`: a button wake resolves its gesture first (before the LED and the panel); `Hold` → settings mode (Task 4), `Press` → `app::run` as today.
- [ ] build + clippy → commit.

### Task 4: firmware — settings mode

**Files:** create `firmware/src/portal.rs`; modify `firmware/src/net.rs` (`Net::ip()`), `firmware/src/app.rs`.

- `portal::Network::access_point(modem, sysloop, nvs)` — AP `frogterm-setup`, open, channel 1, 4 clients; address `app::AP_ADDRESS`.
- Home Wi-Fi: `Net::connect` with the stored SSID/password, address `http://<ip>/`; join failure → `app::settings_failed`, sleep.
- `portal::serve(nvs_part, last_request) -> EspHttpServer` — wildcard GET/POST handler: body up to 16 KB (else 413), reads blob `config`, `core::portal::handle(.., true)`, a save writes the blob (a write error turns the answer into an error page, nothing claimed saved), logs method + path only.
- Loop: wait for `ends_settings` gestures; green → reload with `Config::usable`; usable → server and Wi-Fi off, `app::end_settings`, deep sleep; unusable → error beep, stay. 15 min without a request: usable → as green; unusable → deep sleep, buttons armed. A 60-min cap ends it in any case.
- Unusable settings at any wake but a timer wake → access-point settings mode; a timer wake → sleep again (`when_time_unknown`), buttons armed.
- [ ] build + clippy → commit.

### Task 5: docs

- README (device settings mode, `seed-config`, NVS), `config.example.toml` header, Makefile help.
- [ ] Commit.
