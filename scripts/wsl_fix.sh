"/mnt/c/Program Files/usbipd-win/usbipd.exe" attach --wsl
 --busid 5-2 2>&1; sleep 2; ls -l /dev/ttyUSB* /dev/ttyACM* 2>&1; lsusb 2>/dev/null | grep -i 1a86
