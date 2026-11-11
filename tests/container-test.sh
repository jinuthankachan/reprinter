#!/usr/bin/env bash
# Smoke-test install.sh in throwaway Docker containers. No printer needed:
# CUPS and the driver are installed for real, then the queue is created on a
# fake device URI and checked.
#
#   tests/container-test.sh                         # default image set
#   tests/container-test.sh debian:13 ubuntu:24.04  # chosen images
#   FROM_SOURCE=1 tests/container-test.sh debian:13 # exercise the source build
#   PROFILE=canon-lbp1210 tests/container-test.sh   # profile under test
#
# Images must be Debian-family and match the host architecture (or have
# qemu binfmt set up for cross-architecture runs, e.g. --platform via DOCKER_OPTS).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${PROFILE:-canon-lbp1210}"
read -r -a DOCKER_OPTS <<<"${DOCKER_OPTS:-}"

images=("$@")
[ ${#images[@]} -gt 0 ] || images=(debian:12 debian:13 ubuntu:22.04 ubuntu:24.04)

# Fake device URI on the driver's own backend scheme: cupsd refuses URIs whose
# backend is not installed, so this also proves the driver installed it.
[ -f "$ROOT/printers/$PROFILE.conf" ] || { echo "no profile printers/$PROFILE.conf" >&2; exit 1; }
scheme="$(
  # shellcheck disable=SC1090 # profile and driver chosen at runtime
  . "$ROOT/printers/$PROFILE.conf" && . "$ROOT/drivers/$PRINTER_DRIVER.sh" && echo "$DRIVER_URI_SCHEME"
)"
fake_uri="$scheme://Test/CONTAINER?serial=CONTAINER"

args=(--printer "$PROFILE" --uri "$fake_uri")
[ "${FROM_SOURCE:-0}" = 1 ] && args+=(--from-source)

# Runs inside the container as root, with PROFILE and FAKE_URI in the environment.
# shellcheck disable=SC2016 # expanded inside the container
check='
set -euo pipefail
expect() { if eval "$1" >/dev/null; then echo "ok   $1"; else echo "FAIL $1"; exit 1; fi; }
. "/src/printers/$PROFILE.conf"
queue="$PRINTER_QUEUE"
/src/install.sh "$@"
echo "--- checks for queue $queue"
expect "lpstat -v $queue | grep -F \"$FAKE_URI\""
expect "lpstat -p $queue | grep enabled"
expect "lpoptions -p $queue | grep printer-is-shared=true"
expect "lpoptions -p $queue -l | grep \"^PageSize.*\*A4\""
for opt in $PRINTER_OPTIONS; do
  expect "lpoptions -p $queue -l | grep \"^${opt%%=*}/.*\*${opt#*=}\""
done
expect "lpstat -d | grep $queue"
expect "cupsctl | grep _share_printers=1"
expect "cupsctl | grep _remote_any=0"
expect "cupsctl | grep _remote_admin=0"
echo "--- rerun updates the queue in place, and removes queues for the printer on another backend"
# What the desktop makes when the printer is plugged in before its driver is
# installed: a queue on the stock usb backend with a generic driver.
lpadmin -p Desktop -E -v "usb://${FAKE_URI#*://}" -m drv:///sample.drv/laserjet.ppd
lpadmin -p Other -E -v "usb://Test/OTHER?serial=OTHER" -m drv:///sample.drv/laserjet.ppd
/src/install.sh "$@" --paper letter >/tmp/rerun.log
expect "lpstat -v $queue | grep -F \"$FAKE_URI\""
expect "lpoptions -p $queue -l | grep \"^PageSize.*\*Letter\""
expect "grep \"Removing queue .Desktop.\" /tmp/rerun.log"
expect "! lpstat -v Desktop 2>/dev/null"
expect "lpstat -v Other"
lpadmin -x Other
# cupsd writes printers.conf when it restarts at the latest, which the rerun did.
expect "sed -n \"/Printer $queue>/,/<.Printer>/p\" /etc/cups/printers.conf | grep -x \"ErrorPolicy retry-job\""
echo "--- --local makes it unshared"
/src/install.sh "$@" --local >/dev/null
expect "lpoptions -p $queue | grep printer-is-shared=false"
echo PASS
'

failed=()
for img in "${images[@]}"; do
  echo "=================== $img"
  if docker run --rm "${DOCKER_OPTS[@]}" -v "$ROOT:/src:ro" \
    -e PROFILE="$PROFILE" -e FAKE_URI="$fake_uri" "$img" \
    bash -c "$check" _ "${args[@]}"; then
    echo "=================== $img: PASS"
  else
    echo "=================== $img: FAIL"
    failed+=("$img")
  fi
done

if [ ${#failed[@]} -gt 0 ]; then
  echo "FAILED: ${failed[*]}"
  exit 1
fi
echo "All passed: ${images[*]}"
