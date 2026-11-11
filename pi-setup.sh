#!/usr/bin/env bash
# Turn a Raspberry Pi 3B into a Wi-Fi print server for a Canon LASER SHOT LBP-1210.
#
# Run ON THE PI, as a normal user with sudo rights, with the printer plugged in
# via USB and powered on:
#   chmod +x pi-setup.sh && ./pi-setup.sh
#
# Tested target: Raspberry Pi OS Lite (64-bit), Debian 12 "bookworm" or 13 "trixie".
# The driver is captppd (open-source Canon CAPT v1 driver, https://github.com/darkvision77/captppd).
# Canon's own Linux CAPT driver is x86-only and does not run on the Pi.

set -euo pipefail

CAPTPPD_VERSION="1.0.0"
QUEUE_NAME="${QUEUE_NAME:-LBP1210}"
PPD="LBP1210CAPTPPD.ppd"

log() { printf '\n==> %s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 0. Sanity checks
# ---------------------------------------------------------------------------
ARCH="$(dpkg --print-architecture)"
case "$ARCH" in
  arm64) DEB="captppd_${CAPTPPD_VERSION}_arm64.deb" ;;
  armhf)
    # The armhf .deb depends on "libcups2"; on Debian 13 (trixie) 32-bit that
    # package is called libcups2t64 and does NOT provide libcups2, so the
    # prebuilt package will not install there. Bookworm 32-bit is fine.
    DEB="captppd_${CAPTPPD_VERSION}_armhf.deb"
    if grep -q '^VERSION_CODENAME=trixie' /etc/os-release; then
      die "32-bit trixie detected. Reflash with Raspberry Pi OS Lite (64-bit), or build captppd from source (see README.md)."
    fi
    ;;
  *) die "Unsupported architecture: $ARCH (expected arm64 or armhf)" ;;
esac

if ! lsusb | grep -qi '04a9:2617'; then
  echo "WARNING: Canon LBP-1210 (USB ID 04a9:2617) not seen on the USB bus."
  echo "         Plug it in and power it on. Continuing with the install anyway."
fi

# ---------------------------------------------------------------------------
# 1. Packages: CUPS, filters, Ghostscript (does the rasterising), Avahi (mDNS/Bonjour)
# ---------------------------------------------------------------------------
log "Installing CUPS, cups-filters, ghostscript, avahi, libusb"
sudo apt-get update
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y \
  cups cups-filters ghostscript avahi-daemon libusb-1.0-0 curl usbutils

# ---------------------------------------------------------------------------
# 2. captppd driver (prebuilt .deb from the project's GitHub release)
# ---------------------------------------------------------------------------
log "Installing captppd ${CAPTPPD_VERSION} (${DEB})"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
curl -fsSL -o "$TMP/$DEB" \
  "https://github.com/darkvision77/captppd/releases/download/v${CAPTPPD_VERSION}/${DEB}"
sudo apt-get install -y "$TMP/$DEB"

# The backend talks to the printer through libusb and detaches the kernel
# usblp driver itself. Blacklisting usblp is NOT required, but it avoids the
# device bouncing between usblp and libusb between jobs.
log "Blacklisting the usblp kernel module (captusb uses libusb directly)"
echo 'blacklist usblp' | sudo tee /etc/modprobe.d/blacklist-usblp.conf >/dev/null
sudo modprobe -r usblp 2>/dev/null || true

# ---------------------------------------------------------------------------
# 3. Let this user administer CUPS, open CUPS to the LAN, share printers
# ---------------------------------------------------------------------------
log "Configuring CUPS for LAN access and sharing"
sudo usermod -aG lpadmin "$USER"
sudo cupsctl --remote-any --remote-admin --share-printers
sudo systemctl enable --now cups avahi-daemon
sudo systemctl restart cups
sleep 2

# ---------------------------------------------------------------------------
# 4. Create the queue on the captusb:// URI
# ---------------------------------------------------------------------------
log "Looking for the printer with lpinfo -v"
URI="$(sudo lpinfo -v | awk '/captusb:\/\//{print $2; exit}')"
if [ -z "$URI" ]; then
  echo "captusb backend did not find the printer. Output of lpinfo -v:"
  sudo lpinfo -v
  echo
  echo "Try running the backend directly for diagnostics:"
  echo "  sudo $(cups-config --serverbin 2>/dev/null || echo /usr/lib/cups)/backend/captusb"
  die "printer not detected; fix USB connection and rerun this script"
fi
echo "Found: $URI"

log "Creating queue '$QUEUE_NAME'"
if lpstat -p "$QUEUE_NAME" >/dev/null 2>&1; then
  sudo lpadmin -x "$QUEUE_NAME"
fi
sudo lpadmin -p "$QUEUE_NAME" -E -v "$URI" -m "$PPD" \
  -L "Raspberry Pi" -D "Canon LBP-1210 (captppd)" \
  -o printer-is-shared=true \
  -o media=a4 -o Resolution=600dpi
sudo lpadmin -d "$QUEUE_NAME"
sudo cupsenable "$QUEUE_NAME"
sudo cupsaccept "$QUEUE_NAME"

# ---------------------------------------------------------------------------
# 5. Report
# ---------------------------------------------------------------------------
IP="$(hostname -I | awk '{print $1}')"
log "Done"
lpstat -v "$QUEUE_NAME"
cat <<EOT

Queue:       $QUEUE_NAME
CUPS web UI: http://$IP:631/   (also http://$(hostname).local:631/)
IPP URL for clients: ipp://$IP:631/printers/$QUEUE_NAME
                     http://$IP:631/printers/$QUEUE_NAME   (Windows "shared printer by name")

Test print from the Pi:
  echo "hello from the pi" | lp -d $QUEUE_NAME
  lpstat -o            # watch the job
  tail -f /var/log/cups/error_log

Log out and back in once so your user picks up the lpadmin group.
Give the Pi a fixed IP (DHCP reservation on the router) so client URLs never change.
EOT
