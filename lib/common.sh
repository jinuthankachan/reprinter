# shellcheck shell=bash disable=SC2034 # globals are read by the other modules
# Shared helpers: logging, privilege, platform detection, packages, services.
# Sourced by install.sh; expects `set -euo pipefail` and $WORKDIR from the caller.

log()  { printf '\n==> %s\n' "$*"; }
info() { printf '    %s\n' "$*"; }
warn() { printf 'WARNING: %s\n' "$*" >&2; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Run a command as root: directly when already root, otherwise through sudo.
as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  else
    sudo "$@"
  fi
}

# The human account that should administer CUPS: the user who invoked sudo,
# or the current user when not root. Empty when running as plain root.
admin_user() {
  if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
    printf '%s\n' "$SUDO_USER"
  elif [ "$(id -u)" -ne 0 ]; then
    id -un
  fi
}

# Read one field from /etc/os-release without sourcing it into our namespace.
os_field() {
  sed -n "s/^$1=//p" /etc/os-release | head -n1 | tr -d "\"'"
}

# Sets OS_ID, OS_PRETTY, ARCH. Dies on unsupported systems.
# Only apt/dpkg systems so far; another family needs pkg_install and the
# driver modules' package steps taught its package manager.
platform_detect() {
  [ -r /etc/os-release ] || die "/etc/os-release not found; cannot identify the distribution"
  OS_ID="$(os_field ID)"
  OS_PRETTY="$(os_field PRETTY_NAME)"
  local like
  like="$(os_field ID_LIKE)"

  case " $OS_ID $like " in
    *" debian "* | *" ubuntu "*) ;;
    *) die "Unsupported distribution: ${OS_PRETTY:-$OS_ID}. Only Debian-family systems (Debian, Ubuntu, Raspberry Pi OS, Mint, ...) are supported so far." ;;
  esac

  have dpkg || die "dpkg not found"
  ARCH="$(dpkg --print-architecture)"
}

APT_UPDATED=0
# Install distribution packages. Extra apt options (e.g. --no-install-recommends)
# may be passed along with package names.
pkg_install() {
  if [ "$APT_UPDATED" -eq 0 ]; then
    as_root apt-get update
    APT_UPDATED=1
  fi
  as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y "$@"
}

# Enable and (re)start a system service. Works with systemd and with SysV-style
# `service` (containers, non-systemd distros). Returns non-zero on failure.
service_restart() {
  local svc="$1"
  if [ -d /run/systemd/system ]; then
    as_root systemctl enable "$svc" >/dev/null 2>&1 || true
    as_root systemctl restart "$svc"
  elif have service && [ -e "/etc/init.d/$svc" ]; then
    as_root service "$svc" restart
  else
    return 1
  fi
}

# Is USB device vid:pid (lowercase hex, e.g. 04a9:2617) attached? Uses sysfs, so
# it works before usbutils is installed.
usb_present() {
  local want="${1,,}" d
  for d in /sys/bus/usb/devices/*; do
    [ -r "$d/idVendor" ] && [ -r "$d/idProduct" ] || continue
    [ "$(cat "$d/idVendor"):$(cat "$d/idProduct")" = "$want" ] && return 0
  done
  return 1
}

# Parallel build jobs: bounded by CPUs and by available RAM (~700 MB per C++
# compile job; a Raspberry Pi 3B has 1 GB in total).
build_jobs() {
  local cpus mem_mb by_mem
  cpus="$(nproc)"
  mem_mb="$(awk '/^MemAvailable:/ {print int($2 / 1024)}' /proc/meminfo)"
  by_mem=$((mem_mb / 700))
  [ "$by_mem" -ge 1 ] || by_mem=1
  echo $((cpus < by_mem ? cpus : by_mem))
}

# Succeeds when version $1 >= version $2.
version_ge() {
  [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" = "$2" ]
}
