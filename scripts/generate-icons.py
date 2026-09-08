#!/usr/bin/env python3
"""Generate the AgentStatusIndicator application icon from a source artwork.

The artwork (icons/artwork-source.png, 600x1200 RGBA) is a portrait vertical
traffic light on a transparent background. Each platform icon is a square
canvas with the artwork scaled to full height and centered horizontally, so
the art is never cropped (variant T chosen for the product).

Requires Pillow (maintainer-only tool):
  pip3 install Pillow

Outputs (relative to the repository root):
  icons/AgentStatusIndicator-1024.png   composed square master (1024x1024)
  icons/AppIcon.icns                    macOS bundle icon
  icons/AgentStatusIndicator.ico        Windows executable icon (multi-size)
  icons/hicolor/<size>/apps/agent-status-indicator.png   Linux icons
"""

import os
import shutil
import subprocess
import sys

try:
    from PIL import Image
except ImportError as error:  # pragma: no cover - exercised only without Pillow
    raise SystemExit(
        "generate-icons.py needs Pillow: pip3 install Pillow"
    ) from error

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SOURCE = os.path.join(ROOT, "icons", "artwork-source.png")
OUT = os.path.join(ROOT, "icons")


def compose_square(source, size):
    """Square canvas with the artwork scaled to full height, centered."""
    image = Image.open(source).convert("RGBA")
    scale = size / float(image.height)
    width = max(1, round(image.width * scale))
    art = image.resize((width, size), Image.LANCZOS)
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    canvas.alpha_composite(art, ((size - width) // 2, 0))
    return canvas


def write_png(path, image):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    image.save(path, format="PNG")


def main():
    if not os.path.isfile(SOURCE):
        raise SystemExit(f"missing source artwork: {SOURCE}")

    master = compose_square(SOURCE, 1024)

    # 1) Composed square master.
    master_path = os.path.join(OUT, "AgentStatusIndicator-1024.png")
    write_png(master_path, master)

    # 2) macOS iconset -> icns via iconutil.
    iconset = os.path.join(OUT, "AppIcon.iconset")
    if os.path.isdir(iconset):
        shutil.rmtree(iconset)
    os.makedirs(iconset)
    iconutil_spec = {
        "icon_16x16.png": 16,
        "icon_16x16@2x.png": 32,
        "icon_32x32.png": 32,
        "icon_32x32@2x.png": 64,
        "icon_128x128.png": 128,
        "icon_128x128@2x.png": 256,
        "icon_256x256.png": 256,
        "icon_256x256@2x.png": 512,
        "icon_512x512.png": 512,
        "icon_512x512@2x.png": 1024,
    }
    for name, size in iconutil_spec.items():
        write_png(os.path.join(iconset, name), compose_square(SOURCE, size))
    subprocess.run(
        ["iconutil", "-c", "icns", iconset, "-o", os.path.join(OUT, "AppIcon.icns")],
        check=True,
    )
    shutil.rmtree(iconset)

    # 3) Windows .ico with the usual size ladder (PNG-compressed entries).
    icon_sizes = [16, 24, 32, 48, 64, 128, 256]
    icons = [compose_square(SOURCE, size) for size in icon_sizes]
    icons[0].save(
        os.path.join(OUT, "AgentStatusIndicator.ico"),
        format="ICO",
        sizes=[(size, size) for size in icon_sizes],
        append_images=icons[1:],
    )

    # 4) Linux hicolor theme tree.
    for size in (16, 32, 48, 64, 128, 256, 512, 1024):
        icon_dir = os.path.join(OUT, "hicolor", f"{size}x{size}", "apps")
        write_png(
            os.path.join(icon_dir, "agent-status-indicator.png"),
            compose_square(SOURCE, size),
        )

    print("wrote:", master_path)
    print("wrote:", os.path.join(OUT, "AppIcon.icns"))
    print("wrote:", os.path.join(OUT, "AgentStatusIndicator.ico"))
    print("wrote:", os.path.join(OUT, "hicolor", "<size>/apps/..."))


if __name__ == "__main__":
    sys.exit(main())
