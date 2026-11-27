#!/bin/sh
# Attaches the reTerminal's USB serial port (a Windows COM port) to WSL and shows it.
# usage: scripts/wsl_fix.sh [COMn | BUSID]   (default COM7; see: usbipd.exe list)
#
# The bus id changes with the USB socket, the COM port does not, so the bus id
# is looked up from the COM port in `usbipd list`.

USBIPD=$(command -v usbipd.exe 2>/dev/null || echo "/mnt/c/Program Files/usbipd-win/usbipd.exe")
ARG=${1:-COM7}

list=$("$USBIPD" list 2>&1 | tr -d '\r')
case "$ARG" in
    [0-9]*-[0-9]*) line=$(printf '%s\n' "$list" | grep -E "^$ARG[[:space:]]") ;;
    *) line=$(printf '%s\n' "$list" | grep -iF "($ARG)") ;;
esac
if [ -z "$line" ]; then
    echo "$ARG not found among the USB devices of Windows:"
    printf '%s\n' "$list"
    exit 1
fi
BUSID=${line%% *}
echo "$ARG is bus id $BUSID: $line"

case "$line" in
    *"Not shared"*)
        echo "not shared yet; once, in an administrator PowerShell, run:"
        echo "  usbipd bind --busid $BUSID"
        exit 1
        ;;
    *Attached*) echo "already attached to WSL" ;;
    *) "$USBIPD" attach --wsl --busid "$BUSID" 2>&1 || exit 1 ;;
esac

# wait for the serial device to appear
for _ in 1 2 3 4 5 6 7 8 9 10; do
    ls /dev/ttyUSB* /dev/ttyACM* >/dev/null 2>&1 && break
    sleep 1
done
found=
for dev in /dev/ttyUSB* /dev/ttyACM*; do
    [ -e "$dev" ] && ls -l "$dev" && found=1
done
[ -n "$found" ] || { echo "no serial device appeared in WSL"; exit 1; }
