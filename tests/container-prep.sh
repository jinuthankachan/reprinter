#!/usr/bin/env bash
# Runs first in every test container, as root: gives a bare image what a real
# host already has. Debian and Ubuntu images need nothing.
#
# Arch images ship without package lists and without an init system. Sync
# and upgrade once (a real Arch host is kept up to date), and give install.sh
# a SysV-style `service` for CUPS, which is what it falls back to without
# systemd.

set -euo pipefail

if command -v pacman >/dev/null; then
  pacman -Syu --noconfirm --needed procps-ng >/dev/null
  mkdir -p /etc/init.d
  cat >/usr/local/bin/service <<'EOF'
#!/bin/sh
exec "/etc/init.d/$1" "$2"
EOF
  cat >/etc/init.d/cups <<'EOF'
#!/bin/sh
if pkill -x cupsd; then
  while pgrep -x cupsd >/dev/null; do sleep 0.1; done
fi
exec cupsd
EOF
  chmod 755 /usr/local/bin/service /etc/init.d/cups
fi
