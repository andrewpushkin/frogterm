# Firmware update from the settings page — plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** upload a release's `firmware.bin` on the settings page; the device installs it into a second app slot, boots it, and rolls back if it fails.

**Spec:** `docs/superpowers/specs/2026-11-28-settings-portal-design.md`, section "Firmware update from the page".

## Global Constraints

- New partitions only after `screens`: `otadata` 0xA10000 (0x2000), `ota_0` 0xA20000 (0x300000), `ota_1` 0xD20000 (0x300000). Nothing else moves.
- Largest upload: 0x300000 bytes. ESP32-S3 chip id 9; app description magic 0xABCD5432 at offset 32.
- frogterm's version tag in every firmware: `<frogterm-version:N>` (N = `FT_VERSION`).
- `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`; the running firmware confirms itself only right before a normal deep sleep (not after a fatal error, the watchdog or a crash pause).
- USB flashing erases `otadata` and writes the app to `factory`.
- No new crates. Never flash unless asked. Commit after every green step, no Claude co-author line.

## Review Focus

- `firmware-full.bin` uploaded by mistake: refused with a message naming `firmware.bin`.
- An upload cut off half way, or a file over 3 MB: refused, the old firmware keeps running, settings mode goes on.
- A device still on the old partition table: the page says it needs one USB flash.
- The new firmware hangs (watchdog sleep) or panics: rolled back at the next restart.
- The version tag split across two network chunks: still found.

### Task 1: core — firmware image check, the page's Firmware box
- `core/src/firmware.rs`: `MAX_IMAGE`, `VERSION_PREFIX`, `check_header(head) -> Result<()>`, `TagScanner { feed, version }`, `check_image(bytes) -> Result<String>`.
- `portal::Host { wifi_needed, firmware }` replaces the `wifi_needed` argument of `handle`; the page shows `{{firmware_version}}` and the upload box (JS).
- Tests: real-looking headers (app / full image / other chip / short), tag across chunks, missing tag, page shows the version and the box.

### Task 2: emulator — `POST /update`
- `portal_server`: `/update` takes up to `MAX_IMAGE`, answers `check_image`'s result ("checked, the emulator installs nothing"); 16 KB stays the limit elsewhere. Socket test.

### Task 3: firmware — layout, bootloader, update handler, rollback
- `partitions.csv`, `sdkconfig.defaults`, `Makefile` (bootloader from the esp-idf build, `--target-app-partition factory`, `--erase-parts otadata`), `site/build.sh` (blank `otadata` part).
- `firmware/src/ota.rs`: version tag static, `confirm()`, `rolled_back()`; `portal.rs` `/update` streaming into `EspOta`; settings loop ends after an install (usable settings: `end_settings` + sleep; none: restart).
- build + clippy.

### Task 4: docs — README, page.
