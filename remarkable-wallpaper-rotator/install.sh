#!/usr/bin/env bash
# install.sh — run this ON YOUR COMPUTER (not the reMarkable).
#
# Deploys the wallpaper-rotation script + systemd timer to a reMarkable
# device over SSH, and (optionally) uploads a folder of pre-sized images.
#
# Usage:
#   ./install.sh <device-ip> [local-image-folder]
#
# Example:
#   ./install.sh 10.11.99.1 ./my-wallpapers-prepared
#
# Requirements:
#   - Developer mode + SSH enabled on the device (Settings > About >
#     Copyrights and licenses shows the root password and IP).
#   - USB or WiFi connection to the device.

set -euo pipefail

DEVICE_IP="${1:?Usage: ./install.sh <device-ip> [local-image-folder]}"
LOCAL_IMAGES="${2:-}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REMOTE_IMAGE_DIR="/home/root/customization/images/suspended"
REMOTE_SCRIPT_DIR="/usr/share/remarkable/scripts"
SSH_TARGET="root@${DEVICE_IP}"

echo "==> Connecting to reMarkable at ${DEVICE_IP} ..."
ssh "$SSH_TARGET" "mount -o remount,rw / && mkdir -p '${REMOTE_IMAGE_DIR}' '${REMOTE_SCRIPT_DIR}'"

echo "==> Uploading rotation script ..."
scp "${SCRIPT_DIR}/device/rotate-wallpaper.sh" "${SSH_TARGET}:${REMOTE_SCRIPT_DIR}/rotate-wallpaper.sh"
ssh "$SSH_TARGET" "chmod +x '${REMOTE_SCRIPT_DIR}/rotate-wallpaper.sh'"

if [ -n "$LOCAL_IMAGES" ]; then
  if [ ! -d "$LOCAL_IMAGES" ]; then
    echo "!! Image folder not found: $LOCAL_IMAGES (skipping image upload)" >&2
  else
    echo "==> Uploading images from ${LOCAL_IMAGES} ..."
    shopt -s nullglob
    files=("$LOCAL_IMAGES"/*.png "$LOCAL_IMAGES"/*.PNG "$LOCAL_IMAGES"/*.jpg "$LOCAL_IMAGES"/*.jpeg)
    shopt -u nullglob
    if [ "${#files[@]}" -eq 0 ]; then
      echo "!! No .png/.jpg/.jpeg files found in $LOCAL_IMAGES" >&2
    else
      scp "${files[@]}" "${SSH_TARGET}:${REMOTE_IMAGE_DIR}/"
    fi
  fi
else
  echo "==> No image folder given — remember to scp images into:"
  echo "    ${REMOTE_IMAGE_DIR}/ on the device before the timer runs."
fi

echo "==> Installing systemd service + timer ..."
scp "${SCRIPT_DIR}/device/random-screens.service" "${SSH_TARGET}:/etc/systemd/system/random-screens.service"
scp "${SCRIPT_DIR}/device/random-screens.timer" "${SSH_TARGET}:/etc/systemd/system/random-screens.timer"

ssh "$SSH_TARGET" "systemctl daemon-reload && systemctl enable --now random-screens.timer && systemctl start random-screens.service"

echo
echo "==> Done."
echo "    Lock the device (short press the power button) to preview the new sleep screen."
echo "    Rotation interval: edit OnUnitActiveSec in /etc/systemd/system/random-screens.timer"
echo "    on the device, then run: systemctl daemon-reload && systemctl restart random-screens.timer"
echo "    Note: a reMarkable firmware update will wipe these customizations — rerun this script after updating."
