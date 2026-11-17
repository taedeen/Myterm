"""Builds docs/images from the CI screenshots (branch guide-screenshots).

    git fetch origin guide-screenshots && mkdir -p /tmp/shots && git archive origin/guide-screenshots | tar -x -C /tmp/shots
    python3 docs/images/compose.py /tmp/shots docs/images

Phone screens go side by side in pairs, scaled down; Windows screens are
scaled to 1280 wide. Output: out/*.png
"""
import sys
from pathlib import Path

from PIL import Image

src = Path(sys.argv[1])
out = Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)

BG = (30, 33, 51)  # the app's navy, so the gaps blend in
PHONE_W = 480       # each phone screen in a pair
GAP = 40


def phone(name: str) -> Image.Image:
    im = Image.open(src / f"phone-{name}.png").convert("RGB")
    h = round(im.height * PHONE_W / im.width)
    return im.resize((PHONE_W, h), Image.LANCZOS)


def pair(left: str, right: str, name: str) -> None:
    a, b = phone(left), phone(right)
    h = max(a.height, b.height)
    canvas = Image.new("RGB", (PHONE_W * 2 + GAP * 3, h + GAP * 2), BG)
    canvas.paste(a, (GAP, GAP))
    canvas.paste(b, (GAP * 2 + PHONE_W, GAP))
    canvas.save(out / f"{name}.png", optimize=True)
    print(name, canvas.size)


def windows(name: str) -> None:
    im = Image.open(src / f"windows-{name}.png").convert("RGB")
    im.save(out / f"windows-{name}.png", optimize=True)
    print(f"windows-{name}", im.size)


pair("first-run", "vault", "phone-first-run")
pair("hosts", "host-edit", "phone-hosts")
pair("host-key", "terminal", "phone-session")
pair("settings", "devices", "phone-sync")
for n in ("split", "settings", "devices"):
    windows(n)
