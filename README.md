# reprinter

Give old printers a second life as network printers.

Many older printers are host-based ("winprinters"): the computer does the
page rendering, and the only drivers ever made are for x86 Windows or old x86
Linux releases. reprinter turns a small Linux box (a Raspberry Pi, an old
laptop, a mini PC, a home server) into a CUPS print server that runs a working
driver for the printer. Every other device on the network prints to it over
IPP (AirPrint, IPP Everywhere, Mopria) and needs no driver at all.

## Supported printers

| Printer | Profile | Driver | Notes |
|---------|---------|--------|-------|
| Canon LASER SHOT LBP-1210 | `canon-lbp1210` | [captppd](https://github.com/darkvision77/captppd) 1.0.0 | [docs/printers/canon-lbp1210.md](docs/printers/canon-lbp1210.md) |

More printers are planned. The captppd driver also covers the Canon LBP800,
LBP810, LBP1120 and LBP3200, so each of those only needs a profile. See
[docs/adding-a-printer.md](docs/adding-a-printer.md).

## Supported hosts

| | |
|---|---|
| Distributions | Debian 12 / 13, Ubuntu 22.04 / 24.04 and newer, Raspberry Pi OS (Bookworm / Trixie), and their derivatives (Linux Mint, Pop!_OS, ...) |
| Architectures | **amd64 (x86-64)**, **arm64**, **armhf**: prebuilt driver package. Other architectures, and 32-bit Debian 13 / Ubuntu 24.04, build the driver from source automatically. |
| Init system | systemd, or SysV `service` (containers) |
| Not yet | Fedora / RHEL, Arch, openSUSE |

Any machine that runs one of these and has a USB port will do. For a
Raspberry Pi, start with [docs/raspberry-pi.md](docs/raspberry-pi.md).

## Quick start

On the host the printer is plugged into (printer connected and powered on):

```sh
git clone https://github.com/jinuthankachan/reprinter.git
cd reprinter
./install.sh
```

Run it as a user with sudo rights, or as root. No git on the host? Copy the
directory over instead: `scp -r reprinter <user>@<host>:`.

`install.sh` does the following:

1. Detects which supported printer is on USB (or takes `--printer`).
2. Installs `cups`, `cups-filters`, `ghostscript` and `avahi-daemon`.
3. Installs the printer's driver: the upstream prebuilt package for the host's
   architecture, verified by SHA-256, or a build from the pinned upstream
   source when no prebuilt package fits.
4. Applies host tweaks the driver needs (for CAPT printers, it blacklists the
   `usblp` kernel module).
5. Adds your user to `lpadmin`, opens CUPS to the LAN and shares printers
   (`cupsctl --remote-any --remote-admin --share-printers`), and opens IPP and
   mDNS in `ufw` or `firewalld` if one is active.
6. Finds the printer's device URI with `lpinfo -v` and creates the shared
   queue, or updates it in place if it already exists. The new queue becomes
   the default only if the host has no default printer yet.
7. Prints the URLs for clients.

Rerunning is safe: installed packages are skipped and the queue is updated in
place, so pending jobs are kept.

### Options

| Option | Default | Meaning |
|--------|---------|---------|
| `-p`, `--printer ID` | auto-detect | Printer profile from `printers/` (`--list` shows them) |
| `-q`, `--queue NAME` | profile's (e.g. `LBP1210`) | CUPS queue name; letters, digits, `_ . -` |
| `--paper SIZE` | `a4` | Default paper size as a CUPS media name (`letter`, `legal`, `a5`, ...) |
| `--uri URI` | from `lpinfo -v` | Device URI to use instead of searching for the printer |
| `--local` | off | Do not share on the network: local printing only, CUPS sharing settings untouched |
| `--from-source` | off | Build the driver from source even when a prebuilt package exists |
| `-l`, `--list` | | List supported printers, and whether each is connected |

`QUEUE_NAME` and `PAPER` in the environment work as defaults for `--queue` and
`--paper`.

```sh
./install.sh --list
./install.sh --printer canon-lbp1210 --queue Laser --paper letter
./install.sh --local         # an x86 desktop that only needs to print locally
```

Test from the host:

```sh
echo "hello" | lp -d LBP1210
lpstat -o
```

## Adding the printer on clients

Avahi advertises the queue as an IPP Everywhere / AirPrint printer, so most
clients find it on their own. In the URLs below, `<host>` is the print
server's IP address and `<queue>` the queue name (default `LBP1210`).

**Windows 10/11**
Settings → Bluetooth & devices → Printers & scanners → Add device. If it does
not appear within a minute: "Add manually" → "Select a shared printer by name"
→ enter

```
http://<host>:631/printers/<queue>
```

Driver prompt: Manufacturer "Microsoft" → "Microsoft IPP Class Driver". Any
PDF-capable generic driver also works. Do not install the printer's own driver
on the PC.

**macOS**
System Settings → Printers & Scanners → Add. The queue shows up via Bonjour;
pick it and leave "Use: AirPrint" or "Generic PostScript/PDF".

**Linux**
`cups-browsed` or the GNOME/KDE printer dialog discovers it. Manually:

```sh
lpadmin -p <queue> -E -v ipp://<host>:631/printers/<queue> -m everywhere
```

**Android / iOS**
Android's default print service and iOS AirPrint both discover it on the same
network.

## Troubleshooting

Printer-specific problems (detection, stuck jobs, driver bugs) are covered in
the printer's page under [docs/printers/](docs/printers/).

| Symptom | Check |
|---------|-------|
| `install.sh` says no supported printer found | `./install.sh --list` shows what it looks for; `lsusb` must list the printer's USB ID. Try another cable or port, or pass `--printer`. |
| Clients cannot see the printer | Same network / VLAN as the server; `systemctl status avahi-daemon`. Add it by IPP URL instead (above). |
| Windows "cannot connect" | Use `http://`, not `https://`, and the `:631` port. |
| Clients time out | A firewall on the server: open `631/tcp` (IPP) and `5353/udp` (mDNS). `ufw` and `firewalld` are handled automatically. |
| Web UI refuses the admin login | Log out and back in on the server so your user picks up the `lpadmin` group. |
| Change defaults (toner density, toner save, paper) | CUPS web UI → Printers → `<queue>` → Administration → "Set Default Options". |
| Anything else | `sudo cupsctl --debug-logging`, print again, read `/var/log/cups/error_log`. |

CUPS web UI: `http://<host>:631/` (the Administration tab asks for your login
on the server).

## Security notes

- Sharing mode enables `--remote-any` and `--remote-admin`: any network that
  can reach port 631 can print and open the CUPS web UI, and administration
  needs the password of a user in `lpadmin`. That is fine on a home LAN. Do not
  expose port 631 to the Internet; on such a host use `--local` or a firewall.
- Prebuilt driver packages are checked against pinned SHA-256 digests and
  source builds against a pinned upstream commit. A mismatch stops the install.

## Uninstall

For the default LBP-1210 setup:

```sh
sudo lpadmin -x LBP1210
sudo apt remove captppd
sudo rm /etc/modprobe.d/blacklist-usblp.conf
sudo cupsctl --no-remote-any --no-remote-admin --no-share-printers
```

## Repository layout

```
install.sh               entry point
lib/common.sh            platform detection, packages, services
lib/cups.sh              CUPS stack, sharing, firewall, device discovery, queue
drivers/<driver>.sh      one module per driver: install, host tweaks, diagnostics
printers/<id>.conf       one profile per printer model
docs/                    host guides, printer notes, contributor guide
tests/container-test.sh  Docker smoke tests (no printer needed)
```

A printer profile says which driver it needs and how to recognise the printer;
the driver module knows how to install that driver on any supported host. New
printers that use an existing driver only need a profile. See
[docs/adding-a-printer.md](docs/adding-a-printer.md).

Run the smoke tests (needs Docker; installs everything inside throwaway
containers and creates the queue on a fake device URI):

```sh
tests/container-test.sh                          # Debian 12/13, Ubuntu 22.04/24.04
FROM_SOURCE=1 tests/container-test.sh debian:13  # source-build path
```
