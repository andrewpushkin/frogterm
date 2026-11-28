# Settings portal — design

Date: 2026-11-28. Status: draft for the owner's approval.

## Intent

Settings stop being fixed at build time. They live in a **configuration file**
the device reads when it runs; a **settings web page**, served by the device
itself, edits that file. No reflashing to change the album, calendars, Wi-Fi,
times or look.

The emulator gets the same modes and serves **the same HTML**; it is built
first, the firmware follows.

## What the owner decided

- A **10-second hold of the right button** (Next): settings mode as a **Wi-Fi
  client** — the device joins the configured Wi-Fi and serves the page there.
- A **10-second hold of the green button**: settings mode as a **Wi-Fi access
  point** — the device makes its own network and serves the page on it.
- In settings mode the screen shows the **Wi-Fi network name** and the
  **page address**.
- A **green press** in settings mode returns to normal mode.
- Settings are a **configuration file**, not build-time values.
- Format: keep **`config.toml`**, read and written by a **small reader in
  `core/`** (approach A below).
- Emulator first, with the same HTML as the device.

## Approaches (for the file format)

| | Approach | Trade-off |
|---|---|---|
| **A (chosen)** | Keep `config.toml`; a ~100-line reader/writer in `core/` for its flat shape (`key = "text"`, numbers, lists of text, `#` comments) | Owner's existing file keeps working, same code on device and PC, small; unusual TOML (tables, multi-line strings) is refused with a clear error |
| B | The `toml` crate on the device too | Full TOML, nothing to write; a few hundred KB of firmware, against the small-crates rule for `core/` |
| C | Switch to JSON (`serde_json` is already in `core/`) | No new code; loses comments, the owner's file must be converted |

## Goals

- Every setting of `config.toml` can be changed from a phone browser.
- One page, one form handler, one validator, one file format — in `core/`,
  used by both the firmware and the emulator.
- The device works with no file at all (first flash): it goes straight to
  access-point settings mode.

## Non-goals

- No OTA firmware update from the page.
- No accounts or login on the page.
- No captive-portal pop-up tricks (DNS hijack) in the first version — the
  address on the screen is typed in by hand.

## Anti-goals

- Saving a setting that breaks the device: the page never writes a file that
  the validator refuses.
- Losing the owner's comments in `config.toml` when the page saves.
- Secrets (Wi-Fi password, secret calendar links) appearing in logs.

## Behaviour

### Buttons

- A press now acts **on release**, so a hold can be told from a press. A press
  shorter than 10 s does exactly what it does today (green: sync; side buttons:
  browse).
- Held **10 s**: a distinct beep the moment it counts (the finger can come
  off), then settings mode. Green → access point; Right → Wi-Fi client. A long
  hold of Left does nothing new (a normal press on release).
- Waking from deep sleep works the same: the press wakes the chip, the
  firmware watches the pin until release or 10 s.

### Settings mode

- **Access point** (green): open network named `frogterm-setup`; page at
  `http://192.168.4.1/`.
- **Wi-Fi client** (right): joins `wifi_ssid`; page at
  `http://<device IP>/`. If joining fails: error beep, back to normal mode
  (the screen is not touched).
- The panel shows a settings screen: title, network name, page address, and
  "press green to finish". Drawn by `core/` (same screen in the emulator).
- The **LED is on** for the whole of settings mode.
- **Green press** → leave settings mode: the page server and Wi-Fi stop, and a
  normal green-button sync runs with the (possibly new) settings. Side presses
  in settings mode are ignored with the busy pips.
- **Timeout** (my call): 15 min with no request to the page → leave settings
  mode as if green was pressed, so a forgotten device does not drain the
  battery.
- **Panel rest** (my call): the settings screen is drawn at once even while
  the panel rests (rare, owner-initiated). The sync after leaving follows the
  normal rest rule.

### The page

- One HTML page with a form: every key of `config.toml`, grouped as in
  `config.example.toml` (photos, time zone, weather, calendars, look, device).
  Calendars and update times as one-per-line text areas.
- Secret fields (Wi-Fi password, calendar links containing `/private-`) are
  shown **empty with "unchanged" as placeholder** (my call); left empty, the
  stored value stays.
- **Save** → the form is validated with the same rules as today's
  `Config::parse`; on error the page comes back with the message and nothing
  is saved; on success the file is written and the page says "Saved — press
  the green button to finish".
- Plain HTML + a little CSS, no JavaScript needed, no external resources (the
  access point has no internet). Embedded in the binary from `core/`.

### The file

- **Emulator**: the `--config` file (default `config.toml`), read at every
  wake and rewritten by the page.
- **Device**: the same text in NVS (blob `config`, a few KB, wear-levelled).
  No file → access-point settings mode at power-on.
- Writing **keeps the file's own lines**: a changed key's value is replaced in
  place, comments and order stay; a key that was missing is appended; a key
  set back to its default stays written (simple, explicit).

## Where the code goes

- `core/src/config.rs` (new): `Config`, the flat-TOML reader, the in-place
  writer, the validator (moved from `pc/src/config.rs`, `serde`/`toml` no
  longer needed for it).
- `core/src/portal.rs` + `core/src/portal.html` (new): the page, form decoding
  (`application/x-www-form-urlencoded`), and one function both servers call:
  request (method, path, body) + current file text → response + new file
  text if saved.
- `core/src/buttons.rs`: hold detection (press vs 10-s hold) as a pure,
  tested state machine fed with pin levels and time.
- `core/src/layout.rs`: the settings screen.
- `pc/`: hold detection in the window (mouse or key held 10 s); a small HTTP
  server on `std::net::TcpListener` (no new crate) calling `core::portal`.
  Access-point mode is pretended: the screen shows `frogterm-setup` and the
  PC's address; client mode shows `wifi_ssid` and the PC's LAN address.
- `firmware/` (second step): config from NVS instead of `build.rs`
  (`build.rs` stops baking settings, `settings.rs` goes), Wi-Fi AP / client
  and `esp-idf-svc`'s HTTP server calling `core::portal`.
- README and `config.example.toml` comments updated (no more "flash again
  after changing it").

## Left to the builder

- Port numbers for the emulator's server (8080 suggested, next free if taken).
- Exact look of the page and of the settings screen, within the inks and the
  existing fonts.
- Whether the firmware's first step keeps a build-time `config.toml` as a
  seed for an empty NVS (handy for the owner's own device) — off by default.
