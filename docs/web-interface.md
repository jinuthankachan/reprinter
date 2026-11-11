# Web interface

`reprinter-web` is a small web server (Rust) that runs on the print server
next to CUPS. From any browser on the network, including a phone, you can:

- **Print a file**: upload a PDF, PostScript, JPEG, PNG, GIF, TIFF or plain
  text file, check the preview, choose the page setup and print it. No driver
  or app is needed on the device.
- **Watch the queue**: active and recent jobs on every printer, and why a
  job is not printing; retry or cancel jobs, one at a time or all at once.
- **Keep the queue clear**: remove unwanted printers, such as a second queue
  for the same printer on the wrong driver, and stuck jobs. Jobs that CUPS
  holds because printing them failed are canceled by themselves after a
  while (30 minutes by default).
- **Change settings**: printer descriptions, default options (paper size,
  toner density, ...), sharing, device URI; add printers from reprinter's
  profiles; CUPS network sharing; the web interface's own settings and
  admin password.

It complements CUPS rather than replacing it: devices that print over IPP
(AirPrint, Mopria, Windows) keep doing so, and everything the web interface
changes is ordinary CUPS configuration.

## Install

`reprinter-web` is a single file. It contains the web interface and
reprinter's installer (`install.sh` with `lib/`, `drivers/` and
`printers/`), so one command sets up the whole print server:

```sh
arch=$(dpkg --print-architecture 2>/dev/null || uname -m | sed 's/x86_64/amd64/; s/aarch64/arm64/; s/armv[67]l/armhf/')
curl -fLo reprinter-web https://github.com/jinuthankachan/reprinter/releases/latest/download/reprinter-web-$arch
chmod +x reprinter-web
sudo ./reprinter-web install
```

Then open `http://<host>:8631/`. The admin password for the Settings page is
printed at the end of the install.

`reprinter-web install`:

1. Runs the built-in `install.sh`: CUPS, the printer's driver, and a shared
   queue for the printer on USB (see the [README](../README.md#quick-start)).
   It is unpacked into a private temporary directory and removed afterwards.
2. Copies itself to `/usr/local/bin/reprinter-web`.
3. Creates the `reprinter` system user and adds it to the CUPS admin group:
   the first group in `SystemGroup` of `/etc/cups/cups-files.conf` other than
   `root` and `wheel`, so `lpadmin` on Debian and `sys` on Arch.
4. Writes `/var/lib/reprinter/config.toml` if it does not exist. When no
   admin password is set, it sets a random one and prints it at the end.
5. Installs and starts the `reprinter-web` systemd service, and opens its
   port in `ufw` or `firewalld` if one is active.

| Option | Meaning |
|--------|---------|
| `-p`, `--printer ID`, `-q`, `--queue NAME`, `--paper SIZE`, `--uri URI`, `--from-source` | Passed to `install.sh` (see the README's option table) |
| `-l`, `--list` | List supported printers, and whether each is connected |
| `--local` | Printer not shared on the network, and the web interface on 127.0.0.1 only |
| `--port PORT` | Web interface port (default 8631, or the port already configured). Port 80 works too. |
| `--web-only` | Skip the printer setup (CUPS and a queue are already there, e.g. from `install.sh`) |

Rerunning is safe: `install.sh` updates the queue in place, and the binary,
unit and service are replaced while the settings are kept. To update
reprinter-web, run the new binary's `install` (with `--web-only` to skip
the printer part).

```sh
sudo reprinter-web uninstall            # stop and remove the service and binary
sudo reprinter-web uninstall --purge    # also the settings and the reprinter user
```

Printers, CUPS and drivers stay; the README's Uninstall section removes them.

### The binaries

There is one per architecture, named after `dpkg --print-architecture`
(on Arch: `x86_64` is `amd64`, `aarch64` is `arm64`):

| File | Hosts |
|------|-------|
| `reprinter-web-amd64` | 64-bit PCs |
| `reprinter-web-arm64` | 64-bit Raspberry Pi OS and other 64-bit ARM boards |
| `reprinter-web-armhf` | 32-bit Raspberry Pi OS (ARMv6 and newer, so every Pi) and other 32-bit ARM |

They are statically linked (musl), so they do not depend on the host's C
library and run on every supported Debian, Ubuntu, Raspberry Pi OS and
Arch release. Check them against `SHA256SUMS` from the same release:
`sha256sum -c --ignore-missing SHA256SUMS`.

Pushing a version tag (`git tag v0.1.0 && git push origin v0.1.0`) builds
and publishes them (`.github/workflows/release.yml`). To build them
yourself, on any Linux PC with [rustup](https://rustup.rs):

```sh
make release                  # web/dist/reprinter-web-{amd64,arm64,armhf} and SHA256SUMS
make release ARCHS=arm64      # one architecture
```

No cross compiler is needed: Rust's own linker (`rust-lld`) links the musl
targets. Then copy the file to the host, e.g.
`scp web/dist/reprinter-web-arm64 <user>@printpi.local:reprinter-web`.

During development, `cargo run -- --share-dir ..` (from `web/`) reads the
printer profiles from the checkout instead of the built-in copy.

## Using it

### Print

1. Pick the printer. Its state is shown next to it.
2. Choose or drop a file. Other formats (Word, LibreOffice, HTML, ...) are
   refused: save them as PDF first.
3. Set up the pages and press **Print**.

| Setting | Notes |
|---------|-------|
| Copies, Collate | Collate appears from 2 copies |
| Pages | Pages and ranges, e.g. `1-3, 5`; empty prints all |
| Paper size, Two-sided, Paper source, Paper type, Color | Shown when the printer's driver has them |
| Orientation | Automatic, portrait or landscape |
| Scaling | Automatic, fit to page, fill page (crops), actual size |
| Pages per sheet | 1, 2, 4, 6, 9 or 16 |
| More options | Odd or even pages only, reverse order, and every other option of the printer's driver (resolution, toner density, toner saving, ...) |

**Two-sided by hand.** Printers without a duplexer, such as the LBP-1210,
get a "Two-sided by hand" option. It prints the odd pages, then shows what
to do: put the sheets back in the tray turned over, and print the even pages.
If the back sides come out in the wrong order, tick "Print back sides in
reverse order" (remembered by the browser). Which way to turn the stack
depends on the printer; try with a short document first.

Uploaded files stay on the server so they can be printed again (and for the
second pass of two-sided printing) until "Remove", or until they have not
been used for the configured time (60 minutes by default). They are also
deleted when the service restarts. So that uploads cannot fill the disk, the
server keeps at most 100 of them, taking at most 10 times the upload limit
together (500 MB by default); beyond that, uploads are refused until older
ones are removed or expire.

### Jobs

Active jobs show why they are not printing. A job is **stuck** when CUPS
holds it because printing it failed, for example because the printer did
not answer: CUPS then retries it every few minutes. Switch the printer off
and on, then press **Retry** to try again right away, or **Cancel**.
**Cancel all** cancels every active job (of the printer picked in the list).

Stuck jobs are canceled by themselves once they are older than "Cancel
stuck jobs after" (Settings → Web interface; 30 minutes by default, 0 turns
it off), so a forgotten job does not print hours later. Jobs someone held on
purpose (`lp -H hold`) and jobs waiting on a disabled printer are left alone.

### Settings

| Section | What it changes |
|---------|-----------------|
| Clean up | Shown when there is something to clean up: unwanted printers (remove them) and stuck jobs (retry or cancel them). |
| Printers | Description, location, device URI ("Detect" lists devices on the same backend), shared, accepting jobs, enabled; the driver's default options for jobs from every device; make default; test page; delete |
| Add a printer | Creates a queue from a profile in `printers/`, like `install.sh` does: pick the detected device, the queue name, default paper and sharing. The driver must be installed (`./install.sh --printer <id>`). |
| Network sharing | `cupsctl --[no-]share-printers --[no-]remote-any --[no-]remote-admin`. Saving restarts CUPS. |
| Web interface | Upload limit, how long uploads are kept, when stuck jobs are canceled, listen address (needs a restart: `sudo systemctl restart reprinter-web`), admin password |

A printer is **unwanted** when:

- it reaches the same printer as one of reprinter's queues, but through a
  different backend. Typically a desktop made it on the stock `usb://`
  backend with a generic driver when the printer was plugged in before its
  driver was installed. It sends the printer data it does not understand,
  which can jam it until it is switched off and on. `install.sh` removes
  these by itself.
- it only passes its jobs on to another queue on the same server
  (`ipp://localhost/printers/...`), as `cups-browsed` sometimes creates.

The Print page marks unwanted printers and never picks one by itself.

## Security

- **The settings need the admin password.** `reprinter-web install` sets a
  random one when there is none and prints it; change it under Settings →
  Web interface. Printing, the queue and retrying or canceling jobs (also all
  at once) stay open to everyone on the network, as with the printer itself.
  Removing the password ("Remove password") opens the settings to everyone
  who can open the page, including pointing a printer at another server,
  which then receives every document printed on it.
- The password is stored as an Argon2 hash in
  `/var/lib/reprinter/config.toml` (readable by the service only). Logins
  last 12 hours or until the service restarts. Passwords are checked one at a
  time, and a wrong one holds the next check back for a second, so guessing
  is slow and many logins at once cannot exhaust the server's memory.
- The server only answers to its own names: IP addresses, `localhost`, its
  host name and `<hostname>.local`. That stops DNS rebinding, where another
  web site points its own domain name at the server to use the API from a
  visitor's browser. To open the page by another name (e.g. one from your
  router's DNS), add it to `/var/lib/reprinter/config.toml`, without the
  port, and restart the service:

  ```toml
  allowed_hosts = ["printer.lan"]
  ```
- The page is plain HTTP, like CUPS on port 631: fine on a home LAN, not for
  the Internet. Do not forward the port on your router; use `--local` on a
  host that is reachable from outside.
- The service runs as the unprivileged `reprinter` user under systemd
  sandboxing. Its membership of the CUPS admin group (`lpadmin`, or `sys` on
  Arch) is what lets it manage printers; CUPS
  checks it over its local socket. It never runs a shell: options are
  validated (printer options against the driver's own choices) and passed to
  the CUPS tools as separate arguments.
- Requests that change anything must carry an `X-Reprinter` header, which
  other web sites cannot add, so a malicious page cannot make your browser
  print or change settings.

Forgot the password: remove it, then rerun the install, which sets and
prints a new one and restarts the service:

```sh
sudo -u reprinter /usr/local/bin/reprinter-web --config /var/lib/reprinter/config.toml --clear-password
sudo reprinter-web install --web-only
```

## Troubleshooting

| Symptom | Check |
|---------|-------|
| Page does not load | `systemctl status reprinter-web`, `journalctl -u reprinter-web`. Firewall: open the port (8631/tcp). |
| "reprinter-web does not answer to the name ..." | The page was opened by a name the server does not know as its own. Use its IP address or `<hostname>.local`, or add the name to `allowed_hosts` (see [Security](#security)). |
| Login says "too many login attempts" | Many logins are being sent at once, e.g. by something guessing the password. Try again in a minute. |
| Upload refused: "the server holds too many uploaded files" | Remove files you no longer need, or wait until unused ones expire (Settings → Web interface). |
| "cannot reach CUPS" | `systemctl status cups`. The service talks to `/run/cups/cups.sock`. |
| Changing a printer says "Forbidden" or "Unauthorized" | The `reprinter` user must be in the CUPS admin group (`lpadmin`, or `sys` on Arch): `id reprinter`. Rerun `sudo reprinter-web install --web-only`. |
| Network sharing: "CUPS on this server only lets root change these settings" | Some CUPS builds (Ubuntu 24.04) refuse configuration changes from anyone but root, whatever their groups. Run the `sudo cupsctl ...` command shown on the page. |
| A new printer profile is missing from "Add a printer" | The profiles are built into the binary: rebuild it (`make release`) and run its `install --web-only`. |
| "Driver not installed" | `sudo reprinter-web install --printer <id>` installs it. |
| Upload refused as too large | Settings → Web interface → Upload limit. |
| A job is held and nothing prints | The printer did not take it; the job's note says what the backend reported. Switch the printer off and on, then Retry. If it keeps failing, check Settings → Clean up for an unwanted printer that may have jammed it. |
| A job fails | Same as any CUPS job: the printer's page under [printers/](printers/), and `/var/log/cups/error_log`. |

## How it works

```
browser ──HTTP──> reprinter-web ──IPP over /run/cups/cups.sock──> cupsd   (printers, jobs: read)
                        └──────── lp, cancel, lpadmin, cupsenable, ──────> cupsd   (changes)
                                  cupsaccept, cupsctl, lpinfo, lpoptions
```

- `web/src/`: the server. `ipp.rs` is a minimal IPP client for reading
  printer and job state; `cups.rs` wraps the CUPS tools and validates
  everything passed to them; `profiles.rs` reads `printers/*.conf` and
  `drivers/*.sh` without executing them; `routes/` has the HTTP API.
- `web/static/`: the page, plain HTML, CSS and JavaScript with no build step,
  embedded in the binary.
- `web/build.rs` embeds `install.sh`, `lib/`, `drivers/` and `printers/`;
  `setup.rs` implements `install` and `uninstall` around them.
- Documents go to `lp` on standard input with an explicit
  `document-format`, so CUPS never has to guess the file type.

The JSON API under `/api` is what the page uses:

| Method and path | |
|-----------------|---|
| `GET /api/session` | Host name, version, whether a password is set and the caller is logged in |
| `POST /api/login`, `POST /api/logout` | `{"password": ...}`; session cookie |
| `GET /api/printers` | Queues and their state; `unwanted` says why a queue should be removed |
| `GET /api/printers/{name}/options` | The driver's options, choices and defaults |
| `POST /api/uploads` | Multipart field `file`; returns the upload's id and type |
| `GET`, `DELETE /api/uploads/{id}` | The file (for the preview); remove it |
| `POST /api/print` | `{"upload_id", "printer", "copies", "collate", "page_ranges", "page_set", "reverse", "orientation", "scaling", "number_up", "options": {...}, "title"}` |
| `GET /api/jobs?printer=&history=` | Active and finished jobs; `stuck` marks jobs CUPS holds after a failure, `message` is what the backend said |
| `DELETE /api/jobs?printer=&stuck=` | Cancel all active jobs, of one printer and/or only the stuck ones; returns the ids canceled |
| `DELETE /api/jobs/{id}` | Cancel a job |
| `POST /api/jobs/{id}/retry` | Release a held job so CUPS tries it now |
| `/api/admin/...` | Settings (need the password when one is set) |

Every request other than `GET` needs the header `X-Reprinter: 1`.

## Tests

```sh
make test       # unit tests: IPP encoding, parsers, validation, lp arguments, installer
make test-web   # end to end in Docker (Debian 12/13, Ubuntu 22.04/24.04, Arch), no printer needed
```

`make test-web` (`tests/web-test.sh`) builds the static amd64 binary and puts only that file
into each container (no checkout), runs `reprinter-web install`, adds a queue
whose backend writes jobs to files and one whose backend always fails, and
drives the API with `curl`: uploads, printing with a page setup, queue and
cancel, printer settings, unwanted printers, stuck jobs (retry and cancel),
adding a printer from a profile, CUPS sharing, the admin password and the
web interface settings.
It ends with a `--web-only` rerun and `uninstall --purge`.
