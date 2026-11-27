# syntax=docker/dockerfile:1
# frogterm development image: everything needed to build, test and run the project,
# including the ESP32-S3 firmware. The host only needs Docker, make and usbipd-win.
#
# Used by the Makefile; you normally don't run it by hand.

FROM ubuntu:26.04

ARG DEBIAN_FRONTEND=noninteractive

# C toolchain (ring), ESP-IDF prerequisites, libudev for espflash.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential ca-certificates curl git wget xz-utils unzip \
      python3 python3-venv python3-pip cmake ninja-build pkg-config \
      libudev-dev libusb-1.0-0 flex bison gperf ccache dfu-util \
 && rm -rf /var/lib/apt/lists/*

# Rust, pinned. Installed system-wide and readable by any user, because the
# Makefile runs containers as the host user so build files stay owned by you.
ARG RUST_VERSION=1.100.0
ENV RUSTUP_HOME=/opt/rust/rustup \
    CARGO_HOME=/opt/rust/cargo \
    PATH=/opt/rust/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal \
        --default-toolchain ${RUST_VERSION} --component rustfmt,clippy \
 && rustc --version && cargo --version

# ESP32-S3 Rust tools: Xtensa toolchain ("esp"), flasher, linker proxy.
# espup asks the GitHub API for the toolchain release; the optional build secret
# github_token (the Makefile passes `gh auth token`) avoids its rate limit.
RUN --mount=type=secret,id=github_token,env=GITHUB_TOKEN \
    cargo install --locked espup@0.19.0 espflash@4.7.0 ldproxy@0.3.5 \
 && espup install --targets esp32s3 --export-file /opt/export-esp.sh \
 && rm -rf /opt/rust/cargo/registry /opt/rust/cargo/git

# ESP-IDF, the version esp-idf-sys 0.39 builds against by default.
ARG ESP_IDF_VERSION=v5.5.5
ENV IDF_PATH=/opt/esp-idf \
    IDF_TOOLS_PATH=/opt/espressif \
    ESP_IDF_TOOLS_INSTALL_DIR=fromenv \
    ESP_IDF_VERSION=${ESP_IDF_VERSION}
RUN git clone --depth 1 --branch ${ESP_IDF_VERSION} --recursive --shallow-submodules \
      https://github.com/espressif/esp-idf.git ${IDF_PATH} \
 && ${IDF_PATH}/install.sh esp32s3 \
 && rm -rf ${IDF_TOOLS_PATH}/dist

# Every container gets the ESP environment (Xtensa paths, ESP-IDF tools).
RUN printf '%s\n' '#!/bin/sh' \
      '. /opt/export-esp.sh' \
      '. /opt/esp-idf/export.sh >/dev/null 2>&1' \
      'exec "$@"' > /opt/entrypoint.sh \
 && chmod +x /opt/entrypoint.sh \
 && chmod -R a+rX /opt
ENTRYPOINT ["/opt/entrypoint.sh"]

# X11 client libraries for the emulator window (shown by WSLg or the Linux
# desktop). A layer of its own, so adding it did not rebuild the toolchains.
RUN apt-get update \
 && apt-get install -y --no-install-recommends libx11-6 libxcursor1 \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /work
