# Canon LASER SHOT LBP-1210

| | |
|---|---|
| Profile | `canon-lbp1210` |
| USB ID | `04a9:2617` |
| Driver | [captppd](https://github.com/darkvision77/captppd) 1.0.0 (module `drivers/captppd.sh`) |
| PPD | `LBP1210CAPTPPD.ppd` ("Canon LBP1210, captppd 1.0.0") |
| Device URI | `captusb://Canon/LBP1210?drv=capt&serial=...` |
| Default queue | `LBP1210`, A4, 600 dpi |

## Why this printer needs a print server

The LBP-1210 is a CAPT ("winprinter") device. It has no page description
language of its own: the host renders every page and streams compressed
bitmaps (SCoA format) over USB, with a bidirectional handshake. So:

- A generic or raw CUPS queue cannot drive it; a real CAPT driver has to run
  on the machine the printer is plugged into.
- Canon's official Linux CAPT driver (`cndrvcups-capt` 2.71) ships x86 /
  x86_64 binaries only and depends on the `ccpd` daemon and 32-bit libraries.
  It does not run on ARM and is fragile on current x86 distributions.
- The open-source `captdriver` project (newer LBP2900/3000 family) does not
  support the LBP-1210 because it lacks SCoA.
- **captppd** (with its protocol library
  [libcapt](https://github.com/darkvision77/libcapt)) is an open-source,
  reverse-engineered CAPT v1 driver for the LBP800, LBP810, LBP1120,
  **LBP1210** and LBP3200. It is a `captusb://` CUPS backend built on libusb,
  plus PPDs and a USB quirks file that keeps the stock CUPS `usb` backend away
  from the printer. Release 1.0.0 ships prebuilt `amd64`, `arm64` and `armhf`
  Debian packages.

reprinter uses captppd on every host, x86 and ARM alike. Clients never need a
Canon driver: they send PDF over IPP and the server does all rendering.

## Features

Supported by captppd 1.0.0: 600 dpi, multi-page jobs, toner saving, toner
density (Darkness 1 to 5), media type (plain, plain L, transparency, thick,
thick H), image refinement, cleaning (CUPS web UI → Printers → `LBP1210` →
Maintenance menu).

Not implemented: 300 dpi and custom page sizes. The hardware cannot print
about 4 mm from the top edge. The parallel port is not supported; use USB.

## Troubleshooting

| Symptom | Check |
|---------|-------|
| `lpinfo -v` shows no `captusb://` line | `lsusb` must list `04a9:2617 Canon, Inc.`. Try another cable or port; run `sudo /usr/lib/cups/backend/captusb` by hand. |
| `install.sh` sees a `captusb://` URI but says it does not match | The printer reported a model string the profile does not expect. Rerun with `--uri '<that uri>'` and report the URI so the profile's `PRINTER_URI_REGEX` can be fixed. |
| Job stuck "Processing", nothing printed | `sudo cupsctl --debug-logging`, print again, then `grep '\] \[Job' /var/log/cups/error_log`. Confirm the queue uses `captusb://` (`lpstat -v`) and the PPD is "Canon LBP1210, captppd 1.0.0", not a Canon "CAPT ver.x" PPD. |
| Printed once, later jobs hang | Power-cycle the printer, then `sudo cupsenable LBP1210`. Report at https://github.com/darkvision77/captppd/issues with the log. |

## If captppd does not work for your unit

1. **Rebuild the driver from source**: `./install.sh --from-source`. This is
   also what happens automatically where no prebuilt package fits.

2. **USB over IP instead of a CUPS driver.** Run VirtualHere USB Server (free
   for one device) or `usbip` on the server, attach the printer over the
   network from a Windows or macOS client, and use Canon's native LBP-1210
   driver there. Only one client at a time can hold the printer, and that
   client needs the Canon driver. Keeps the bidirectional CAPT channel intact.

3. **Canon's own x86 driver.** On an x86 host it can run natively; on a
   Raspberry Pi only under emulation (`qemu-user-static` plus an i386 chroot
   running `ccpd` / `captfilter`), which is slow on a Pi 3B. Either way it is
   fragile across CUPS and kernel updates. Last resort.

## Sources

- captppd: https://github.com/darkvision77/captppd (release 1.0.0 assets: amd64 / arm64 / armhf .deb, x86_64 .rpm)
- libcapt (protocol and SCoA): https://github.com/darkvision77/libcapt
- captdriver wiki stating LBP1210 needs SCoA and is unsupported there: https://github.com/mounaiban/captdriver/wiki
- Canon CAPT Linux driver 2.71 (x86 only): https://asia.canon/en/support/0100459601
- LBP-1120 on Raspberry Pi write-up (older Boichat driver, same printer family): https://sudonull.com/post/76280-Canon-Laser-Shot-LBP-1120-printer-and-Raspberry-Pi-based-print-server
