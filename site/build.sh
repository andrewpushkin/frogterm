#!/bin/sh
# The GitHub Pages site: site/index.html with the release filled in, the
# screenshot, and the firmware for the page's browser installer (ESP Web
# Tools needs the files on the same site).
#
#   site/build.sh VERSION OWNER/REPO OUT_DIR
#
# Needs build/frogterm.bin and build/frogterm-full.bin (`make firmware`).
set -eu
version=$1 repo=$2 out=$3
date=$(date -u +"%-d %B %Y")
mkdir -p "$out"
sed -e "s|{{VERSION}}|$version|g" -e "s|{{DATE}}|$date|g" -e "s|{{REPO}}|$repo|g" site/index.html > "$out/index.html"
cp docs/emulator.png "$out/"
# the installer writes only these parts, so the device's settings (NVS at
# 0x9000) stay unless the owner chooses to erase the device
head -c 32768 build/frogterm-full.bin > "$out/bootloader.bin"                       # 0x0
tail -c +32769 build/frogterm-full.bin | head -c 4096 > "$out/partitions.bin"     # 0x8000
cp build/frogterm.bin "$out/firmware.bin"                                         # 0x10000
cat > "$out/manifest.json" <<EOF
{
  "name": "frogterm",
  "version": "$version",
  "new_install_prompt_erase": true,
  "builds": [
    {
      "chipFamily": "ESP32-S3",
      "parts": [
        { "path": "bootloader.bin", "offset": 0 },
        { "path": "partitions.bin", "offset": 32768 },
        { "path": "firmware.bin", "offset": 65536 }
      ]
    }
  ]
}
EOF
echo "site for frogterm $version in $out"
