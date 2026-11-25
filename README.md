# frogterm

Firmware for a Seeed **reTerminal E1002** (ESP32-S3, 7.3" six-colour E Ink
Spectra 6 panel) that turns it into a family photo frame:

- a random photo from a **Google Photos** album (portrait photos come in pairs,
  one of them from the newest 300), with the date it was taken in its
  top-right corner,
- **today's events** from Google Calendar (tomorrow's once today is done),
- the **weather** for the next 6 hours (rain chance, or the UV index when high),
- **room temperature** and **battery level**.

It updates every day at 06:00 and whenever the green button is pressed, then
deep-sleeps (≈14 µA). Everything is written in Rust, and everything builds in
Docker.

```
┌──────────────────────────────────────────────────────────────┐
│                       photo(s), full screen                  │
│┌──────────────────────────────┐┌────────────────────────────┐│
││Wed 25 Nov  Tel Aviv 06:00 ⌂22° ▭96%││06:00 07:00 ... 11:00  ││
││08:00-08:45  Математика       ││ ☀    ☀   ⛅   ☂            ││
││13:00-14:00  Swim             ││ 14°  16°  18°  19°         ││
││all day      Birthday         ││ UV 7      30%  60%         ││
│└──────────────────────────────┘└────────────────────────────┘│
└──────────────────────────────────────────────────────────────┘
```

## What you need

- Windows with WSL2, Docker inside WSL, `make`
- [usbipd-win](https://github.com/dorssel/usbipd-win) to give WSL the device's USB port
- a Google Photos album and (optionally) Google calendars — see below

Rust, the ESP32 toolchain, ESP-IDF and espflash are all in the Docker image,
which is built automatically the first time (`ubuntu:26.04`).

## Configure

```sh
cp config.example.toml config.toml
```

`config.toml` is gitignored and is the **only** place for private data. The
firmware has it baked in, so after changing it, flash again.

## Where to get your real data

### Photos: `album_url`

1. On [photos.google.com](https://photos.google.com): **Albums → New album**.
   For a frame that fills itself, choose **Automatically add photos of people
   & pets** and pick the faces (a "live album").
2. Open the album → **Share** (or **⋮ → Options**) → turn on **Link sharing**
   → **Copy link**. It looks like `https://photos.app.goo.gl/AbCdEf123`.
3. Anyone with this link can see the album. To revoke it, turn link sharing
   off and on again (new link) and flash again.

Check it on the PC: `make pc-check` reads the whole album without a login.

### Calendars: `calendars`

For **your own** calendars, use the *secret* iCal address:

1. On a computer, open [calendar.google.com](https://calendar.google.com) →
   ⚙️ **Settings**.
2. In the left column scroll down to **Settings for my calendars** and click
   the calendar.
3. Scroll to **Integrate calendar** → **Secret address in iCal format** → copy.
   It contains `/private-…/basic.ics` and works like a password.
   (The *public* address only works if the calendar is public.)

For **public** calendars (holidays), use the public address, e.g.
`https://calendar.google.com/calendar/ical/en.jewish%23holiday%40group.v.calendar.google.com/public/basic.ics`.

All calendars are merged. Repeating events, edited or cancelled occurrences
and all-day events are supported; a time written in the title ("13:00 Swim")
is removed because the time column shows it.

### Weather: `city`, `latitude`, `longitude`

Weather comes from [Open-Meteo](https://open-meteo.com) (free, no key).
`city` is looked up by name. For a specific neighbourhood, set `latitude` and
`longitude`: in Google Maps right-click the place — the first menu line is
`latitude, longitude`. `place_name` is the label shown on the screen.

### Time zone: `timezone`

A POSIX TZ string; the default is Israel, `IST-2IDT,M3.4.4/26,M10.5.0`
(UTC+2, summer time UTC+3 from the Friday before the last Sunday of March to
the last Sunday of October). The device sets its clock from the internet on
every Wi-Fi connection; this string only turns UTC into local time.

### Wi-Fi: `wifi_ssid`, `wifi_password`

Your 2.4 GHz network (the ESP32 has no 5 GHz). WPA2 or open.

### Other settings

`overlay_opacity`, `update_hour`, `photo_batch`, `refresh_minutes` — see the
comments in `config.example.toml`.

## Try it on the PC first

```sh
make emulator     # the device in a window, with its three buttons to click
make emulator ARGS="--room 22 --battery 90 --seed 3"   # preview the header, fixed photos
make emulator ARGS=--no-rest       # no e-paper rest between refreshes
make emulator ARGS="--once next"   # no window: one wake (green|next|previous) -> out/screen.png
make pc-check     # the Google album works without a login?
make tests        # all unit tests
```

The emulator runs the firmware's own wake logic (`core/src/app.rs`): click
**< Previous**, **Update** (the green button) and **Next >** (or press
Left / Space / Right) and it does exactly what the device does — stored
photos, batches, the panel's rest between refreshes, the 06:00 timer wake.
`out/` is its flash (stored screens, weather/calendar, state); delete it for
a factory-fresh device. The window shows the LED (on while awake) and the
last log line; the full log is in the terminal.

In WSL the window appears through WSLg. On Windows itself (no Docker), with
[Rust](https://rustup.rs) installed:

```powershell
cargo run --release -p frogterm-pc -- emulator
```

## Build and flash

One time, in an **administrator** PowerShell on Windows:

```powershell
winget install usbipd
usbipd list                      # find the reTerminal (USB serial), note its BUSID, e.g. 2-3
usbipd bind --busid 2-3
```

Then in WSL:

```sh
make                              # firmware -> build/frogterm.bin
make flash USBIPD_BUSID=2-3       # attaches the USB port to WSL and flashes
make monitor                      # serial log (Ctrl+C to quit)
make flash FEATURES=bars          # test build: six colour bars only
```

## Using it

| Sound | Meaning |
|---|---|
| short high chirp | button press noticed |
| one beep | update finished |
| three low beeps | update failed (retried in 30 min, up to 3 times) |

- **Green button**: a stored photo not seen yet; once every stored photo has
  been on the screen, a new batch is downloaded. The 06:00 update does the
  same. Presses during an update chirp and are ignored. The panel rests
  between refreshes (E Ink care); an earlier press is carried out
  automatically when it may.
- **Side buttons**: previous / next stored photo, no Wi-Fi. Photos seen this
  way count as seen.
- **No Wi-Fi most of the time**: the device keeps a batch of 10 ready-to-show
  photos in flash. A wake within 8 h of the last weather/calendar fetch (same
  day) shows a stored photo without Wi-Fi (~1.6 s awake). Older data is
  fetched again; when all stored photos were seen, a new batch is downloaded.
- **Battery**: a refresh is skipped below 3.45 V (no half-drawn screens).

## How it works

- `core/` — everything testable on a PC that also runs on the ESP32: Google
  album reading, dithering to the six inks, layout and fonts (Latin, Cyrillic,
  Hebrew right-to-left), weather, iCal calendar, Israel time zone, schedule,
  panel protocol, and what one wake does (`app.rs`, behind a `Device` trait).
- `pc/` — the emulator (a PC `Device` in a window) and the album check.
- `firmware/` — the reTerminal `Device`, with `esp-idf-svc`: e-paper on
  SPI, RTC and temperature sensor on I2C, buzzer, battery, Wi-Fi, HTTPS,
  flash storage, deep sleep.
- `docs/superpowers/` — design and implementation plan.

Flash wear: photo slots are rewritten about once per batch, the weather and
calendar go to wear-levelled NVS, the album list weekly — far below the
~100 000 erase cycles of the flash.

## Credits

- ED2208 panel init sequence: Zephyr `drivers/display/display_ed2208_gca.c`
  (Apache-2.0), matching Seeed_GFX.
- Battery voltage-to-percent table: Seeed's reTerminal E-series ESPHome cookbook.

## License

MIT — see [LICENSE](LICENSE).
