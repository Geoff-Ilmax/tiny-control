#!/usr/bin/env bash
# Throwaway research helper: run Omarchy's Windows VM with the Tiny 3 passed
# through and every USB packet written to a pcap by QEMU itself.
#
# It never edits /var/lib/omarchy/windows/docker-compose.yml. It layers a
# temporary override on top of it, generated here at run time. The next normal
# start through Omarchy's Windows launcher recreates the container from the
# untouched compose, without the camera.
#
#   capture.sh start NAME   pause tiny-guard/tiny-remote, start the VM, record to NAME.pcap
#   capture.sh stop         stop the VM, copy the pcap here, restart the services
set -euo pipefail

BASE=/var/lib/omarchy/windows/docker-compose.yml
PROJECT_DIR=/var/lib/omarchy/windows
HOST_SHARED=/var/lib/omarchy/windows/mounts/users/1000/shared
HERE=$(cd "$(dirname "$0")" && pwd)
OUT="$HERE/../captures"
OVERRIDE="$HERE/.override.yml"
VIDPID=3564:ff02

dc() { docker compose -p windows --project-directory "$PROJECT_DIR" -f "$BASE" -f "$OVERRIDE" "$@"; }

case "${1:-}" in
start)
  name=${2:?usage: capture.sh start NAME}
  [[ $name =~ ^[A-Za-z0-9_-]+$ ]] || { echo "NAME: letters, digits, - and _ only" >&2; exit 1; }
  line=$(lsusb -d "$VIDPID") || { echo "Tiny 3 ($VIDPID) not found" >&2; exit 1; }
  bus=$(awk '{print $2}' <<<"$line"); dev=$(awk '{sub(":","",$4); print $4}' <<<"$line")
  node=/dev/bus/usb/$bus/$dev
  echo "Tiny 3 at $node"
  systemctl --user stop tiny-guard.service tiny-remote.service
  # Only the Tiny 3's own node goes into the container, not the whole USB bus.
  cat >"$OVERRIDE" <<EOF
services:
  windows:
    devices:
      - $node:$node
    environment:
      ARGUMENTS: "-rtc base=localtime,clock=host,driftfix=slew -device usb-host,bus=xhci.0,hostbus=$((10#$bus)),hostaddr=$((10#$dev)),pcap=/shared/$name.pcap"
EOF
  echo "$name" >"$HERE/.current"
  dc up -d
  echo "VM starting with the Tiny 3 attached. Open http://127.0.0.1:8006 or RDP to 127.0.0.1."
  ;;
stop)
  name=$(cat "$HERE/.current" 2>/dev/null || true)
  dc down
  mkdir -p "$OUT"
  if [[ -n $name && -r $HOST_SHARED/$name.pcap ]]; then
    cp "$HOST_SHARED/$name.pcap" "$OUT/" && echo "saved captures/$name.pcap ($(stat -c %s "$OUT/$name.pcap") bytes)"
  else
    echo "no readable pcap for '$name' in $HOST_SHARED" >&2
  fi
  rm -f "$OVERRIDE" "$HERE/.current"
  systemctl --user start tiny-guard.service tiny-remote.service
  echo "VM stopped, services restarted. Give the Tiny 3 a few seconds to come back to Linux."
  ;;
*) sed -n 2,13p "$0"; exit 1 ;;
esac
