# shellcheck shell=bash
# CUPS server side: packages, LAN sharing, firewall, device discovery, queues.
# Uses the PRINTER_* profile fields and QUEUE/URI/PAPER/SHARE from install.sh.

NEED_RELOGIN=0

cups_install_stack() {
  log "Installing CUPS, cups-filters, Ghostscript, Avahi, libusb"
  # Ghostscript does the rasterising; Avahi advertises queues (mDNS/Bonjour,
  # IPP Everywhere, AirPrint) so clients find them without configuration.
  pkg_install cups cups-filters ghostscript avahi-daemon libusb-1.0-0 \
    usbutils curl ca-certificates
}

cups_wait() {
  local _
  for _ in $(seq 1 30); do
    lpstat -r >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  die "CUPS scheduler did not come up; check: journalctl -u cups / /var/log/cups/error_log"
}

cups_restart() {
  service_restart cups || die "could not (re)start CUPS (no systemd and no /etc/init.d/cups)"
  cups_wait
}

# Open IPP and mDNS when a host firewall is active. No-op otherwise.
firewall_open() {
  if have ufw && as_root ufw status 2>/dev/null | grep -q '^Status: active'; then
    log "ufw is active: allowing IPP (631/tcp) and mDNS (5353/udp)"
    as_root ufw allow 631/tcp comment 'CUPS IPP' >/dev/null
    as_root ufw allow 5353/udp comment 'mDNS' >/dev/null
  elif have firewall-cmd && as_root firewall-cmd --state >/dev/null 2>&1; then
    log "firewalld is running: allowing the ipp and mdns services"
    as_root firewall-cmd --permanent --add-service=ipp --add-service=mdns >/dev/null
    as_root firewall-cmd --reload >/dev/null
  fi
}

cups_configure() {
  local user
  user="$(admin_user)"
  if [ -n "$user" ] && ! id -nG "$user" | tr ' ' '\n' | grep -qx lpadmin; then
    log "Adding $user to the lpadmin group (CUPS administration)"
    as_root usermod -aG lpadmin "$user"
    NEED_RELOGIN=1
  fi

  cups_restart

  if [ "$SHARE" -eq 1 ]; then
    log "Opening CUPS to the LAN and sharing printers"
    as_root cupsctl --remote-any --remote-admin --share-printers
    firewall_open
    service_restart avahi-daemon ||
      warn "could not start avahi-daemon; clients will not auto-discover the printer (add it by IPP URL instead)"
    cups_restart
  fi
}

# All device URIs the driver's backend currently reports, one per line.
cups_list_uris() {
  as_root lpinfo --include-schemes "$DRIVER_URI_SCHEME" -v 2>/dev/null | awk '{print $2}'
}

# Sets URI from `lpinfo -v` using the profile's PRINTER_URI_REGEX. Dies with
# diagnostics when the printer is not found.
cups_find_uri() {
  log "Looking for the printer (lpinfo -v, scheme $DRIVER_URI_SCHEME://)"
  local uris
  uris="$(cups_list_uris)"
  URI="$(printf '%s\n' "$uris" | grep -E -m1 "$PRINTER_URI_REGEX" || true)"
  [ -n "$URI" ] && { info "Found: $URI"; return; }

  if [ -n "$uris" ]; then
    echo "The $DRIVER_URI_SCHEME backend sees other devices, but none matching $PRINTER_MODEL:"
    printf '%s\n' "$uris" | sed 's/^/  /'
    echo "If one of them is this printer, rerun with --uri '<uri>' and report the URI so the profile can be fixed."
  else
    echo "The $DRIVER_URI_SCHEME backend did not report any printer."
    echo "Check that it is plugged in and powered on: lsusb should list USB ID ${PRINTER_USB_IDS:-?}."
  fi
  driver_diagnose
  die "printer not detected. The driver is installed; fix the USB connection and rerun this script."
}

cups_create_queue() {
  local verb=Creating shared=true opt
  lpstat -p "$QUEUE" >/dev/null 2>&1 && verb=Updating
  [ "$SHARE" -eq 1 ] || shared=false

  local -a extra opts=(-o "printer-is-shared=$shared" -o "media=$PAPER")
  read -r -a extra <<<"$PRINTER_OPTIONS"
  for opt in "${extra[@]}"; do
    opts+=(-o "$opt")
  done

  log "$verb queue '$QUEUE'"
  # Modifies an existing queue in place (keeps pending jobs) or creates it.
  as_root lpadmin -p "$QUEUE" -E -v "$URI" -m "$PRINTER_PPD" \
    -L "$(hostname)" -D "$PRINTER_MODEL ($PRINTER_DRIVER)" "${opts[@]}"
  as_root cupsenable "$QUEUE"
  as_root cupsaccept "$QUEUE"

  # Become the default destination only if the host has none yet. CUPS clients
  # take LPDEST / PRINTER from the environment as the default; ignore those.
  if ! env -u LPDEST -u PRINTER lpstat -d 2>/dev/null | grep -q 'system default destination:'; then
    as_root lpadmin -d "$QUEUE"
  fi
}

cups_report() {
  local ip host
  host="$(hostname)"
  ip="$(hostname -I 2>/dev/null | awk '{print $1}')"
  [ -n "$ip" ] || ip="<this-host-ip>"

  log "Done"
  lpstat -v "$QUEUE"
  echo
  echo "Queue:  $QUEUE  ($PRINTER_MODEL, driver $PRINTER_DRIVER)"
  if [ "$SHARE" -eq 1 ]; then
    cat <<EOT
CUPS web UI:         http://$ip:631/   (also http://$host.local:631/)
IPP URL for clients: ipp://$ip:631/printers/$QUEUE
Windows by name:     http://$ip:631/printers/$QUEUE

Give this host a fixed IP (DHCP reservation on the router) so client URLs never change.
EOT
  else
    echo "Local only (--local): not shared on the network."
  fi
  cat <<EOT

Test print from this host:
  echo "hello from $host" | lp -d $QUEUE
  lpstat -o                       # watch the job
  sudo tail -f /var/log/cups/error_log
EOT
  if [ "$NEED_RELOGIN" -eq 1 ]; then
    echo
    echo "Log out and back in once so your user picks up the lpadmin group."
  fi
}
