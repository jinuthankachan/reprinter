#!/usr/bin/env bash
# reprinter: give an old printer a second life as a network printer.
#
# Turns a Linux host (Raspberry Pi, x86 PC, mini PC, ...) into a CUPS print
# server for a printer whose driver only runs there. Clients print over IPP /
# AirPrint / IPP Everywhere and need no driver.
#
# Run ON THE HOST the printer is plugged into, as root or a sudo-capable user:
#   ./install.sh                       # auto-detect a supported USB printer
#   ./install.sh --printer canon-lbp1210
#   ./install.sh --list                # supported printers
#
# Supported hosts: Debian 12+, Ubuntu 22.04+, Raspberry Pi OS (bookworm/trixie)
# and derivatives, on amd64, arm64 and armhf (other architectures build the
# driver from source).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$ROOT/lib/common.sh"
# shellcheck source=lib/cups.sh
. "$ROOT/lib/cups.sh"

usage() {
  cat <<EOF
Usage: $0 [options]

  -p, --printer ID    printer profile from printers/ (default: auto-detect on USB)
  -q, --queue NAME    CUPS queue name (default: the profile's, e.g. LBP1210)
      --paper SIZE    default paper size, CUPS media name (default: a4; e.g. letter)
      --uri URI       use this device URI instead of searching lpinfo -v
      --local         do not share on the network (local printing only)
      --from-source   build the driver from source instead of using a prebuilt package
  -l, --list          list supported printers and exit
  -h, --help          show this help

Environment: QUEUE_NAME and PAPER work as defaults for --queue and --paper.
EOF
}

# ---------------------------------------------------------------------------
# Printer profiles (printers/<id>.conf)
# ---------------------------------------------------------------------------
PROFILE_FIELDS=(PRINTER_MODEL PRINTER_USB_IDS PRINTER_DRIVER PRINTER_PPD
  PRINTER_URI_REGEX PRINTER_QUEUE PRINTER_OPTIONS)
PROFILE_REQUIRED=(PRINTER_MODEL PRINTER_DRIVER PRINTER_PPD PRINTER_URI_REGEX PRINTER_QUEUE)

load_profile() {
  local id="$1" file="$ROOT/printers/$1.conf" v
  [ -f "$file" ] || die "unknown printer '$id'. See: $0 --list"
  for v in "${PROFILE_FIELDS[@]}"; do
    printf -v "$v" ''
  done
  # shellcheck source=printers/canon-lbp1210.conf
  . "$file"
  for v in "${PROFILE_REQUIRED[@]}"; do
    [ -n "${!v}" ] || die "$file: $v is not set"
  done
  [ -f "$ROOT/drivers/$PRINTER_DRIVER.sh" ] ||
    die "$file: driver '$PRINTER_DRIVER' has no module drivers/$PRINTER_DRIVER.sh"
}

profile_ids() {
  local f
  for f in "$ROOT"/printers/*.conf; do
    [ -e "$f" ] || continue
    f="${f##*/}"
    echo "${f%.conf}"
  done
}

# Is any of the loaded profile's USB IDs attached?
profile_connected() {
  local id
  for id in $PRINTER_USB_IDS; do
    usb_present "$id" && return 0
  done
  return 1
}

list_printers() {
  local id connected
  printf '%-16s %-28s %-9s %-11s %s\n' ID MODEL DRIVER USB CONNECTED
  for id in $(profile_ids); do
    (
      load_profile "$id"
      connected=no
      profile_connected && connected=yes
      printf '%-16s %-28s %-9s %-11s %s\n' "$id" "$PRINTER_MODEL" \
        "$PRINTER_DRIVER" "${PRINTER_USB_IDS:--}" "$connected"
    )
  done
}

detect_printer() {
  local id
  for id in $(profile_ids); do
    (load_profile "$id" && profile_connected) && echo "$id"
  done
  return 0
}

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------
PRINTER_ID=""
QUEUE="${QUEUE_NAME:-}"
PAPER="${PAPER:-a4}"
URI=""
SHARE=1
FROM_SOURCE=0

need_value() { [ "$2" -ge 2 ] || die "$1 needs a value (see --help)"; }

while [ $# -gt 0 ]; do
  case "$1" in
    -p | --printer) need_value "$1" $#; PRINTER_ID="$2"; shift 2 ;;
    -q | --queue) need_value "$1" $#; QUEUE="$2"; shift 2 ;;
    --paper) need_value "$1" $#; PAPER="$2"; shift 2 ;;
    --uri) need_value "$1" $#; URI="$2"; shift 2 ;;
    --local) SHARE=0; shift ;;
    --from-source) FROM_SOURCE=1; shift ;;
    -l | --list) list_printers; exit 0 ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown option: $1 (see --help)" ;;
  esac
done

# ---------------------------------------------------------------------------
# 0. Host, printer and driver
# ---------------------------------------------------------------------------
platform_detect

if [ -z "$PRINTER_ID" ]; then
  mapfile -t found < <(detect_printer)
  case "${#found[@]}" in
    1) PRINTER_ID="${found[0]}" ;;
    0) die "no supported printer found on USB. Plug it in and power it on, or pick one with --printer (see --list)." ;;
    *) die "several supported printers are connected (${found[*]}); pick one with --printer" ;;
  esac
fi

load_profile "$PRINTER_ID"
# shellcheck source=drivers/captppd.sh
. "$ROOT/drivers/$PRINTER_DRIVER.sh"

QUEUE="${QUEUE:-$PRINTER_QUEUE}"
[[ "$QUEUE" =~ ^[A-Za-z0-9_.-]+$ ]] ||
  die "queue name '$QUEUE' may only contain letters, digits, '_', '.' and '-'"

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT
chmod 755 "$WORKDIR" # apt reads local .deb files as its unprivileged _apt user

log "reprinter"
info "Host:    ${OS_PRETTY:-$OS_ID} ($ARCH)"
info "Printer: $PRINTER_MODEL  [profile $PRINTER_ID]"
info "Driver:  $PRINTER_DRIVER"
info "Queue:   $QUEUE  (paper $PAPER, $([ "$SHARE" -eq 1 ] && echo "shared on the LAN" || echo "local only"))"

if [ -z "$URI" ] && [ -n "$PRINTER_USB_IDS" ] && ! profile_connected; then
  warn "$PRINTER_MODEL (USB ID $PRINTER_USB_IDS) is not on the USB bus."
  warn "Plug it in and power it on. Continuing with the install anyway."
fi

# ---------------------------------------------------------------------------
# 1. CUPS stack   2. Driver   3. CUPS sharing   4. Queue
# ---------------------------------------------------------------------------
cups_install_stack
driver_install
driver_configure
cups_configure

if [ -n "$URI" ]; then
  info "Using device URI from --uri: $URI"
else
  cups_find_uri
fi

cups_create_queue
cups_remove_conflicting_queues
cups_report
