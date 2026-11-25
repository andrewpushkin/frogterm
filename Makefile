# frogterm - reTerminal E1002 photo + weather + calendar frame
#
# Everything runs inside the frogterm-dev Docker image (see Dockerfile);
# the host needs only Docker, make and (for flashing) usbipd-win.

IMAGE  := frogterm-dev
CONFIG ?= config.toml
TTY    := $(shell [ -t 0 ] && echo -t)

# Run as the host user so build output stays yours; caches live in the repo (gitignored).
DOCKER := docker run --rm -i $(TTY) --init \
	-v "$(CURDIR)":/work -w /work \
	-u $(shell id -u):$(shell id -g) -e HOME=/tmp \
	-e CARGO_HOME=/work/.cargo-docker \
	-e CARGO_TARGET_DIR=/work/build/target \
	$(IMAGE)

PC := cargo run --release -q -p frogterm-pc --

FEATURES     ?=
ELF          := build/target/xtensa-esp32s3-espidf/release/frogterm
USBIPD       ?= $(shell command -v usbipd.exe 2>/dev/null || echo "/mnt/c/Program Files/usbipd-win/usbipd.exe")
USBIPD_BUSID ?=

.PHONY: all firmware flash monitor image tests emulator pc-check clean help

all: firmware

# Firmware for the reTerminal (config.toml is baked in) -> build/frogterm.bin
firmware: image
	@test -f config.toml || { echo "config.toml not found: cp config.example.toml config.toml and edit it"; exit 2; }
	$(DOCKER) sh -c 'cd firmware && cargo build --release $(if $(FEATURES),--features $(FEATURES))'
	$(DOCKER) espflash save-image --chip esp32s3 --flash-size 32mb --partition-table firmware/partitions.csv $(ELF) build/frogterm.bin
	@ls -l build/frogterm.bin

# Serial port of the reTerminal in WSL; attaches it with usbipd-win if needed.
FIND_PORT = port="$(PORT)"; \
	[ -n "$$port" ] || port=$$(ls /dev/ttyACM* /dev/ttyUSB* 2>/dev/null | head -1); \
	if [ -z "$$port" ]; then \
		if [ -z "$(USBIPD_BUSID)" ]; then \
			echo "reTerminal is not attached to WSL. Find its BUSID with:"; \
			echo "  \"$(USBIPD)\" list"; \
			echo "then run: make $@ USBIPD_BUSID=<busid>"; exit 2; fi; \
		"$(USBIPD)" attach --wsl --busid $(USBIPD_BUSID) || exit 2; \
		for i in 1 2 3 4 5 6 7 8 9 10; do \
			port=$$(ls /dev/ttyACM* /dev/ttyUSB* 2>/dev/null | head -1); [ -n "$$port" ] && break; sleep 1; done; \
	fi; \
	[ -n "$$port" ] || { echo "no serial device appeared in WSL"; exit 2; }

DOCKER_DEV = docker run --rm -i $(TTY) --init -v "$(CURDIR)":/work -w /work \
	-u $(shell id -u):$(shell id -g) -e HOME=/tmp \
	--device $$port --group-add $$(stat -c %g $$port) $(IMAGE)

flash: firmware
	@$(FIND_PORT); echo "flashing via $$port"; \
	$(DOCKER_DEV) espflash flash --chip esp32s3 --flash-size 32mb \
		--partition-table firmware/partitions.csv --port $$port $(ELF)

monitor:
	@$(FIND_PORT); $(DOCKER_DEV) espflash monitor --chip esp32s3 --port $$port --elf $(ELF)

# Rebuild the image only when the Dockerfile changes (stamp file in build/).
image: build/.image-stamp

build/.image-stamp: Dockerfile
	docker build -q -t $(IMAGE) . >/dev/null
	@mkdir -p build && touch $@

tests: image
	$(DOCKER) cargo test --workspace

# One device update on the PC: random photo(s) + weather + calendar -> out/screen.png
emulator: image
	@test -f "$(CONFIG)" || { echo "$(CONFIG) not found: cp config.example.toml config.toml and edit it"; exit 2; }
	$(DOCKER) $(PC) emulator --config "$(CONFIG)" --out out $(ARGS)
	@if command -v explorer.exe >/dev/null 2>&1 && command -v wslpath >/dev/null 2>&1; then \
		explorer.exe "$$(wslpath -w out/screen.png)" || true; fi

# Check the Google album: ALBUM=<link>, or album_url from $(CONFIG)
pc-check: image
	$(DOCKER) $(PC) check $(ALBUM) --config "$(CONFIG)" --out out $(ARGS)

clean:
	rm -rf build out target .cargo-docker firmware/target

help:
	@echo "make            build the firmware -> build/frogterm.bin (in Docker)"
	@echo "make flash      flash the reTerminal [USBIPD_BUSID=<id>] [PORT=/dev/ttyACM0] [FEATURES=bars]"
	@echo "make monitor    serial log of the reTerminal"
	@echo "make tests      run all unit tests"
	@echo "make emulator   one device update on the PC -> out/screen.png"
	@echo "                (ARGS=--refresh re-reads the whole album)"
	@echo "make pc-check   check the Google album [ALBUM=<link>] [ARGS='--index 3']"
	@echo "make clean      remove build output and caches"
