# frogterm — reTerminal E1002 photo + weather frame

Date: 2026-11-25
Status: draft for approval (updated with owner decisions, 2026-11-25)

## 1. Intent

A custom firmware for a Seeed **reTerminal E1002** (ESP32-S3, 7.3" 800×480
six-colour E Ink Spectra 6 panel, battery powered) that turns it into a
family photo frame with a small weather forecast.

- Once a day at **06:00 local time (Asia/Jerusalem)**, and whenever the
  **green button** is pressed, the device wakes, shows a **random photo from
  the whole Google Photos album** and the **hourly weather for the next 6
  hours in Tel Aviv**, **beeps**, and goes back to deep sleep.
- The photo is **full screen** (800×480) as the background; the weather sits
  in a box on top. Where the box goes does not matter to the owner.
- **Portrait photos come in pairs** (owner's call): if the random photo is
  tall, a second tall photo **from the newest 300** is shown next to it, each
  398×480 with a 4 px white gap. Tall = portrait or square (it loses less in
  a half slot). A landscape photo is fully random and fills the screen.
- **Today's calendar events** (owner's call) from Google Calendar "secret
  address in iCal format" links (no password), merged: Family + public
  Israeli holidays (+ main calendar when the owner sends its link).
- Photos come from the owner's live album "frogterm" (Google adds every photo
  of the chosen people automatically), shared as **"anyone with the link"** —
  no Google password, token or account on the device.
- The photo is downloaded **already converted to 800×480 by Google**.
- Code in **Rust** (C only where Rust is not possible). The target is a
  resource-limited ESP32-S3: code shared with the firmware (`core/`) may only
  use small embedded-friendly crates (`embedded-graphics`, `u8g2-fonts`,
  `serde_json`, `anyhow`); heavy crates stay in the PC tools (`pc/`).
- A **PC emulator** (`make emulator`) runs one full device update on the PC
  from `config.toml` (album link + city) and saves `out/screen.png`.
- Root **Makefile** (`make`, `make clean`, `make tests`, `make flash`) and a
  **Dockerfile** with the entire toolchain. Device on the owner's Windows PC
  as **COM7**; development in **WSL2**.

## 2. Goals

1. Wake at 06:00 local time every day (DST-correct), within ±2 minutes.
2. Wake on green-button press (KEY0, GPIO3) from deep sleep.
3. Each update: Wi-Fi → pick a random photo from the cached album list →
   download it as 800×480 JPEG → decode → dither to 6 colours → fetch
   weather → draw weather box → refresh panel → beep → deep sleep.
4. Random pick is uniform over the **whole album** (several thousand photos on
   2026-11-25, and growing), not just the newest page.
5. Weather box: 6 hourly slots with hour, icon, temperature °C, rain %.
6. Everything builds and tests inside Docker; flashing from WSL via
   usbipd-win with one command.
7. Battery life measured in months.

## 3. Non-goals

- No helper server, cloud function or PC-side service at runtime.
- No Google OAuth / official Photos API (it cannot read live albums or
  shared albums since 31 March 2025).
- No OTA updates, no on-device config portal. Config is baked in at build
  time; changing it means reflashing.
- No use of KEY1/KEY2, SD card, microphone, touch, temperature sensor.
- No multiple albums, locations or languages; labels English/numeric.
- No video playback. Videos/motion photos in the album are shown as their
  still frame (Google's thumbnail), not filtered out.

## 4. Anti-goals (failure even if it "works")

- Device stays awake and drains the battery after a failure.
- Screen left blank, half-drawn, or showing an error page because one
  download failed (e-paper keeps the last image for free — use that).
- Secrets (Wi-Fi password, album link) committed to git.
- Flashing needs host tools beyond Docker, make and usbipd-win.

## 5. Hardware facts (Seeed wiki / Zephyr docs — verify against schematic)

| Item | Detail |
|---|---|
| SoC | ESP32-S3, 8 MB OPI PSRAM, flash 32 MB per Zephyr (Arduino pages say 8 MB — check with `espflash board-info`) |
| Panel | 7.3" 800×480 Spectra 6 (black, white, yellow, red, blue, green), controller **ED2208-GCA**, write-only SPI |
| ePaper pins (Seeed_GFX Setup521 = Zephyr DTS) | SCK 7, MOSI 9, CS 10, DC 11, RST 12 (active low), BUSY 13 (LOW = busy, pull-up); SPI2, 4 MHz; no panel power-enable pin |
| Panel protocol (Zephyr `display_ed2208_gca.c`, Apache-2.0) | reset LOW 20 ms, HIGH, wait 10 ms; init table (see firmware plan); `0x10` + 192 000 bytes (4 bpp, left pixel = high nibble, codes 0 black 1 white 2 yellow 3 red 5 blue 6 green); refresh `0x12 0x00` (BUSY ≤ 40 s); power off `0x02 0x00`; deep sleep `0x07 0xA5` |
| Buttons | KEY0 GPIO3 (green, right), KEY1 GPIO4, KEY2 GPIO5; active-low, HW pull-up |
| Deep-sleep wake | `esp_sleep_enable_ext1_wakeup(1<<3, ANY_LOW)` + keep RTC pull-up on GPIO3 |
| Buzzer | GPIO45, passive piezo, PWM (LEDC), ~800–2000 Hz |
| RTC | PCF8563 @ 0x51, I2C SDA 19 / SCL 20, CR1220 backup. INT wiring unknown — don't rely on it |
| Battery | ADC GPIO1 (×2 divider), enable GPIO21 (HIGH = on), 4.2 V full |
| LED | GPIO6, active-low |
| SD card | shares SCK/MOSI with panel; SD_CS 14, SD_EN 16 — keep SD off / CS high |
| Serial | USB-UART bridge on GPIO43/44, 115200 — COM7 on Windows |

Panel driver reference (init sequence, refresh, BUSY polarity): Seeed_GFX
(`BOARD_SCREEN_COMBO 521`, `Setup521`) on GitHub; Zephyr `reterminal_e1002`
as cross-check. Schematic:
`https://files.seeedstudio.com/wiki/reterminal_e10xx/res/202004321_reTerminal_E1002_V1_2_SCH_251120.pdf`.

## 6. Approaches considered

### 6.1 Where the photo work happens
- **On-device (chosen — owner's call).** No extra infrastructure.
- Helper server that scrapes/dithers — rejected: needs an always-on machine.

### 6.2 Which photos are eligible
- **Whole album via Google's internal paging request (chosen — owner's
  call, tested OK).** Uniform random over all photos. Risk: undocumented
  request, may change.
- Newest 300 only (public share page alone) — rejected by owner: older
  photos would never appear.

### 6.3 Firmware stack
- **A. Rust `std` on ESP-IDF via `esp-idf-svc` (chosen — owner accepted
  recommendation).** Wi-Fi, HTTPS (mbedTLS + CA bundle), redirects, SNTP,
  deep sleep, NVS, LEDC from mature ESP-IDF; app code in Rust; `serde_json`
  works as on the PC.
- B. Bare-metal Rust (`esp-hal` + Embassy) — rejected: weak TLS validation,
  DIY HTTP for multi-MB downloads.
- C. C on ESP-IDF — rejected: owner prefers Rust; A is nearly as mature.

### 6.4 Flashing COM7 from WSL2
- **usbipd-win (chosen — owner will install it).** `make flash` attaches the
  USB device to WSL, `espflash` runs in Docker.
- Windows-side `espflash.exe` — rejected: tool outside Docker.

## 7. Design

### 7.1 Update cycle (one pass per wake)

```
wake (timer | KEY0 | power-on)
  │
  ├─ timer wake but RTC says > 2 min before 06:00 → sleep remainder (no Wi-Fi)
  ├─ init: LED on, SD off, battery read
  ├─ Wi-Fi connect (timeout 20 s); SNTP → write PCF8563
  ├─ photo list missing or older than 7 days → refresh list (7.3)
  ├─ pick a fully random photo; if tall, add a tall partner from the newest 300
  ├─ per slot: GET <url>=w{800|398}-h480-p-rj → JPEG (30–200 KB) → decode →
  │     veil the parts under the boxes (overlay_opacity) → dither into its area
  │     of the 4 bpp framebuffer (192 KB); on 403/404 retry with a new pick
  │     (max 3) and mark the list for refresh on next wake
  ├─ save the photo screen to flash (fallback)
  ├─ GET Open-Meteo forecast; GET + stream each calendar (iCal) → today's events
  ├─ draw events box (left) and weather box (right); Wi-Fi off
  ├─ panel: reset → init → 0x10 + frame → refresh → wait BUSY → power off → sleep
  ├─ beep (success)
  └─ compute next wake → deep sleep (timer + KEY0)
```

Button presses while awake are ignored. Global watchdog: 120 s for a normal
update, 240 s when the list is refreshed; a timeout is a failure.

### 7.2 Failure handling (builder's call, owner may override)

| Failure | Behaviour |
|---|---|
| Wi-Fi / SNTP fails | Don't touch screen. Error beep. Retry in 30 min, max 3 times, then wait for next 06:00 |
| List refresh fails | Keep using the old list if one exists; else treat as photo failure |
| Photo download / decode fails | Cached last photo with fresh weather; if no cache, keep old screen as above |
| Weather fails | Photo + box saying "weather unavailable" |
| Calendar fails | Events box says "calendar unavailable" (one failing calendar of several is skipped) |
| Panel BUSY timeout | Error beep, sleep as normal |
| Battery < 10 % | Still update; box shows low-battery marker |

Beeps: success = one short 2 kHz beep (~150 ms); error = three short 1 kHz beeps.

### 7.3 Photo source — verified on PC (`pc/`, 2026-11-25)

A typical album: a live album with several thousand items (still growing as Google
adds matches), mostly portrait.

1. **Share page.** GET the share link (`photos.app.goo.gl/...`), follow
   redirects to `photos.google.com/share/<album_id>?key=<key>`. Album id and
   key come from that final URL. The HTML (~1.4 MB) embeds the first page as
   JSON in `AF_initDataCallback({key: 'ds:1', hash: '…', data:<PAYLOAD>, sideChannel: {}})`.
   On the device, stream the body and keep only the `data:` … `, sideChannel:`
   slice (~200 KB) in PSRAM.
2. **Next pages.** `POST https://photos.google.com/_/PhotosUi/data/batchexecute?rpcids=snAcKc`,
   form field `f.req=[[["snAcKc","[\"<album_id>\",\"<token>\",null,\"<key>\"]",null,"generic"]]]`.
   No cookies/session values needed. Response: `)]}'` then length-prefixed
   JSON chunks; the line starting `[["wrb.fr","snAcKc",` has PAYLOAD as an
   escaped JSON string at `[0][2]`. ~200 KB, ~0.8 s each on PC.
3. **PAYLOAD**: `[null, [ITEM…≤300], next_token|null, [album_id, title, …, [21]=total, …], …]`,
   ITEM `[media_id, [base_url, width, height, …], timestamp, …]`.
   Whole album on PC: 29 pages, 6.8 MB, 23 s.
4. **Photo list cache.** Store `width height base_url` (~150 B each; ~1.3 MB now) in a
   flash data partition sized for ≥ 20 000 photos (≈ 3 MB), with the refresh
   time in NVS. Refresh weekly, when missing, or after a photo 403/404.
   Reuse one TLS connection (keep-alive) for all pages.
5. **Download.** `<base_url>=w800-h480-p-rj`: `p` = Google smart crop (keeps
   the person; chosen over centre crop `c` after comparing on the owner's
   photos), `rj` = force JPEG (without it PNG originals come back as PNG).
   Always a **baseline** 800×480 JPEG, 30–200 KB, ~1 s.
6. **Dithering**: error diffusion to the 6-colour palette using measured
   Spectra 6 colour values; exact palette/algorithm is the builder's call,
   judged by eye on the device.

The parsing code in `pc/src/album.rs` (serde_json, 9 unit tests) is what the
firmware reuses.

**Open (from the builder check): how long do cached base URLs stay valid?**
Everything was verified on one day. If URLs expire in less than a week, the
list-in-flash design must change (e.g. cache page tokens and fetch one random
page per wake). The emulator caches its list for 7 days and logs the list age
when a cached URL fails — run `make emulator` on several days this week to
find out before the firmware's storage code is written.

### 7.4 Weather

- Open-Meteo, no key:
  `https://api.open-meteo.com/v1/forecast?latitude=32.0853&longitude=34.7818&hourly=temperature_2m,precipitation_probability,weather_code&timezone=Asia%2FJerusalem&forecast_hours=7`
  (Tel Aviv ≈ 32.0853 N, 34.7818 E; configurable.)
- Slots: current hour + next 5 (update at 06:01 → 06…11).
- WMO `weather_code` → icons: clear, partly cloudy, cloudy, fog, drizzle/rain,
  showers, thunder, snow. Drawn with primitives or small bitmaps — builder's call.

### 7.5 Screen layout (owner-approved in the emulator)

Photo(s) fill 800×480. Two mirrored boxes along the bottom, 388×96 each,
8 px margins: **together ≤ 20 % of the screen** (owner's limit; unit-tested).
Boxes are **50 % transparent** by default (`overlay_opacity`, 0–100): the
photo under them is blended toward white before dithering, labels get a 1 px
white outline. 100 = solid white box.

```
┌──────────────────────────────────────────────────────────────┐
│                      photo(s), full screen                   │
│┌──────────────────────────────┐┌────────────────────────────┐│
││Wed 25 Nov   Tel Aviv 06:00││13:00 14:00 15:00 ... 18:00 ││
││13:00-14:00  Дзюдо            ││ ☁    ⛅    ⛅    ☀     ☀    ││
││all day      Hanukkah         ││ 22°  21°   21°   20°   17° ││
││             +2 more          ││ 0%   0%    0%    0%    0%  ││
│└──────────────────────────────┘└────────────────────────────┘│
└──────────────────────────────────────────────────────────────┘
```

- Events box: date, then place + update time (+ battery % on the device);
  4 rows `HH:MM-HH:MM title` (`all day`, `22:00-...` past midnight); with
  more events the last row is `+N more`. Empty day: "No more events today".
- Weather box: 6 columns `HH:00`, icon, temperature, rain %.
- Fonts: Helvetica bold (u8g2) for labels; titles and place in `cu12`
  Cyrillic+Latin and `cu12` Hebrew, Hebrew drawn right-to-left.

### 7.6 Time and wake scheduling

- TZ: POSIX `IST-2IDT,M3.4.4/26,M10.5.0` (Asia/Jerusalem incl. DST).
- Time source: PCF8563, set from SNTP after each Wi-Fi connect. The internal
  RC clock drifts up to several % in deep sleep, so: sleep until 06:00; on
  timer wake read PCF8563 over I2C (no Wi-Fi); if > 2 min early, sleep the
  remainder again.
- A button wake does not shift the 06:00 schedule.
- First boot (RTC invalid): update immediately, then schedule normally.

### 7.7 Configuration

`config.toml` in repo root, **gitignored**; `config.example.toml` committed;
baked into the firmware at build time. Build fails clearly if missing.

```toml
album_url       = "https://photos.app.goo.gl/XXXX"
city            = "Tel Aviv"            # weather by name (Open-Meteo geocoding)
place_name      = "Tel Aviv"      # optional label
latitude        = 32.0853            # optional exact point (both or neither)
longitude       = 34.7818
calendars       = ["https://calendar.google.com/calendar/ical/.../private-.../basic.ics"]
overlay_opacity = 50                 # optional, default 50
wifi_ssid       = "..."              # firmware only
wifi_password   = "..."
update_hour     = 6                  # firmware only, default 6
```

The album and calendar links are in the device flash; anyone with the device
or a link can read them. Accepted by design (links can be reset and reflashed).

### 7.8 Code structure

```
core/      pure logic, host-tested, runs on the ESP32 (small crates only)
  album.rs  screen.rs  dither.rs  framebuf.rs  layout.rs  text.rs
  weather.rs  calendar.rs  date.rs  tz.rs
pc/        PC tools (ureq, image, toml — never on the device):
           `make pc-check`, `make emulator` (one device update -> out/screen.png)
firmware/  ESP32-S3 binary on esp-idf-svc (see the firmware plan)
```

### 7.9 Build, test, flash

- **Dockerfile**: `ubuntu:26.04` (owner's call) + build-essential + Rust
  1.100.0 via rustup (exists now). The firmware toolchain (espup Xtensa Rust,
  ESP-IDF v5.x pre-fetched, `espflash`, `ldproxy`) is added to the same image
  with the firmware. Containers run as the host user; cargo cache in
  `.cargo-docker/`, build output in `build/` (both gitignored).
- **`make`**: build image if needed, build release firmware → `build/frogterm.bin`.
- **`make clean`**: remove `build/`, Cargo targets, cache volumes (not the image).
- **`make tests`**: `cargo test` for `core` and `pc` inside Docker:
  album parsing (synthetic fixtures — never commit real album data),
  weather parsing (recorded Open-Meteo response), schedule across DST
  (27 Mar 2026, 25 Oct 2026) and year rollover, dither palette output,
  layout bounds + `build/preview.png`.
- **`make flash`**: if no `/dev/ttyUSB*`/`/dev/ttyACM*`, run
  `usbipd.exe attach --wsl --hardware-id <VID:PID>`, wait for the device,
  then `espflash flash` in Docker with `--device`. `PORT=` overrides.
- **`make monitor`**: serial log. **`make pc-check`**: PC album check.
  **`make emulator`**: one device update on the PC → `out/screen.png`
  (opened in Windows automatically).
- One-time Windows setup (README): `winget install usbipd`, `usbipd list`,
  `usbipd bind --hardware-id <VID:PID>`.

### 7.11 Calendar (verified against the Google Calendar API)

Each configured iCal link is streamed through `core::calendar::IcsScanner`
(one VEVENT in memory at a time; Family ≈ 100 KB, holidays ≈ 510 KB).
Supported: UTC / TZID / all-day values, multi-day events, RRULE
DAILY/WEEKLY/MONTHLY/YEARLY with INTERVAL, UNTIL, COUNT, BYDAY (incl. `-1WE`),
BYMONTHDAY, BYMONTH, WKST; EXDATE; RECURRENCE-ID overrides (moved/edited);
STATUS:CANCELLED. Israel time via `core::tz` (no tz database). Titles lose a
leading/trailing `HH:MM`. Only events not yet over are shown.

### 7.10 Acceptance (on real hardware)

1. `make flash` from WSL works with only Docker, make, usbipd-win installed.
2. After flash: full-screen album photo + weather box; beep.
3. Green button → different photo + beep.
4. With a debug wake-time override, it wakes on time and updates.
5. Wi-Fi off → no screen change, error beep, sleeps.
6. First boot builds the photo list (log shows all pages); later boots skip it.
7. Deep-sleep current ≤ 50 µA.

## 8. Risks

| Risk | Mitigation |
|---|---|
| Google changes share page or `snAcKc` request | Parsing isolated in `core/album.rs`; `make pc-check` reproduces on PC in seconds; cached list + cached photo keep the frame working meanwhile |
| ED2208 driver port has no Rust prior art | First hardware milestone: draw 6 colour bars |
| WSL kernel lacks driver for the USB-UART bridge | Check on first attach; fallback Windows `espflash.exe` (documented only) |
| Deep-sleep clock drift | RTC re-check loop (7.6) |
| List refresh too slow on ESP32 (6.8 MB, 29 requests) | Keep-alive, weekly only, 180 s budget; if still too slow, cache page tokens instead |

## 9. Left to the builder

Crate choices (JPEG decoder, fonts), palette and dithering, icon art, font
sizes, timeouts, logging, partition sizes, Docker image tag, Cargo layout,
and any "builder's call" item above that proves wrong on real hardware.
