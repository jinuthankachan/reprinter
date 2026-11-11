# shellcheck shell=bash disable=SC2034 # DRIVER_* is read by lib/cups.sh
# Driver module: captppd, open-source CUPS driver for Canon CAPT v1 printers
# (LBP800, LBP810, LBP1120, LBP1210, LBP3200). BSD-2-Clause.
# https://github.com/darkvision77/captppd  (protocol library: libcapt)
#
# Driver module interface (see docs/adding-a-printer.md):
#   DRIVER_URI_SCHEME   backend scheme the queue's device URI uses
#   driver_install      install the driver (idempotent)
#   driver_configure    host tweaks the driver needs, run after driver_install
#   driver_diagnose     print hints when the printer is not detected

CAPTPPD_VERSION="1.0.0"
CAPTPPD_COMMIT="d6df4f867d801e9dbe77f7e58eaec3a77f118416" # tag v1.0.0
CAPTPPD_REPO="https://github.com/darkvision77/captppd"

DRIVER_URI_SCHEME="captusb"

# SHA-256 of the prebuilt release packages (GitHub release asset digests).
captppd_deb_sha256() {
  case "$1" in
    amd64) echo ac3c18645a384459f571163335b64cbe6813a3bb5d1a0c592bbeea5c41dd5537 ;;
    arm64) echo 29dceddcf9954d20aceb8aae09e10df3fd635efdd37ab9a3f0dd32306a6ea5ba ;;
    armhf) echo 1b020be9b52ceab0d13598e474b9752ae9ab6f2532aaec8975ca7275aa5f0c29 ;;
    *) return 1 ;;
  esac
}

driver_install() {
  local status
  # shellcheck disable=SC2016 # dpkg-query format, not a shell expansion
  status="$(dpkg-query -W -f='${Status} ${Version}' captppd 2>/dev/null || true)"
  if [ "$FROM_SOURCE" -eq 0 ] && [ "$status" = "install ok installed $CAPTPPD_VERSION" ]; then
    log "captppd $CAPTPPD_VERSION is already installed"
    return
  fi

  if [ "$FROM_SOURCE" -eq 0 ] && captppd_install_prebuilt; then
    return
  fi
  captppd_install_from_source
}

# Returns 1 (caller falls back to a source build) when there is no prebuilt
# package for this architecture or its dependencies cannot be met here.
captppd_install_prebuilt() {
  local sha deb
  if ! sha="$(captppd_deb_sha256 "$ARCH")"; then
    info "No prebuilt captppd package for $ARCH"
    return 1
  fi
  deb="captppd_${CAPTPPD_VERSION}_${ARCH}.deb"

  log "Downloading $deb"
  curl -fsSL --retry 3 -o "$WORKDIR/$deb" \
    "$CAPTPPD_REPO/releases/download/v$CAPTPPD_VERSION/$deb" ||
    die "could not download $deb from GitHub"
  echo "$sha  $WORKDIR/$deb" | sha256sum -c --quiet - ||
    die "checksum mismatch for $deb; refusing to install it"
  chmod 644 "$WORKDIR/$deb" # readable by apt's sandbox user

  # The prebuilt packages need glibc >= 2.34 and a package named libcups2.
  # 32-bit Debian 13 / Ubuntu 24.04 renamed it libcups2t64 without an alias,
  # so check installability first instead of failing half way.
  if ! as_root apt-get install -s "$WORKDIR/$deb" >/dev/null 2>&1; then
    warn "prebuilt $deb has unmet dependencies on ${OS_PRETTY:-this system}; building from source instead"
    return 1
  fi

  log "Installing captppd $CAPTPPD_VERSION ($deb)"
  as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y "$WORKDIR/$deb"
}

# Build a .deb from the pinned upstream tag and install it, so the driver can be
# removed later with `apt remove captppd` like the prebuilt one.
captppd_install_from_source() {
  log "Building captppd $CAPTPPD_VERSION from source"
  pkg_install --no-install-recommends git ca-certificates cmake make g++ \
    python3 pkg-config libcups2-dev libusb-1.0-0-dev dpkg-dev file

  local cmake_ver gcc_ver
  cmake_ver="$(cmake --version | awk 'NR==1 {print $3}')"
  gcc_ver="$(g++ -dumpfullversion -dumpversion)"
  version_ge "$cmake_ver" 3.21 || die "captppd needs CMake >= 3.21 (found $cmake_ver). Use Debian 12+ / Ubuntu 22.04+."
  version_ge "$gcc_ver" 11 || die "captppd needs g++ >= 11 (found $gcc_ver). Use Debian 12+ / Ubuntu 22.04+."

  local src="$WORKDIR/captppd-src" build="$WORKDIR/captppd-build" jobs deb
  git -c advice.detachedHead=false clone --quiet --depth 1 \
    --branch "v$CAPTPPD_VERSION" "$CAPTPPD_REPO.git" "$src"
  [ "$(git -C "$src" rev-parse HEAD)" = "$CAPTPPD_COMMIT" ] ||
    die "upstream tag v$CAPTPPD_VERSION no longer points at $CAPTPPD_COMMIT; refusing to build"

  jobs="$(build_jobs)"
  info "Compiling with $jobs job(s); this takes a while on small boards"
  cmake -S "$src" -B "$build" -DCMAKE_BUILD_TYPE=Release
  cmake --build "$build" --parallel "$jobs"
  (cd "$build" && cpack -G DEB)

  deb="$(find "$build" -name "captppd_${CAPTPPD_VERSION}_*.deb" -print -quit)"
  [ -n "$deb" ] || die "cpack did not produce a captppd .deb (see output above)"
  chmod 644 "$deb"
  chmod 755 "$build" "$(dirname "$deb")"

  log "Installing $(basename "$deb")"
  as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y --reinstall "$deb"
}

driver_configure() {
  # captusb drives the printer through libusb and detaches the usblp kernel
  # driver itself. Blacklisting usblp is not required, but it stops the device
  # bouncing between usblp and libusb between jobs. The stock CUPS usb backend
  # also uses libusb, so other USB printers keep working; only software that
  # writes straight to /dev/usb/lp* would be affected.
  local conf=/etc/modprobe.d/blacklist-usblp.conf
  if [ ! -f "$conf" ]; then
    log "Blacklisting the usblp kernel module"
    as_root mkdir -p /etc/modprobe.d
    echo 'blacklist usblp' | as_root tee "$conf" >/dev/null
  fi
  if have modprobe; then
    as_root modprobe -r usblp 2>/dev/null || true
  fi
}

driver_diagnose() {
  local serverbin
  serverbin="$(cups-config --serverbin 2>/dev/null || echo /usr/lib/cups)"
  echo "Run the backend by hand for diagnostics:  sudo $serverbin/backend/captusb"
  echo "Driver issues: $CAPTPPD_REPO/issues"
}
