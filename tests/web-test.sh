#!/usr/bin/env bash
# End-to-end test of reprinter-web in throwaway Docker containers. No printer
# needed. Only the binary goes into the container (no repository checkout),
# so this also proves it is self-contained: `reprinter-web install` sets up
# CUPS, the driver and the LBP-1210 queue on a fake device URI, and itself as
# the service. A second queue prints into files through a capture backend,
# and the API is exercised with curl while reprinter-web runs as its service
# user. Ends with `reprinter-web uninstall --purge`.
#
#   tests/web-test.sh                      # Debian 12/13, Ubuntu 22.04/24.04
#   tests/web-test.sh debian:13            # chosen images
#   BIN=path/to/reprinter-web tests/web-test.sh
#
# Unless BIN is given, the static amd64 binary is built here (musl, so it
# runs on every image regardless of its glibc).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
read -r -a DOCKER_OPTS <<<"${DOCKER_OPTS:-}"

images=("$@")
[ ${#images[@]} -gt 0 ] || images=(debian:12 debian:13 ubuntu:22.04 ubuntu:24.04)

if [ -z "${BIN:-}" ]; then
  "$ROOT/web/build-release.sh" amd64 >/dev/null
  BIN="$ROOT/web/dist/reprinter-web-amd64"
fi
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

# Runs inside the container as root.
# shellcheck disable=SC2016 # expanded inside the container
check='
set -euo pipefail
expect() { if eval "$1" >/dev/null 2>&1; then echo "ok   $1"; else echo "FAIL $1"; [ -f /tmp/web.log ] && tail -20 /tmp/web.log; exit 1; fi; }

echo "--- reprinter-web install: CUPS, driver, LBP1210 queue (fake URI) and the web interface in one step"
expect "/opt/reprinter-web install --list | grep -q canon-lbp1210"
/opt/reprinter-web install --printer canon-lbp1210 --uri "captusb://Test/CONTAINER?serial=CONTAINER" >/tmp/install.log 2>&1 ||
  { tail -40 /tmp/install.log; exit 1; }
expect "lpstat -v LBP1210 | grep -qF captusb://Test/CONTAINER"
expect "lpoptions -p LBP1210 | grep -q printer-is-shared=true"
expect "id -nG reprinter | grep -qw lpadmin"
expect "cmp -s /opt/reprinter-web /usr/local/bin/reprinter-web"
expect "grep -q \"^ExecStart=/usr/local/bin/reprinter-web --config /var/lib/reprinter/config.toml\$\" /etc/systemd/system/reprinter-web.service"
expect "grep -qF \"listen = \\\"0.0.0.0:8631\\\"\" /var/lib/reprinter/config.toml"
expect "test \"\$(stat -c %U:%a /var/lib/reprinter/config.toml)\" = reprinter:600"
expect "! ls -d /tmp/reprinter-install-*"
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq jq >/dev/null

# A queue whose backend writes each job to /tmp/capture, with a driver that
# has a Duplex option (the LBP-1210 has none).
cat >/usr/lib/cups/backend/capture <<"EOF"
#!/bin/sh
if [ $# -eq 0 ]; then echo "direct capture:/out \"Capture\" \"Capture test device\""; exit 0; fi
mkdir -p /tmp/capture
if [ -n "${6:-}" ]; then cat "$6"; else cat; fi >/tmp/capture/job-$1.out
EOF
chmod 755 /usr/lib/cups/backend/capture
lpadmin -p Capture -E -v capture:/out -m drv:///sample.drv/laserjet.ppd 2>/dev/null

# No systemd in the container: start it the way the unit does.
runuser -u reprinter -- /usr/local/bin/reprinter-web --config /var/lib/reprinter/config.toml >/tmp/web.log 2>&1 &
for _ in $(seq 1 50); do curl -fs -o /dev/null http://127.0.0.1:8631/api/session && break; sleep 0.2; done

B=http://127.0.0.1:8631/api
J=/tmp/jar
get()  { curl -fsS -b $J "$B$1"; }
send() {
  local status
  status=$(curl -sS -b $J -c $J -o /tmp/resp -w "%{http_code}" -X "$1" -H "X-Reprinter: 1" \
    -H "Content-Type: application/json" -d "${3:-{\}}" "$B$2") || return 1
  case "$status" in 2*) cat /tmp/resp ;; *) echo "$1 $2 -> HTTP $status: $(cat /tmp/resp)" >&2; return 1 ;; esac
}
code() { curl -s -o /dev/null -w "%{http_code}" -b $J "$@"; }
upload() { curl -fsS -H "X-Reprinter: 1" -F "file=@$1" "$B/uploads" | jq -r .id; }
wait_done() { for _ in $(seq 1 120); do lpstat -W completed -o "$1" | grep -q "^$2 " && return 0; sleep 0.5; done; return 1; }

echo "--- page and printers"
expect "curl -fs http://127.0.0.1:8631/ | grep -q \"<title>reprinter\""
expect "curl -fs http://127.0.0.1:8631/app.js | grep -q submitPrint"
expect "get /printers | jq -e '\''map(.name) == [\"Capture\", \"LBP1210\"]'\''"
expect "get /printers | jq -e '\''.[] | select(.name==\"LBP1210\") | .is_default and .shared and .accepting'\''"
expect "get /printers/LBP1210/options | jq -e '\''.[] | select(.key==\"Resolution\") | .default == \"600dpi\"'\''"
expect "get /printers/Capture/options | jq -e '\''any(.key==\"Duplex\")'\''"
expect "test \"\$(code $B/printers/Nope/options)\" = 404"

echo "--- upload and print a PDF with a page setup"
gs -q -sDEVICE=pdfwrite -o /tmp/doc.pdf -c "/Helvetica findfont 40 scalefont setfont 1 1 3 { 100 400 moveto (Page ) show 3 string cvs show showpage } for"
pdf=$(upload /tmp/doc.pdf)
expect "get /uploads/$pdf | cmp -s - /tmp/doc.pdf"
req="{\"upload_id\":\"$pdf\",\"printer\":\"Capture\",\"copies\":2,\"collate\":true,\"page_ranges\":\"1-2\",\"orientation\":\"landscape\",\"scaling\":\"fit\",\"number_up\":2,\"options\":{\"PageSize\":\"A4\",\"Duplex\":\"DuplexNoTumble\"}}"
job=$(send POST /print "$req" | jq -r .job_id)
expect "echo $job | grep -qE \"^Capture-[0-9]+\$\""
expect "wait_done Capture $job"
expect "test -s /tmp/capture/job-${job#Capture-}.out"
expect "get /jobs | jq -e --argjson id ${job#Capture-} '\''.finished[] | select(.id==\$id) | .name == \"doc.pdf\" and .state == \"completed\" and .user == \"reprinter\"'\''"

echo "--- plain text, with a title"
printf "Shopping list\n- paper\n- toner\n" >/tmp/notes.txt
txt=$(upload /tmp/notes.txt)
job=$(send POST /print "{\"upload_id\":\"$txt\",\"printer\":\"Capture\",\"title\":\"Notes\"}" | jq -r .job_id)
expect "wait_done Capture $job"
expect "get /jobs | jq -e --argjson id ${job#Capture-} '\''.finished[] | select(.id==\$id) | .name == \"Notes\" and .state == \"completed\"'\''"

echo "--- rejected requests"
printf "PK\003\004 not a document" >/tmp/x.docx
expect "test \"\$(code -H \"X-Reprinter: 1\" -F file=@/tmp/x.docx $B/uploads)\" = 415"
expect "test \"\$(code -X POST -H Content-Type:application/json -d {} $B/print)\" = 403"
expect "test \"\$(code -X POST -H X-Reprinter:1 -H Content-Type:application/json -d \"{\\\"upload_id\\\":\\\"$pdf\\\",\\\"printer\\\":\\\"Capture\\\",\\\"options\\\":{\\\"PageSize\\\":\\\"Bogus\\\"}}\" $B/print)\" = 400"
expect "test \"\$(code -X POST -H X-Reprinter:1 -H Content-Type:application/json -d \"{\\\"upload_id\\\":\\\"$pdf\\\",\\\"printer\\\":\\\"Capture\\\",\\\"page_ranges\\\":\\\"3-1\\\"}\" $B/print)\" = 400"

echo "--- queue: disable, queue a job, cancel it"
send PUT /admin/printers/LBP1210 "{\"enabled\":false}" >/dev/null
expect "lpstat -p LBP1210 | grep -q disabled"
job=$(send POST /print "{\"upload_id\":\"$txt\",\"printer\":\"LBP1210\"}" | jq -r .job_id)
id=${job#LBP1210-}
expect "get \"/jobs?printer=LBP1210&history=false\" | jq -e --argjson id $id '\''.active | map(.id) == [\$id] and .[0].state == \"pending\"'\''"
expect "test \"\$(code -X DELETE -H X-Reprinter:1 $B/jobs/$id)\" = 204"
expect "get /jobs | jq -e --argjson id $id '\''(any(.active[]; .id==\$id) | not) and any(.finished[]; .id==\$id and .state==\"canceled\")'\''"
send PUT /admin/printers/LBP1210 "{\"enabled\":true}" >/dev/null
expect "lpstat -p LBP1210 | grep -q enabled"

echo "--- printer settings"
send PUT /admin/printers/Capture "{\"info\":\"Test capture\",\"location\":\"Lab\",\"shared\":false,\"defaults\":{\"PageSize\":\"A4\",\"Resolution\":\"600dpi\"}}" >/dev/null
expect "lpoptions -p Capture -l | grep -q \"^PageSize.*\\*A4\""
expect "lpoptions -p Capture -l | grep -q \"^Resolution.*\\*600dpi\""
expect "lpoptions -p Capture | grep -q \"printer-info=.Test capture\""
expect "lpoptions -p Capture | grep -q printer-location=Lab"
expect "lpoptions -p Capture | grep -q printer-is-shared=false"
send PUT /admin/printers/Capture "{\"accepting\":false}" >/dev/null
expect "get /printers | jq -e '\''.[] | select(.name==\"Capture\") | .accepting == false'\''"
expect "test \"\$(code -X POST -H X-Reprinter:1 -H Content-Type:application/json -d \"{\\\"upload_id\\\":\\\"$pdf\\\",\\\"printer\\\":\\\"Capture\\\"}\" $B/print)\" = 400"
send PUT /admin/printers/Capture "{\"accepting\":true,\"make_default\":true}" >/dev/null
expect "lpstat -d | grep -q Capture"
job=$(send POST /admin/printers/Capture/test-page | jq -r .job_id)
expect "wait_done Capture $job"
expect "get /admin/printers/Capture/devices | jq -e '\''.[0].uri == \"capture:/out\" and .[0].matches'\''"

echo "--- add a printer from a profile"
expect "get /admin/profiles | jq -e '\''.profiles[] | select(.id==\"canon-lbp1210\") | .driver_installed and .queue_exists and .uri_scheme == \"captusb\"'\''"
expect "get /admin/profiles/canon-lbp1210/devices | jq -e '\''type == \"array\"'\''"
send POST /admin/printers "{\"profile\":\"canon-lbp1210\",\"name\":\"Second\",\"device_uri\":\"captusb://Test/SECOND?serial=2\",\"paper\":\"letter\",\"shared\":true}" >/dev/null
expect "lpstat -v Second | grep -qF captusb://Test/SECOND"
expect "lpoptions -p Second -l | grep -q \"^PageSize.*\\*Letter\""
expect "lpoptions -p Second -l | grep -q \"^Resolution.*\\*600dpi\""
expect "lpoptions -p Second | grep -q printer-is-shared=true"
send DELETE /admin/printers/Second >/dev/null
expect "! lpstat -v Second"

echo "--- CUPS network sharing"
expect "get /admin/settings | jq -e '\''.cups.share_printers and .cups.remote_any'\''"
if out=$(send PUT /admin/cups "{\"share_printers\":false,\"remote_any\":false,\"remote_admin\":false}" 2>&1); then
  expect "cupsctl | grep -q _share_printers=0"
  expect "get /printers | jq -e '\''length == 2'\''"
  send PUT /admin/cups "{\"share_printers\":true,\"remote_any\":true,\"remote_admin\":true}" >/dev/null
  expect "cupsctl | grep -q _share_printers=1"
else
  # Some CUPS builds take cupsd.conf changes from root only; the API must say so.
  expect "echo \"\$out\" | grep -qF \"HTTP 403\" && echo \"\$out\" | grep -qF \"sudo cupsctl --no-share-printers --no-remote-any --no-remote-admin\""
  echo "     (this CUPS refuses server settings from non-root users: checked the error instead)"
fi

echo "--- admin password"
send PUT /admin/password "{\"password\":\"secret123\"}" >/dev/null
rm -f $J
expect "get /session | jq -e '\''.password_set and (.admin | not)'\''"
expect "test \"\$(code $B/admin/settings)\" = 401"
expect "test \"\$(code -X PUT -H X-Reprinter:1 -H Content-Type:application/json -d {} $B/admin/printers/Capture)\" = 401"
expect "test \"\$(code -X POST -H X-Reprinter:1 -H Content-Type:application/json -d {\\\"password\\\":\\\"wrong\\\"} $B/login)\" = 401"
expect "send POST /print \"{\\\"upload_id\\\":\\\"$txt\\\",\\\"printer\\\":\\\"Capture\\\"}\""
send POST /login "{\"password\":\"secret123\"}" >/dev/null
expect "get /admin/settings | jq -e .web.password_set"
send PUT /admin/password "{\"password\":\"\"}" >/dev/null
rm -f $J
expect "get /session | jq -e '\''(.password_set | not) and .admin'\''"
send PUT /admin/password "{\"password\":\"another1\"}" >/dev/null
expect "runuser -u reprinter -- /usr/local/bin/reprinter-web --config /var/lib/reprinter/config.toml --clear-password"
expect "! grep -q \"admin_password_hash = \\\"\\\$argon2\" /var/lib/reprinter/config.toml"

echo "--- web settings"
send PUT /admin/web "{\"listen\":\"0.0.0.0:8631\",\"max_upload_mb\":1,\"upload_ttl_minutes\":30}" >/dev/null
expect "grep -q \"max_upload_mb = 1\$\" /var/lib/reprinter/config.toml"
head -c 1500000 /dev/zero | tr "\0" a >/tmp/big.txt
expect "test \"\$(code -H X-Reprinter:1 -F file=@/tmp/big.txt $B/uploads)\" = 413"
expect "test \"\$(code -X PUT -H X-Reprinter:1 -H Content-Type:application/json -d \"{\\\"listen\\\":\\\"nope\\\",\\\"max_upload_mb\\\":1,\\\"upload_ttl_minutes\\\":30}\" $B/admin/web)\" = 400"

echo "--- uploads are removed"
expect "test \"\$(code -X DELETE -H X-Reprinter:1 $B/uploads/$pdf)\" = 204"
expect "test \"\$(code $B/uploads/$pdf)\" = 404"
expect "test ! -e /var/lib/reprinter/uploads/$pdf"
expect "! grep -q \"^error:\" /tmp/web.log"

echo "--- rerun with --web-only, then uninstall"
/usr/local/bin/reprinter-web install --web-only --port 8632 >/tmp/reinstall.log 2>&1 || { cat /tmp/reinstall.log; exit 1; }
expect "grep -qF \"listen = \\\"0.0.0.0:8632\\\"\" /var/lib/reprinter/config.toml"
expect "grep -q \"max_upload_mb = 1\$\" /var/lib/reprinter/config.toml"
pkill -x reprinter-web || true
sleep 0.5
/opt/reprinter-web uninstall --purge >/tmp/uninstall.log 2>&1 || { cat /tmp/uninstall.log; exit 1; }
expect "test ! -e /usr/local/bin/reprinter-web"
expect "test ! -e /etc/systemd/system/reprinter-web.service"
expect "test ! -e /var/lib/reprinter"
expect "! id reprinter"
expect "lpstat -v LBP1210"
echo PASS
'

failed=()
for img in "${images[@]}"; do
  echo "=================== $img"
  if docker run --rm "${DOCKER_OPTS[@]}" -v "$BIN:/opt/reprinter-web:ro" \
    "$img" bash -c "$check"; then
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
