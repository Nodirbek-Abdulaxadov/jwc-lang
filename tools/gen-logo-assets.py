#!/usr/bin/env python3
"""Regenerate every derived JWC logo asset from the master artwork.

The master is `vscode-extension/logo-source.png` — the only file an artist
should ever edit. Everything else in the list below is derived from it, and
the only reason those files agree with each other is that this script made
them all in one pass. Hand-editing one of them makes the set drift, which is
how the marketplace listing once shipped a blank square for two months.

    pip install Pillow
    python3 tools/gen-logo-assets.py

Outputs:
    vscode-extension/icon.png          256x256   marketplace + file icon
    docs/static/img/logo.png           h=128     docusaurus navbar
    docs/static/img/favicon.ico        16..256   browser tab
    docs/static/img/jwc-social-card.png 1200x630 og:image / twitter card

Downscaling happens in premultiplied-alpha space (Pillow's "RGBa" mode).
The master's transparent pixels carry RGB (0,0,0), so a straight RGBA
resize bleeds black into every antialiased edge — visible as a dirty halo
at favicon sizes.
"""

from __future__ import annotations

import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
MASTER = ROOT / "vscode-extension" / "logo-source.png"

# Sampled from the master artwork.
TEAL_DEEP = (0, 116, 107)
TEAL_BRIGHT = (36, 192, 164)
CORAL = (253, 144, 124)

FONT_DIRS = [
    "/usr/share/fonts/truetype/liberation",
    "/usr/share/fonts/truetype/dejavu",
    "/Library/Fonts",
    "C:/Windows/Fonts",
]
FONT_BOLD = ["LiberationSans-Bold.ttf", "DejaVuSans-Bold.ttf", "Arial Bold.ttf"]
FONT_REG = ["LiberationSans-Regular.ttf", "DejaVuSans.ttf", "Arial.ttf"]


def load_font(names: list[str], size: int) -> ImageFont.FreeTypeFont:
    for d in FONT_DIRS:
        for n in names:
            p = Path(d) / n
            if p.exists():
                return ImageFont.truetype(str(p), size)
    return ImageFont.load_default(size)


def trimmed(img: Image.Image) -> Image.Image:
    """Crop to the artwork's alpha bounding box."""
    box = img.split()[3].getbbox()
    return img.crop(box) if box else img


def scaled(img: Image.Image, w: int, h: int) -> Image.Image:
    """Resize through premultiplied alpha so edges don't pick up black."""
    return img.convert("RGBa").resize((w, h), Image.LANCZOS).convert("RGBA")


def fit(art: Image.Image, box_w: int, box_h: int) -> Image.Image:
    """Scale `art` to the largest size fitting box_w x box_h, aspect intact."""
    ratio = min(box_w / art.width, box_h / art.height)
    return scaled(art, max(1, round(art.width * ratio)), max(1, round(art.height * ratio)))


def centered(art: Image.Image, w: int, h: int, fill_ratio: float) -> Image.Image:
    """Place `art` on a transparent w x h canvas at `fill_ratio` of the box."""
    art = fit(art, round(w * fill_ratio), round(h * fill_ratio))
    canvas = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    canvas.alpha_composite(art, ((w - art.width) // 2, (h - art.height) // 2))
    return canvas


def wrap(text: str, font: ImageFont.FreeTypeFont, max_w: int) -> list[str]:
    lines: list[str] = []
    words = text.split()
    line = ""
    for word in words:
        probe = f"{line} {word}".strip()
        if font.getlength(probe) <= max_w or not line:
            line = probe
        else:
            lines.append(line)
            line = word
    if line:
        lines.append(line)
    return lines


def social_card(art: Image.Image, out: Path) -> None:
    W, H = 1200, 630
    top, bottom = (6, 46, 42), (3, 26, 24)

    card = Image.new("RGBA", (W, H))
    draw = ImageDraw.Draw(card)
    for y in range(H):
        t = y / (H - 1)
        draw.line(
            [(0, y), (W, y)],
            fill=tuple(round(top[i] + (bottom[i] - top[i]) * t) for i in range(3)) + (255,),
        )

    # Hairline of brand teal along the top edge.
    draw.rectangle([0, 0, W, 5], fill=TEAL_BRIGHT + (255,))

    logo = fit(art, 360, 330)
    card.alpha_composite(logo, (96, (H - logo.height) // 2 - 10))

    x = 540
    title_font = load_font(FONT_BOLD, 132)
    tag_font = load_font(FONT_REG, 34)
    url_font = load_font(FONT_BOLD, 26)

    draw.text((x, 168), "JWC", font=title_font, fill=(255, 255, 255, 255))

    y = 330
    for line in wrap(
        "A backend-first language for SQL-native business-logic APIs",
        tag_font,
        W - x - 90,
    ):
        draw.text((x, y), line, font=tag_font, fill=(198, 226, 221, 255))
        y += 48

    draw.text((x, y + 26), "jwc.1kb.uz", font=url_font, fill=TEAL_BRIGHT + (255,))

    card.convert("RGB").save(out, "PNG", optimize=True)
    print(f"  {out.relative_to(ROOT)}  {W}x{H}")


def main() -> int:
    if not MASTER.exists():
        print(f"master artwork missing: {MASTER}", file=sys.stderr)
        return 1

    master = Image.open(MASTER).convert("RGBA")
    art = trimmed(master)
    print(f"master {MASTER.relative_to(ROOT)} {master.size} -> trimmed {art.size}")

    # Marketplace icon. 90% fill leaves the breathing room VS Code's own
    # rounded-tile treatment expects; CI rejects anything under 128x128.
    icon = ROOT / "vscode-extension" / "icon.png"
    centered(art, 256, 256, 0.90).save(icon, "PNG", optimize=True)
    print(f"  {icon.relative_to(ROOT)}  256x256")

    # Navbar logo: Docusaurus scales by height, so ship the natural aspect
    # ratio rather than a square with dead space either side.
    navbar = fit(art, 10_000, 120)
    canvas = Image.new("RGBA", (navbar.width + 8, 128), (0, 0, 0, 0))
    canvas.alpha_composite(navbar, (4, 4))
    logo_png = ROOT / "docs" / "static" / "img" / "logo.png"
    canvas.save(logo_png, "PNG", optimize=True)
    print(f"  {logo_png.relative_to(ROOT)}  {canvas.width}x{canvas.height}")

    # Favicon. Pillow renders every entry from the 256px master rather than
    # from each other, so the 16px one stays legible. Stopping at 128 keeps
    # the file near 20 KB — a 256px entry alone triples that, and nothing
    # requests a favicon that large.
    fav = ROOT / "docs" / "static" / "img" / "favicon.ico"
    centered(art, 256, 256, 0.96).save(
        fav,
        "ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128)],
    )
    print(f"  {fav.relative_to(ROOT)}  16..128")

    social_card(art, ROOT / "docs" / "static" / "img" / "jwc-social-card.png")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
