#!/bin/sh
# Attaches the reTerminal's USB serial port to WSL and shows it.
# usage: scripts/wsl_fix.sh [BUSID]   (default 5-2; see: usbipd.exe list)

BUSID=${1:-5-2}
USBIPD="/mnt/c/Program Files/usbipd-win/usbipd.exe"

"$USBIPD" attach --wsl --busid "$BUSID" 2>&1
sleep 2
ls -l /dev/ttyUSB* /dev/ttyACM* 2>&1
# the reTerminal's USB-serial chip (WCH, vendor 1a86)
lsusb 2>/dev/null | grep -i 1a86
