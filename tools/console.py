#!/usr/bin/env python3
"""Test the reTerminal over its USB-serial port: simulated button presses and
the log (runs in the Docker image, which has pyserial; see `make press`).

  console.py --port P press BUTTON [--secs N]  reset the chip and press BUTTON as
                                             the waking button, then the log
                                             until the device sleeps
  console.py --port P send BUTTON [--secs N]   press BUTTON now (device awake)
  console.py --port P log [--secs N]           the log, no reset

BUTTON: green, next, previous (right, left), "hold green", "hold next".
Opening the port resets nothing (DTR/RTS are changed in a safe order).
"""
import argparse
import sys
import time

import serial


def open_port(name):
    # opened with DTR and RTS both on (the auto-reset circuit does nothing
    # then), RTS goes off first: RTS on alone would pull EN low, a reset
    s = serial.Serial()
    s.port, s.baudrate, s.timeout = name, 115200, 0.2
    s.open()
    s.rts = False  # EN high: running
    s.dtr = False  # GPIO0 high: a reset boots the firmware, not the ROM loader
    return s


def reset(s):
    s.dtr = False
    s.rts = True  # EN low
    time.sleep(0.15)
    s.rts = False


def lines(s, secs):
    """The log's lines for `secs` seconds."""
    end, buf = time.time() + secs, b""
    while time.time() < end:
        buf += s.read(256)
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            yield line.decode("utf-8", "replace").rstrip("\r")


def show(line, start):
    print(f"[{time.time() - start:6.1f}] {line}", flush=True)


def follow(s, secs, start, until=None):
    for line in lines(s, secs):
        show(line, start)
        if until and until in line:
            return True
    return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", required=True)
    ap.add_argument("what", choices=["press", "send", "log"])
    ap.add_argument("button", nargs="?", default="")
    ap.add_argument("--secs", type=float, default=None)
    a = ap.parse_args()
    s, start = open_port(a.port), time.time()
    if a.what == "press":
        reset(s)
        # the firmware says when it listens (the first seconds after a reset)
        if not follow(s, 15, start, until="serial: type a button"):
            sys.exit("the device did not offer the serial window after the reset")
        s.write((a.button + "\n").encode())
        show(f">>> {a.button}", start)
        follow(s, a.secs or 300, start, until="deep sleep for")
    elif a.what == "send":
        s.write((a.button + "\n").encode())
        show(f">>> {a.button}", start)
        follow(s, a.secs or 60, start)
    else:
        follow(s, a.secs or 60, start)


if __name__ == "__main__":
    main()
