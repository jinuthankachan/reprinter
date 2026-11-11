# Raspberry Pi as the print server

Any Raspberry Pi with USB works. The original target, and the slowest
supported board, is the Raspberry Pi 3B.

## Hardware and OS

| Item | Choice |
|------|--------|
| Board | Raspberry Pi 3B or newer (the 3B has 1 GB RAM and 2.4 GHz-only Wi-Fi) |
| OS | **Raspberry Pi OS Lite (64-bit)**, Trixie (Debian 13) or Bookworm (Debian 12) |
| Storage | 8 GB+ microSD |
| Power | 5 V / 2.5 A supply. Under-powered Pis drop Wi-Fi. |
| Printer | USB cable to the Pi |

Use the **64-bit** image. On 32-bit Trixie the prebuilt `armhf` driver
package cannot install (it depends on `libcups2`, which 32-bit Trixie renamed
to `libcups2t64` without a compatibility alias), so `install.sh` builds the
driver from source instead. That works, but compiling on a Pi 3B takes a long
time. 32-bit Bookworm and both 64-bit releases use the prebuilt package.

## Step 1: flash and bring the Pi onto the network

1. Raspberry Pi Imager → "Raspberry Pi OS Lite (64-bit)".
2. In the Imager's OS customisation, set the hostname (e.g. `printpi`), a
   username and password, Wi-Fi SSID and password (a 2.4 GHz network for a
   Pi 3B), the country, and **enable SSH**.
3. Boot, then `ssh <user>@printpi.local`.
4. On the router, give the Pi a DHCP reservation so its IP never changes.

## Step 2: install

Plug in and power on the printer, then on the Pi:

```sh
sudo apt install -y git      # if it is not there yet
git clone https://github.com/jinuthankachan/reprinter.git
cd reprinter
./install.sh
```

Or copy the directory from another machine with
`scp -r reprinter <user>@printpi.local:`.

Then add the printer on each client as described in the
[README](../README.md#adding-the-printer-on-clients).

## What to expect

- The first page of a job takes 20 to 40 s on a Pi 3B (Ghostscript renders at
  600 dpi); after that it prints at printer speed.
- If clients cannot see the Pi: it must be on the same network / VLAN as
  them (guest Wi-Fi is usually isolated), and a Pi 3B only joins 2.4 GHz
  networks.
