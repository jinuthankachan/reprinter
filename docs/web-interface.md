# Web interface

`reprinter-web` is a small web server (Rust) that runs on the print server
next to CUPS. From any browser on the network, including a phone, you can:

- **Print a file**: upload a PDF, PostScript, JPEG, PNG, GIF, TIFF or plain
  text file, check the preview, choose the page setup and print it. No driver
  or app is needed on the device.
- **Watch the queue**: active and recent jobs on every printer; cancel jobs.
- **Change settings**: printer descriptions, default options (paper size,
  toner density, ...), sharing, device URI; add printers from reprinter's
  profiles; CUPS network sharing; the web interface's own settings and
  admin password.

It complements CUPS rather than replacing it: devices that print over IPP
(AirPrint, Mopria, Windows) keep doing so, and everything the web interface
changes is ordinary CUPS configuration.

## Install

Run [install.sh](../README.md#quick-start) first, then, on the same host and
as your user (not with sudo; it asks for sudo where needed):

```sh
./install-web.sh
```

Then open `http://<host>:8631/`.

`install-web.sh`:

1. Builds `reprinter-web` with `cargo build --release` (or installs the
   binary given with `--bin`) to `/usr/local/bin/reprinter-web`.
2. Copies `printers/` and `drivers/` to `/usr/local/share/reprinter/` for
   "Add a printer".
3. Creates the `reprinter` system user and adds it to `lpadmin`.
4. Writes `/var/lib/reprinter/config.toml` if it does not exist.
5. Installs and starts the `reprinter-web` systemd service, and opens the
   port in `ufw` or `firewalld` if one is active.

| Option | Meaning |
|--------|---------|
| `--bin PATH` | Install this binary instead of building one |
| `--port PORT` | Listen on this port (default 8631, or the port already configured). Port 80 works too. |
| `--local` | Listen on 127.0.0.1 only |
| `--uninstall` | Stop and remove the service; the settings in `/var/lib/reprinter` are kept |

Rerunning is safe: it reinstalls the binary and profiles and restarts the
service, keeping the settings.

### Getting a binary

Building needs Rust 1.85 or newer:

- **Debian 13 / Raspberry Pi OS Trixie**: `sudo apt install cargo`.
- **Older releases** (Debian 12, Ubuntu 22.04/24.04, Raspberry Pi OS
  Bookworm): their packaged Rust is too old; install it with
  [rustup](https://rustup.rs).

Compiling on a Raspberry Pi 3B works but is slow. Alternatively build on a
PC for the Pi's architecture, for example with
[cross](https://github.com/cross-rs/cross), and install the result:

```sh
cd web
cross build --release --target aarch64-unknown-linux-gnu   # 64-bit Pi OS
cross build --release --target armv7-unknown-linux-gnueabihf  # 32-bit Pi OS
scp target/aarch64-unknown-linux-gnu/release/reprinter-web <user>@printpi.local:
# on the Pi:
./install-web.sh --bin ~/reprinter-web
```

A binary built natively on one machine needs a glibc at least as new as
that machine's on the target; building on the oldest release you deploy to
(or with `cross`) avoids surprises.

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
deleted when the service restarts.

### Settings

| Section | What it changes |
|---------|-----------------|
| Printers | Description, location, device URI ("Detect" lists devices on the same backend), shared, accepting jobs, enabled; the driver's default options for jobs from every device; make default; test page; delete |
| Add a printer | Creates a queue from a profile in `printers/`, like `install.sh` does: pick the detected device, the queue name, default paper and sharing. The driver must be installed (`./install.sh --printer <id>`). |
| Network sharing | `cupsctl --[no-]share-printers --[no-]remote-any --[no-]remote-admin`. Saving restarts CUPS. |
| Web interface | Upload limit, how long uploads are kept, listen address (needs a restart: `sudo systemctl restart reprinter-web`), admin password |

## Security

- **Without an admin password, anyone who can open the page can change the
  settings**, including deleting printers. Set one under Settings → Web
  interface. Printing, the queue and canceling jobs stay open to everyone
  on the network, as with the printer itself.
- The password is stored as an Argon2 hash in
  `/var/lib/reprinter/config.toml` (readable by the service only). Logins
  last 12 hours or until the service restarts.
- The page is plain HTTP, like CUPS on port 631: fine on a home LAN, not for
  the Internet. Do not forward the port on your router; use `--local` on a
  host that is reachable from outside.
- The service runs as the unprivileged `reprinter` user under systemd
  sandboxing. Its `lpadmin` membership is what lets it manage printers; CUPS
  checks it over its local socket. It never runs a shell: options are
  validated (printer options against the driver's own choices) and passed to
  the CUPS tools as separate arguments.
- Requests that change anything must carry an `X-Reprinter` header, which
  other web sites cannot add, so a malicious page cannot make your browser
  print or change settings.

Forgot the password:

```sh
sudo -u reprinter /usr/local/bin/reprinter-web --config /var/lib/reprinter/config.toml --clear-password
sudo systemctl restart reprinter-web
```

## Troubleshooting

| Symptom | Check |
|---------|-------|
| Page does not load | `systemctl status reprinter-web`, `journalctl -u reprinter-web`. Firewall: open the port (8631/tcp). |
| "cannot reach CUPS" | `systemctl status cups`. The service talks to `/run/cups/cups.sock`. |
| Changing a printer says "Forbidden" or "Unauthorized" | The `reprinter` user must be in `lpadmin`: `id reprinter`. Rerun `./install-web.sh`. |
| Network sharing: "CUPS on this server only lets root change these settings" | Some CUPS builds (Ubuntu 24.04) refuse configuration changes from anyone but root, whatever their groups. Run the `sudo cupsctl ...` command shown on the page. |
| "Add a printer" lists nothing | The profiles live in `/usr/local/share/reprinter/printers`; rerun `./install-web.sh` after adding one. |
| "Driver not installed" | `./install.sh --printer <id>` installs it. |
| Upload refused as too large | Settings → Web interface → Upload limit. |
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
- Documents go to `lp` on standard input with an explicit
  `document-format`, so CUPS never has to guess the file type.

The JSON API under `/api` is what the page uses:

| Method and path | |
|-----------------|---|
| `GET /api/session` | Host name, version, whether a password is set and the caller is logged in |
| `POST /api/login`, `POST /api/logout` | `{"password": ...}`; session cookie |
| `GET /api/printers` | Queues and their state |
| `GET /api/printers/{name}/options` | The driver's options, choices and defaults |
| `POST /api/uploads` | Multipart field `file`; returns the upload's id and type |
| `GET`, `DELETE /api/uploads/{id}` | The file (for the preview); remove it |
| `POST /api/print` | `{"upload_id", "printer", "copies", "collate", "page_ranges", "page_set", "reverse", "orientation", "scaling", "number_up", "options": {...}, "title"}` |
| `GET /api/jobs?printer=&history=` | Active and finished jobs |
| `DELETE /api/jobs/{id}` | Cancel a job |
| `/api/admin/...` | Settings (need the password when one is set) |

Every request other than `GET` needs the header `X-Reprinter: 1`.

## Tests

```sh
cd web && cargo test     # unit tests: IPP encoding, parsers, validation, lp arguments
tests/web-test.sh        # end to end in Docker (Debian 13, Ubuntu 24.04), no printer needed
```

`tests/web-test.sh` builds the binary, runs `install.sh` and
`install-web.sh` in a container, adds a queue whose backend writes jobs to
files, and drives the API with `curl`: uploads, printing with a page setup,
queue and cancel, printer settings, adding a printer from a profile, CUPS
sharing, the admin password and upload limits. The binary is built on the
host, so the images need a glibc at least as new as the host's.
