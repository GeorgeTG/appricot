#!/bin/sh
# Moves the stand-in app's override-redirect popup window far outside its parent's box, the
# moment it maps, so the browser pass can see whether the popup stays clamped. Dev-only.
#
#   $1  where to move it to, in root coordinates (default 1000,620)
#
# The popup is the 160x120 root child with no name; it is mapped one second after the stand-in
# app starts and unmapped one second later, so this polls every 100 ms.
set -u
target="${1:-1000 620}"
i=0
while [ "${i}" -lt 60 ]; do
  id=$(xwininfo -root -children 2>/dev/null | awk '$0 ~ /160x120/ { print $1; exit }')
  if [ -n "${id}" ]; then
    # shellcheck disable=SC2086
    xdotool windowmove "${id}" ${target}
    echo "moved popup ${id} to ${target}"
    exit 0
  fi
  i=$((i + 1))
  sleep 0.1
done
echo "no popup found to move"
exit 0
