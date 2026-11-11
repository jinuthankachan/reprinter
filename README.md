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

On the host the printer is plugged into (printer connected and powered on),
as a user with sudo rights. Either way sets up the same print server.

**One file**, with the [web interface](#web-interface):

```sh
curl -fLo reprinter-web https://github.com/jinuthankachan/reprinter/releases/latest/download/reprinter-web-$(dpkg --print-architecture)
chmod +x reprinter-web
sudo ./reprinter-web install
```

`reprinter-web` carries `install.sh` and every printer profile inside it, so
nothing else is needed: it runs the steps below, then installs itself as the
web interface service. It takes the same options as `install.sh` plus a few
of its own (`./reprinter-web install --help`). The binaries (`amd64`,
`arm64`, `armhf`) are static and run on every supported release; the release
page has their `SHA256SUMS`. To build them yourself: `make release` (see
[Development](#development)).

**From the repository**, print server only:

```sh
git clone https://github.com/jinuthankachan/reprinter.git
cd reprinter
./install.sh
```

No git on the host? Copy the directory over instead:
`scp -r reprinter <user>@<host>:`.

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
   the default only if the host has no default printer yet. When printing
   fails, the queue holds the job and retries it every few minutes
   (`printer-error-policy=retry-job`) instead of stopping.
7. Removes other queues that reach the same printer through a different
   backend. A desktop often creates one on the stock `usb://` backend with a
   generic driver when the printer is plugged in before its driver is
   installed. Such a queue sends the printer data it does not understand,
   which can jam it until it is switched off and on.
8. Prints the URLs for clients.

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

## Web interface

`sudo ./reprinter-web install` (see [Quick start](#quick-start)) also sets up
a web page for printing and settings. Already set up with `install.sh`? Add
just the web interface with `sudo ./reprinter-web install --web-only`.

Open `http://<host>:8631/` from any browser, phone included. Upload a
PDF, image or text file, choose the page setup (copies, pages, paper size,
orientation, scaling, pages per sheet, the printer's own options, and
two-sided printing by hand for printers without a duplexer) and print it.
The Jobs page shows, retries and cancels jobs; the Settings page changes
printer defaults and sharing, adds printers from the profiles in `printers/`,
cleans up unwanted printers and stuck jobs, and sets an admin password. Jobs
that CUPS holds because printing failed are canceled after 30 minutes
(configurable), so they cannot clog the queue. Set the admin password: until
then anyone on the network can change the settings.

Details, options and security notes:
[docs/web-interface.md](docs/web-interface.md).

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
| A job shows "held" and nothing prints | Printing it failed and CUPS retries it every few minutes; `lpstat -l -o` shows why. Switch the printer off and on, then retry it (web interface: Retry, or `lp -i <id> -H resume`) or cancel it. |
| Several queues for the same printer | `lpstat -v`. Delete the ones not on the driver's backend (`sudo lpadmin -x <name>`, or Settings → Clean up in the web interface); rerunning `install.sh` removes them too. |
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
- The web interface (port 8631) lets anyone on the network print; set an
  admin password so that only you can change settings. It runs as an
  unprivileged `reprinter` user in the `lpadmin` group. See
  [docs/web-interface.md](docs/web-interface.md#security).

## Uninstall

For the default LBP-1210 setup:

```sh
sudo reprinter-web uninstall --purge   # if the web interface is installed
sudo lpadmin -x LBP1210
sudo apt remove captppd
sudo rm /etc/modprobe.d/blacklist-usblp.conf
sudo cupsctl --no-remote-any --no-remote-admin --no-share-printers
```

## Repository layout

```
install.sh               entry point
Makefile                 developer commands (run make to list them)
lib/common.sh            platform detection, packages, services
lib/cups.sh              CUPS stack, sharing, firewall, device discovery, queue
drivers/<driver>.sh      one module per driver: install, host tweaks, diagnostics
printers/<id>.conf       one profile per printer model
web/                     reprinter-web: web interface and one-file installer (src/, static/,
                         systemd unit, build-release.sh); embeds install.sh, lib/, drivers/, printers/
.github/workflows/       release builds of the reprinter-web binaries
docs/                    host guides, printer notes, web interface, contributor guide
tests/container-test.sh  Docker smoke tests (no printer needed)
tests/web-test.sh        Docker end-to-end tests of the web interface
```

A printer profile says which driver it needs and how to recognise the printer;
the driver module knows how to install that driver on any supported host. New
printers that use an existing driver only need a profile. See
[docs/adding-a-printer.md](docs/adding-a-printer.md).

## Development

`make` lists the developer commands:

| Command | What it does |
|---------|--------------|
| `make build` | Build `reprinter-web` for this machine (`web/target/release/`) |
| `make run` | Run the web interface on `http://127.0.0.1:8631/` against this machine's CUPS, with the printer profiles from the checkout. Settings and uploads go to `web/dev/`. Changing printers needs your user in `lpadmin`. |
| `make test` | Unit tests |
| `make lint` | `cargo fmt --check`, clippy, shell syntax, and shellcheck if it is installed |
| `make check` | `lint`, then `test` |
| `make test-install` | `install.sh` in throwaway Docker containers, with the queue on a fake device URI |
| `make test-install-source` | The same with the driver built from source |
| `make test-web` | `reprinter-web` end to end in Docker: one-step install, printing, settings, uninstall |
| `make test-all` | `check`, `test-install` and `test-web` |
| `make release` | Static `reprinter-web` binaries for amd64, arm64 and armhf, with `SHA256SUMS`, in `web/dist/` |
| `make install` | Build, then `sudo reprinter-web install` on this machine |
| `make uninstall` | Remove the web interface service from this machine; `PURGE=1` also removes its settings and the `reprinter` user |
| `make tag` | Tag `v<version>` from `web/Cargo.toml` for a release (does not push) |
| `make clean` | Remove build output, `web/dist/` and `web/dev/` |

Variables refine them:

```sh
make run PORT=9000
make release ARCHS=arm64
make test-install IMAGES="debian:12 ubuntu:24.04"
PROFILE=<id> make test-install                       # another printer profile
make install INSTALL_ARGS="--printer canon-lbp1210 --paper letter"
make uninstall PURGE=1
```

Building needs Rust ([rustup](https://rustup.rs)); `release` and `test-web`
build static binaries, and rustup adds the musl targets they need. The
`test-*` targets need Docker, but no printer: they cover Debian 12/13 and
Ubuntu 22.04/24.04.

### Releasing

1. Set `version` in `web/Cargo.toml` and commit.
2. `make tag`, then push the tag: `git push origin v<version>`.

The release workflow (`.github/workflows/release.yml`) then builds the three
binaries and attaches them, with `SHA256SUMS`, to the GitHub release. The
download link in [Quick start](#quick-start) always fetches the latest
release.
