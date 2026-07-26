#!/bin/sh
# rotate-wallpaper.sh
#
# Runs ON the reMarkable device (busybox ash, not bash). Picks a random
# image from a folder and installs it as the sleep-screen (suspended.png).
#
# Usage: rotate-wallpaper.sh [IMAGE_DIR] [TARGET_PNG]
#   IMAGE_DIR   folder of candidate images (default: /home/root/customization/images/suspended)
#   TARGET_PNG  system image to overwrite   (default: /usr/share/remarkable/suspended.png)
#
# Called periodically by random-screens.timer via random-screens.service.

set -eu

IMAGE_DIR="${1:-/home/root/customization/images/suspended}"
TARGET="${2:-/usr/share/remarkable/suspended.png}"
BACKUP_DIR="$(dirname "$TARGET")"
BACKUP="${BACKUP_DIR}/$(basename "$TARGET" .png).original.png"

# The root filesystem is mounted read-only after every boot / firmware
# update, so every run tries to remount it read-write first. Harmless if
# it's already rw.
mount -o remount,rw / 2>/dev/null || true

# Keep a one-time copy of reMarkable's stock image so it's easy to restore.
if [ -f "$TARGET" ] && [ ! -f "$BACKUP" ]; then
  cp "$TARGET" "$BACKUP"
fi

if [ ! -d "$IMAGE_DIR" ]; then
  echo "rotate-wallpaper: image folder not found: $IMAGE_DIR" >&2
  exit 1
fi

CANDIDATES=$(find "$IMAGE_DIR" -maxdepth 1 -type f \
  \( -iname '*.png' -o -iname '*.jpg' -o -iname '*.jpeg' \) 2>/dev/null)

if [ -z "$CANDIDATES" ]; then
  echo "rotate-wallpaper: no .png/.jpg/.jpeg files in $IMAGE_DIR" >&2
  exit 1
fi

PICK=$(printf '%s\n' "$CANDIDATES" | \
  awk -v seed="$(date +%s)$$" 'BEGIN{srand(seed)} {a[NR]=$0} END{if(NR>0) print a[int(rand()*NR)+1]}')

if [ -z "$PICK" ]; then
  echo "rotate-wallpaper: failed to pick an image" >&2
  exit 1
fi

cp "$PICK" "$TARGET"
echo "rotate-wallpaper: set $(basename "$TARGET") <- $PICK"
