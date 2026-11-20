#!/usr/bin/env bash
# Root-scope installer for tiny-control: udev rule + system config.
# Run once with sudo:  sudo ./packaging/install-root.sh
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "run me with sudo: sudo $0" >&2
    exit 1
fi

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RULE=/etc/udev/rules.d/71-tiny-control.rules

echo "==> installing udev rule -> $RULE"
install -Dm644 "$REPO/packaging/udev/71-tiny-control.rules" "$RULE"

# The cold-plug WB pin RUN+= calls tinyctl, which must be at a path root can exec.
# Resolve the real location (AUR: /usr/bin; local install: ~SUDO_USER/.local/bin
# or /usr/local/bin) and rewrite the rule's RUN path to match. If none is found,
# strip the RUN line so udev doesn't log failures — the daemon still covers the
# stream-open case.
user_home=""
[[ -n "${SUDO_USER:-}" ]] && user_home="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
tinyctl_path=""
for cand in /usr/bin/tinyctl /usr/local/bin/tinyctl ${user_home:+"$user_home/.local/bin/tinyctl"}; do
    if [[ -x "$cand" ]]; then tinyctl_path="$cand"; break; fi
done

if [[ -n "$tinyctl_path" && "$tinyctl_path" != "/usr/bin/tinyctl" ]]; then
    echo "==> pointing the cold-plug WB pin at $tinyctl_path"
    sed -i "s#/usr/bin/tinyctl #${tinyctl_path} #g" "$RULE"
elif [[ -z "$tinyctl_path" ]]; then
    echo "==> tinyctl not found on a root-executable path; removing the cold-plug WB pin"
    # Strip only the RUN+= action: the access (uaccess/GROUP) part must stay.
    sed -i 's#, RUN+="[^"]*tinyctl[^"]*"##' "$RULE"
fi

echo "==> installing system config (kept if it exists) -> /etc/tiny-control/config"
if [[ ! -f /etc/tiny-control/config ]]; then
    install -Dm644 "$REPO/packaging/config.example" /etc/tiny-control/config
else
    echo "    (existing /etc/tiny-control/config left untouched)"
fi

echo "==> reloading udev rules"
udevadm control --reload-rules
# NB: this trigger re-applies the rule to an already-plugged camera, which fires
# the WB pin now (a control write, not a stream — it does not start streaming).
udevadm trigger --action=add --subsystem-match=video4linux

echo "Done. (A replug also applies the rule.)"
