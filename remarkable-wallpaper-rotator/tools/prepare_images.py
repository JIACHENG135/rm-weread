#!/usr/bin/env python3
"""
prepare_images.py — run this on YOUR COMPUTER before uploading images.

Resizes/crops a folder of arbitrary images to the reMarkable's native
screen resolution and saves them as PNGs, ready to be copied into
/home/root/customization/images/suspended on the device (see install.sh).

Usage:
    python3 prepare_images.py <src_folder> <dst_folder> [options]

Options:
    --width N       target width  (default 1620, reMarkable Paper Pro)
    --height N      target height (default 2160, reMarkable Paper Pro)
                    use --width 1404 --height 1872 for reMarkable 2
    --grayscale     convert to grayscale (recommended: cleaner E Ink
                    rendering and smaller files)

Example:
    python3 prepare_images.py ~/Pictures/wallpapers ./prepared --grayscale
"""
import argparse
import pathlib
import sys

try:
    from PIL import Image, ImageOps
except ImportError:
    sys.exit("Missing dependency. Install with: pip install pillow --break-system-packages")

SUPPORTED = {".png", ".jpg", ".jpeg", ".webp", ".bmp", ".tiff", ".tif"}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("src", help="folder of source images")
    parser.add_argument("dst", help="output folder (created if missing)")
    parser.add_argument("--width", type=int, default=1620, help="target width in px (default 1620)")
    parser.add_argument("--height", type=int, default=2160, help="target height in px (default 2160)")
    parser.add_argument("--grayscale", action="store_true", help="convert to grayscale for cleaner E Ink rendering")
    args = parser.parse_args()

    src = pathlib.Path(args.src).expanduser()
    dst = pathlib.Path(args.dst).expanduser()
    if not src.is_dir():
        sys.exit(f"Source folder not found: {src}")
    dst.mkdir(parents=True, exist_ok=True)

    files = sorted(p for p in src.iterdir() if p.suffix.lower() in SUPPORTED)
    if not files:
        sys.exit(f"No supported images found in {src} (looked for {sorted(SUPPORTED)})")

    for i, path in enumerate(files, 1):
        try:
            img = Image.open(path)
            img = ImageOps.exif_transpose(img)
            img = img.convert("RGB")
            fitted = ImageOps.fit(img, (args.width, args.height), method=Image.LANCZOS, centering=(0.5, 0.5))
            if args.grayscale:
                fitted = ImageOps.grayscale(fitted).convert("RGB")
            out_name = f"wallpaper_{i:03d}.png"
            fitted.save(dst / out_name, format="PNG")
            print(f"[{i}/{len(files)}] {path.name} -> {out_name}")
        except Exception as e:
            print(f"[{i}/{len(files)}] SKIPPED {path.name}: {e}", file=sys.stderr)

    print(f"\nDone. Prepared images are in: {dst}")
    print("Next: ./install.sh <device-ip> " + str(dst))


if __name__ == "__main__":
    main()
