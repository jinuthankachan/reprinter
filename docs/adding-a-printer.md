# Adding a printer

reprinter separates two things:

- A **printer profile** (`printers/<id>.conf`) describes one printer model:
  how to recognise it, which driver it needs, which PPD and defaults to use.
- A **driver module** (`drivers/<name>.sh`) knows how to install one driver
  on every supported host (architecture, distribution release) and what host
  tweaks it needs. Several profiles can share one driver.

`install.sh` and `lib/` are generic: they never mention a specific printer or
driver. Adding a printer should only touch `printers/`, `drivers/` (when the
driver is new) and `docs/`.

## Case 1: the driver already exists

Example: the Canon LBP3200, which captppd already supports.

1. Plug the printer in and install the driver through any profile that uses
   it: `./install.sh --printer canon-lbp1210` installs captppd, then stops at
   detection and lists the `captusb://` URIs it did see.
2. Collect the facts:

   ```sh
   lsusb                                    # USB ID, e.g. 04a9:2636
   lpinfo -m | grep -i lbp3200              # PPD name, e.g. LBP3200CAPTPPD.ppd
   sudo lpinfo --include-schemes captusb -v # device URI the backend reports
   ```

3. Copy `printers/canon-lbp1210.conf` to `printers/<id>.conf` and fill in the
   fields (table below). Profile ids are `<vendor>-<model>`, lowercase.
4. Check it is listed and detected: `./install.sh --list`.
5. Install for real: `./install.sh --printer <id>`, then print a test page,
   a multi-page PDF and a second job after the first finished.
6. Add `docs/printers/<id>.md` (copy the LBP-1210 page) and a row to the
   "Supported printers" table in the README.

### Profile fields

Profiles are bash files sourced by `install.sh`; keep them to plain
assignments.

| Field | Required | Meaning |
|-------|----------|---------|
| `PRINTER_MODEL` | yes | Human-readable name, used in messages and the queue description |
| `PRINTER_USB_IDS` | no | Space-separated `vendor:product` IDs (lowercase hex, as in `lsusb`) for auto-detection. Empty: never auto-detected, `--printer` required. |
| `PRINTER_DRIVER` | yes | Driver module name: `drivers/<name>.sh` |
| `PRINTER_PPD` | yes | Model passed to `lpadmin -m`: a PPD name from `lpinfo -m`, or `everywhere` for driverless printers |
| `PRINTER_URI_REGEX` | yes | Extended regex that picks this printer's URI out of `lpinfo -v` for the driver's scheme. Match the model loosely (spaces may appear as `%20`, hyphens may be dropped) but specifically enough not to match sibling models. |
| `PRINTER_QUEUE` | yes | Default queue name: letters, digits, `_ . -` |
| `PRINTER_OPTIONS` | no | Space-separated `key=value` defaults passed as `lpadmin -o`, e.g. `Resolution=600dpi`. Paper size comes from `--paper`, not from here. |

## Case 2: a new driver

Write `drivers/<name>.sh`, using `drivers/captppd.sh` as the template. A
driver module is sourced after the profile and must define:

| Name | Kind | Contract |
|------|------|----------|
| `DRIVER_URI_SCHEME` | variable | Scheme of the CUPS backend that drives the printer (`captusb`, `usb`, `hp`, ...). `lpinfo -v` is limited to it. |
| `driver_install` | function | Install the driver. Must be idempotent (fast no-op when the pinned version is already installed) and honour `FROM_SOURCE=1`. |
| `driver_configure` | function | Host tweaks the driver needs (kernel modules, udev rules, ...). Runs after `driver_install`, on every run. May be empty. |
| `driver_diagnose` | function | Print hints when the printer is not found; called just before `install.sh` gives up. |

Available to modules:

- Variables: `ARCH` (dpkg architecture: `amd64`, `arm64`, `armhf`, ...),
  `OS_ID`, `OS_PRETTY`, `WORKDIR` (temporary directory, removed on exit,
  readable by apt), `FROM_SOURCE` (0 or 1), and the `PRINTER_*` profile fields.
- Helpers from `lib/common.sh`: `log`, `info`, `warn`, `die`, `have`,
  `as_root` (runs as root directly or through sudo), `pkg_install` (apt, runs
  `apt-get update` once), `build_jobs` (parallel jobs that fit in RAM),
  `version_ge`.

Rules for driver modules:

- **Pin everything.** A fixed upstream version; SHA-256 digests for
  downloaded packages; the expected commit for source builds. Refuse to
  install on a mismatch.
- **Install as a package** (prebuilt, or a `.deb` built locally with `cpack`
  or `dpkg-deb`) rather than `make install`, so `apt remove` undoes it.
- **Prefer the prebuilt package, fall back to source.** Check that a prebuilt
  package is installable (`apt-get install -s`) before installing it, so a
  dependency mismatch on one release turns into a source build instead of a
  half-installed system.
- **Do not create queues or change CUPS settings.** `lib/cups.sh` owns that.
- Keep driver-specific explanations in the module's comments and in
  `docs/printers/<id>.md`.

## Testing

`tests/container-test.sh` runs the whole install in throwaway Docker
containers and creates the queue on a fake device URI, so it needs no
printer. It checks the queue, its defaults, sharing, and that a rerun
updates the queue in place.

```sh
PROFILE=<id> tests/container-test.sh                         # Debian 12/13, Ubuntu 22.04/24.04
PROFILE=<id> FROM_SOURCE=1 tests/container-test.sh debian:13 # source build
```

It cannot test the USB side. Before marking a printer supported, print on
real hardware from at least one x86 and one ARM host if you can, and from a
Windows, macOS, Linux or mobile client over the network.
