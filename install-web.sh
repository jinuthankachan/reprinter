#!/usr/bin/env bash
# reprinter web interface: install reprinter-web, a browser front end for the
# print server (upload and print files, manage printers and settings), as a
# systemd service.
#
# Run after ./install.sh, on the same host, as your user (it uses sudo where
# needed; cargo builds as you):
#   ./install-web.sh                 build with cargo, install and start
#   ./install-web.sh --bin PATH      install a reprinter-web binary built elsewhere
#   ./install-web.sh --port 80       listen on another port (default 8631)
#   ./install-web.sh --uninstall
#
# See docs/web-interface.md.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$ROOT/lib/common.sh"

BIN_DIR=/usr/local/bin
SHARE_DIR=/usr/local/share/reprinter
STATE_DIR=/var/lib/reprinter
CONFIG="$STATE_DIR/config.toml"
UNIT=/etc/systemd/system/reprinter-web.service
SERVICE_USER=reprinter
DEFAULT_PORT=8631

usage() {
  cat <<EOF
Usage: $0 [options]

      --bin PATH      install this reprinter-web binary instead of building one
      --port PORT     port to listen on (default: $DEFAULT_PORT, or the one already configured)
      --local         listen on 127.0.0.1 only (no access from the network)
      --uninstall     stop and remove the service (settings in $STATE_DIR are kept)
  -h, --help          show this help
EOF
}

BIN=""
PORT=""
LOCAL=0
UNINSTALL=0

need_value() { [ "$2" -ge 2 ] || die "$1 needs a value (see --help)"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --bin) need_value "$1" $#; BIN="$2"; shift 2 ;;
    --port) need_value "$1" $#; PORT="$2"; shift 2 ;;
    --local) LOCAL=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown option: $1 (see --help)" ;;
  esac
done

if [ -n "$PORT" ]; then
  [[ "$PORT" =~ ^[0-9]+$ ]] && [ "$PORT" -ge 1 ] && [ "$PORT" -le 65535 ] || die "invalid port: $PORT"
fi

have_systemd() { [ -d /run/systemd/system ]; }

# ---------------------------------------------------------------------------
# Uninstall
# ---------------------------------------------------------------------------
if [ "$UNINSTALL" -eq 1 ]; then
  log "Removing reprinter-web"
  if have_systemd; then
    as_root systemctl disable --now reprinter-web 2>/dev/null || true
  fi
  as_root rm -f "$UNIT" "$BIN_DIR/reprinter-web"
  if have_systemd; then
    as_root systemctl daemon-reload
  fi
  as_root rm -rf "$SHARE_DIR"
  info "Kept the settings in $STATE_DIR and the $SERVICE_USER user. To remove them too:"
  info "  sudo rm -rf $STATE_DIR && sudo userdel $SERVICE_USER"
  exit 0
fi

# ---------------------------------------------------------------------------
# 1. Binary
# ---------------------------------------------------------------------------
platform_detect
have lpstat || die "CUPS is not installed. Run ./install.sh first."

if [ -z "$BIN" ]; then
  have cargo || die "cargo not found. Install Rust 1.85+ (Debian 13: sudo apt install cargo; anywhere: https://rustup.rs) and run this as your user, not with sudo. Or build reprinter-web on another machine and pass --bin (see docs/web-interface.md)."
  rust_ver="$(rustc --version | awk '{print $2}')"
  version_ge "$rust_ver" 1.85 ||
    die "reprinter-web needs Rust 1.85 or newer (found $rust_ver). Install a current toolchain with https://rustup.rs, or pass --bin."
  log "Building reprinter-web (cargo build --release)"
  info "A few minutes on a PC; much longer on a small board such as a Raspberry Pi 3B."
  (cd "$ROOT/web" && CARGO_BUILD_JOBS="$(build_jobs)" cargo build --release --locked)
  BIN="$ROOT/web/target/release/reprinter-web"
fi
[ -f "$BIN" ] || die "$BIN not found"
version="$("$BIN" --version 2>&1)" ||
  die "$BIN does not run on this host ($ARCH): $version. Build it for this architecture (see docs/web-interface.md)."

log "Installing $version"
as_root install -m 0755 "$BIN" "$BIN_DIR/reprinter-web"

# Printer profiles and driver modules, for "Add a printer" in the settings.
as_root rm -rf "$SHARE_DIR"
as_root mkdir -p "$SHARE_DIR"
as_root cp -r "$ROOT/printers" "$ROOT/drivers" "$SHARE_DIR/"
as_root chmod -R a+rX "$SHARE_DIR"

# ---------------------------------------------------------------------------
# 2. Service user and settings
# ---------------------------------------------------------------------------
if ! id "$SERVICE_USER" >/dev/null 2>&1; then
  log "Creating the $SERVICE_USER system user"
  as_root useradd --system --user-group --home-dir "$STATE_DIR" --no-create-home \
    --shell /usr/sbin/nologin "$SERVICE_USER"
fi
# lpadmin membership is what CUPS checks before letting it change printers.
as_root usermod -aG lpadmin "$SERVICE_USER"
as_root install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$STATE_DIR"

if [ -n "$PORT" ] || [ "$LOCAL" -eq 1 ] || ! as_root test -f "$CONFIG"; then
  current="$(as_root sed -n 's/^listen = "\(.*\)"$/\1/p' "$CONFIG" 2>/dev/null || true)"
  port="${PORT:-${current##*:}}"
  port="${port:-$DEFAULT_PORT}"
  listen="$([ "$LOCAL" -eq 1 ] && echo 127.0.0.1 || echo 0.0.0.0):$port"
  if as_root grep -q '^listen = ' "$CONFIG" 2>/dev/null; then
    as_root sed -i "s|^listen = .*|listen = \"$listen\"|" "$CONFIG"
  elif as_root test -f "$CONFIG"; then
    echo "listen = \"$listen\"" | as_root tee -a "$CONFIG" >/dev/null
  else
    printf '# reprinter-web settings. The Settings page rewrites this file.\n\nlisten = "%s"\n' "$listen" |
      as_root tee "$CONFIG" >/dev/null
  fi
fi
as_root chown "$SERVICE_USER:$SERVICE_USER" "$CONFIG"
as_root chmod 0600 "$CONFIG"
listen="$(as_root sed -n 's/^listen = "\(.*\)"$/\1/p' "$CONFIG")"
listen="${listen:-0.0.0.0:$DEFAULT_PORT}"
port="${listen##*:}"

# ---------------------------------------------------------------------------
# 3. Service and firewall
# ---------------------------------------------------------------------------
as_root install -D -m 0644 "$ROOT/web/reprinter-web.service" "$UNIT"
started=0
if have_systemd; then
  log "Starting the reprinter-web service"
  as_root systemctl daemon-reload
  as_root systemctl enable reprinter-web >/dev/null 2>&1
  as_root systemctl restart reprinter-web
  for _ in $(seq 1 20); do
    if systemctl is-active --quiet reprinter-web && curl -fsS -o /dev/null "http://127.0.0.1:$port/api/session" 2>/dev/null; then
      started=1
      break
    fi
    sleep 0.5
  done
  [ "$started" -eq 1 ] || warn "reprinter-web did not come up; check: journalctl -u reprinter-web"
else
  warn "no systemd here: start reprinter-web by hand:"
  warn "  sudo runuser -u $SERVICE_USER -- $BIN_DIR/reprinter-web --config $CONFIG --share-dir $SHARE_DIR"
fi

case "$listen" in
  127.* | localhost:* | "[::1]":*) ;;
  *)
    if have ufw && as_root ufw status 2>/dev/null | grep -q '^Status: active'; then
      log "ufw is active: allowing $port/tcp (reprinter web)"
      as_root ufw allow "$port/tcp" comment 'reprinter web' >/dev/null
    elif have firewall-cmd && as_root firewall-cmd --state >/dev/null 2>&1; then
      log "firewalld is running: allowing $port/tcp (reprinter web)"
      as_root firewall-cmd --permanent --add-port="$port/tcp" >/dev/null
      as_root firewall-cmd --reload >/dev/null
    fi
    ;;
esac

# ---------------------------------------------------------------------------
# 4. Report
# ---------------------------------------------------------------------------
ip="$(hostname -I 2>/dev/null | awk '{print $1}')"
log "Done"
case "$listen" in
  127.* | localhost:* | "[::1]":*) echo "Web interface: http://127.0.0.1:$port/  (this host only)" ;;
  *) echo "Web interface: http://${ip:-<this-host-ip>}:$port/   (also http://$(hostname).local:$port/)" ;;
esac
cat <<EOT

Anyone who can open the page can print and, until you set an admin password
(Settings -> Web interface), change the settings.

Logs:      journalctl -u reprinter-web
Settings:  $CONFIG
Forgot the admin password?
  sudo -u $SERVICE_USER $BIN_DIR/reprinter-web --config $CONFIG --clear-password
  sudo systemctl restart reprinter-web
EOT
