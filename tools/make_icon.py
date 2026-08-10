#!/usr/bin/env python
# GlowAudio app icon generator.
#
# Renders the app icon programmatically (Pillow only, no SVG rasterizer needed)
# so the artwork is reproducible: tweak the constants, re-run, get every size.
#
# Usage:
#   python tools/make_icon.py --sheet              # concept contact sheet (review)
#   python tools/make_icon.py --master dial        # write the 1024px master PNG
#
# After --master, regenerate the platform icon set with:
#   npx tauri icon src-tauri/icons/icon-source.png

import argparse
import io
import math
import os
import struct
import sys

from PIL import Image, ImageChops, ImageDraw, ImageEnhance, ImageFilter, ImageFont

# ------------------------------------------------------------------ constants

SS = 4  # supersample factor; every shape is drawn at SS * size then downscaled

CYAN = (0, 240, 255)
PURPLE = (176, 38, 255)
PLATE_TOP = (26, 29, 42)
PLATE_BOTTOM = (10, 11, 16)

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


# -------------------------------------------------------------------- helpers


def linear_gradient(size, c0, c1, bbox=None):
    """Diagonal (top-left -> bottom-right) gradient as an RGB image.

    `bbox` maps the full colour range onto that box instead of the whole
    canvas, so a small glyph still spans cyan all the way to purple.
    """
    x0, y0, x1, y1 = bbox if bbox else (0, 0, size - 1, size - 1)
    span = max(1.0, (x1 - x0) + (y1 - y0))
    # Build one gradient row/col pair cheaply: value depends only on x+y.
    ramp = []
    for s in range((size - 1) * 2 + 1):
        t = min(1.0, max(0.0, (s - (x0 + y0)) / span))
        ramp.append(
            (
                int(c0[0] + (c1[0] - c0[0]) * t),
                int(c0[1] + (c1[1] - c0[1]) * t),
                int(c0[2] + (c1[2] - c0[2]) * t),
            )
        )
    grad = Image.new("RGB", (size, size))
    px = grad.load()
    for y in range(size):
        for x in range(size):
            px[x, y] = ramp[x + y]
    return grad


def colorize(mask, c0, c1, gradient=None):
    """Turn an L mask into an RGBA layer painted with the neon gradient."""
    grad = gradient if gradient is not None else linear_gradient(mask.size[0], c0, c1)
    layer = grad.convert("RGBA")
    layer.putalpha(mask)
    return layer


def screen_over(base_rgb, layer_rgba, strength=1.0):
    """Additive-ish light blend: composite the layer onto black, then screen."""
    lit = Image.new("RGB", base_rgb.size, (0, 0, 0))
    lit.paste(layer_rgba.convert("RGB"), (0, 0), layer_rgba)
    if strength != 1.0:
        lit = ImageEnhance.Brightness(lit).enhance(strength)
    return ImageChops.screen(base_rgb, lit)


def rounded_mask(size, radius_ratio=0.225):
    """Squircle-ish plate mask matching the Windows app-icon silhouette."""
    m = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(m)
    d.rounded_rectangle([0, 0, size - 1, size - 1], radius=int(size * radius_ratio), fill=255)
    return m


def plate_background(size):
    """Dark glass plate with a soft cyan bloom behind the glyph."""
    bg = linear_gradient(size, PLATE_TOP, PLATE_BOTTOM)

    # Radial bloom centred slightly above the middle, kept very dim.
    bloom = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(bloom)
    r = int(size * 0.42)
    cx, cy = size // 2, int(size * 0.46)
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=60)
    bloom = bloom.filter(ImageFilter.GaussianBlur(size * 0.18))
    tint = Image.new("RGBA", (size, size), CYAN + (0,))
    tint.putalpha(bloom)
    return screen_over(bg, tint, 0.55)


def dot(draw, cx, cy, r, fill=255):
    draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=fill)


def polyline(draw, pts, width, fill=255):
    """Stroke with round joints and round caps (ImageDraw has no round cap)."""
    draw.line(pts, fill=fill, width=int(width), joint="curve")
    r = width / 2.0
    dot(draw, pts[0][0], pts[0][1], r, fill)
    dot(draw, pts[-1][0], pts[-1][1], r, fill)


def bezier(p0, p1, p2, steps=64):
    out = []
    for i in range(steps + 1):
        t = i / steps
        u = 1 - t
        out.append(
            (
                u * u * p0[0] + 2 * u * t * p1[0] + t * t * p2[0],
                u * u * p0[1] + 2 * u * t * p1[1] + t * t * p2[1],
            )
        )
    return out


# ------------------------------------------------------------------- concepts
#
# Every concept draws into an L mask at canvas resolution N and returns it.
# Coordinates are expressed as fractions of N so the art scales cleanly.


def glyph_dial(N, compact=False):
    """Neon volume dial: 270 deg ring with a knob tip, EQ bars inside.

    `compact` is the 16-24 px variant: thicker strokes and wider gaps, since
    at those sizes the hairlines merge into a blob after downsampling.
    """
    m = Image.new("L", (N, N), 0)
    d = ImageDraw.Draw(m)

    cx = cy = N / 2
    r = N * (0.355 if compact else 0.345)
    w = N * (0.105 if compact else 0.078)
    d.arc([cx - r, cy - r, cx + r, cy + r], start=125, end=415, fill=255, width=int(w))

    # Round both arc ends, then a brighter knob at the sweep tip.
    for ang in (125, 55):
        a = math.radians(ang)
        dot(d, cx + r * math.cos(a), cy + r * math.sin(a), w / 2)
    a = math.radians(55)
    dot(d, cx + r * math.cos(a), cy + r * math.sin(a), w * (0.82 if compact else 0.95))

    # Bars inside the ring, clear of the stroke at every size.
    if compact:
        heights, bw, gap = (0.20, 0.36, 0.26), N * 0.085, N * 0.062
    else:
        heights, bw, gap = (0.19, 0.34, 0.24), N * 0.072, N * 0.055
    total = len(heights) * bw + (len(heights) - 1) * gap
    x = cx - total / 2
    for h in heights:
        half = N * h / 2
        d.rounded_rectangle([x, cy - half, x + bw, cy + half], radius=bw / 2, fill=255)
        x += bw + gap
    return m


def glyph_route(N, compact=False):
    """Routing fork: one source splitting into two glowing endpoints."""
    m = Image.new("L", (N, N), 0)
    d = ImageDraw.Draw(m)

    w = N * 0.082
    src = (N * 0.235, N * 0.5)
    mid = (N * 0.47, N * 0.5)
    up = (N * 0.775, N * 0.275)
    dn = (N * 0.775, N * 0.725)

    polyline(d, [src, mid], w)
    polyline(d, bezier(mid, (N * 0.66, N * 0.5), up), w)
    polyline(d, bezier(mid, (N * 0.66, N * 0.5), dn), w)

    dot(d, src[0], src[1], N * 0.088)
    dot(d, up[0], up[1], N * 0.088)
    dot(d, dn[0], dn[1], N * 0.088)

    # Signal ticks radiating from the source node.
    for i, ang in enumerate((-32, 0, 32)):
        a = math.radians(180 + ang)
        r0, r1 = N * 0.145, N * 0.215
        polyline(
            d,
            [
                (src[0] + r0 * math.cos(a), src[1] + r0 * math.sin(a)),
                (src[0] + r1 * math.cos(a), src[1] + r1 * math.sin(a)),
            ],
            w * 0.62,
        )
    return m


def glyph_bars(N, compact=False):
    """Equalizer bars, wave-shaped heights, fully rounded caps."""
    m = Image.new("L", (N, N), 0)
    d = ImageDraw.Draw(m)

    heights = (0.30, 0.54, 0.80, 0.46, 0.24)
    bw = N * 0.088
    gap = N * 0.052
    total = len(heights) * bw + (len(heights) - 1) * gap
    x = N / 2 - total / 2
    cy = N / 2
    for h in heights:
        half = N * h / 2
        d.rounded_rectangle([x, cy - half, x + bw, cy + half], radius=bw / 2, fill=255)
        x += bw + gap
    return m


def glyph_phones(N, compact=False):
    """Headphones: headband arc plus two ear cups."""
    m = Image.new("L", (N, N), 0)
    d = ImageDraw.Draw(m)

    cx, cy = N / 2, N * 0.5
    r = N * 0.29
    w = N * 0.085
    d.arc([cx - r, cy - r, cx + r, cy + r], start=185, end=355, fill=255, width=int(w))

    cup_w, cup_h = N * 0.145, N * 0.30
    for sx in (cx - r - w * 0.05, cx + r + w * 0.05):
        d.rounded_rectangle(
            [sx - cup_w / 2, cy - cup_h * 0.18, sx + cup_w / 2, cy + cup_h * 0.82],
            radius=cup_w / 2,
            fill=255,
        )
    return m


CONCEPTS = {
    "dial": glyph_dial,
    "route": glyph_route,
    "bars": glyph_bars,
    "phones": glyph_phones,
}


# --------------------------------------------------------------------- render


def render(concept, size, glow=None, compact=None):
    """Render one concept at `size` px, RGBA, with the plate and neon glow.

    Below 32 px the glow is dialled back and the compact glyph is used;
    a full-strength bloom just smears into an unreadable dot at tray size.
    """
    if compact is None:
        compact = size <= 32
    if glow is None:
        glow = 0.5 if compact else 1.0

    N = size * SS
    art = plate_background(N)
    mask = CONCEPTS[concept](N, compact)
    # Map the full cyan -> purple range onto the glyph itself, not the canvas.
    grad = linear_gradient(N, CYAN, PURPLE, mask.getbbox())
    layer = colorize(mask, CYAN, PURPLE, grad)

    # Two glow passes: a wide halo plus a tight bloom hugging the strokes.
    wide = colorize(mask.filter(ImageFilter.GaussianBlur(N * 0.045)), CYAN, PURPLE, grad)
    tight = colorize(mask.filter(ImageFilter.GaussianBlur(N * 0.012)), CYAN, PURPLE, grad)
    art = screen_over(art, wide, 0.55 * glow)
    art = screen_over(art, tight, 0.75 * glow)

    out = art.convert("RGBA")
    out = Image.alpha_composite(out, layer)

    # Hairline rim so the plate stays readable on a dark taskbar. At tray
    # sizes it is sub-pixel noise, so skip it there.
    if not compact:
        rim = Image.new("RGBA", (N, N), (0, 0, 0, 0))
        ImageDraw.Draw(rim).rounded_rectangle(
            [1, 1, N - 2, N - 2],
            radius=int(N * 0.225),
            outline=(255, 255, 255, 26),
            width=max(1, int(N * 0.006)),
        )
        out = Image.alpha_composite(out, rim)

    out.putalpha(rounded_mask(N))
    return out.resize((size, size), Image.LANCZOS)


# ---------------------------------------------------------------------- sheet


def _font(px, bold=False):
    names = ("segoeuib.ttf", "arialbd.ttf") if bold else ("segoeui.ttf", "arial.ttf")
    for name in names:
        path = os.path.join(os.environ.get("WINDIR", r"C:\Windows"), "Fonts", name)
        if os.path.exists(path):
            return ImageFont.truetype(path, px)
    return ImageFont.load_default()


def contact_sheet(out_path):
    """One PNG showing every concept at 256px plus 48/32/24/16 downsamples."""
    names = list(CONCEPTS)
    pad, big = 28, 256
    row_h = big + 76
    sheet = Image.new("RGBA", (pad * 2 + big + 340, pad + row_h * len(names)), (18, 19, 26, 255))
    d = ImageDraw.Draw(sheet)
    f_title = _font(22)
    f_small = _font(15)

    for i, name in enumerate(names):
        y = pad + i * row_h
        master = render(name, big)
        sheet.alpha_composite(master, (pad, y + 34))
        d.text((pad, y + 4), name, font=f_title, fill=(0, 240, 255, 255))

        # Small sizes on both a dark and a light strip (taskbar themes).
        x = pad + big + 40
        for bgc, label in (((14, 15, 20, 255), "dark"), ((243, 243, 243, 255), "light")):
            strip_y = y + 54 if label == "dark" else y + 160
            d.text((x - 12, strip_y - 32), label, font=f_small, fill=(130, 130, 140, 255))
            d.rectangle([x - 12, strip_y - 14, x + 260, strip_y + 74], fill=bgc)
            sx = x
            for s in (48, 32, 24, 16):
                small = render(name, s)
                sheet.alpha_composite(small, (sx, strip_y + (48 - s) // 2))
                d.text((sx, strip_y + 54), str(s), font=f_small, fill=(130, 130, 140, 255))
                sx += s + 26

    sheet.convert("RGB").save(out_path)
    print(f"wrote {out_path}")


# --------------------------------------------------------------------- master


def write_master(concept, out_path, size=1024):
    img = render(concept, size)
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    img.save(out_path)
    print(f"wrote {out_path} ({size}x{size}, concept={concept})")


# ------------------------------------------------------------------------ ico
#
# Written by hand instead of via Pillow's ICO encoder for two reasons:
#   1. every size gets its own tuned render (Pillow would downscale one image);
#   2. Pillow omits the 1bpp AND mask on 32bpp entries. Windows tolerates that
#      in most shells but not all, and the icon Tauri shipped has it, so match.
# Layout matches what `tauri icon` produces: DIB below 256, PNG at 256.

ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)


def _dib_entry(img):
    """32bpp bottom-up BITMAPINFOHEADER DIB plus the 1bpp AND mask."""
    w, h = img.size
    flipped = img.transpose(Image.FLIP_TOP_BOTTOM)
    r, g, b, a = flipped.split()
    pixels = Image.merge("RGBA", (b, g, r, a)).tobytes()

    # AND mask: 1 = transparent. Rows are padded to a 4-byte boundary.
    stride = ((w + 31) // 32) * 4
    alpha = a.load()
    mask = bytearray()
    for y in range(h):
        row = bytearray(stride)
        for x in range(w):
            if alpha[x, y] == 0:
                row[x // 8] |= 0x80 >> (x % 8)
        mask += row

    header = struct.pack(
        "<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, len(pixels) + len(mask), 0, 0, 0, 0
    )
    return header + pixels + bytes(mask)


def write_ico(concept, out_path, sizes=ICO_SIZES):
    entries = []
    for s in sizes:
        img = render(concept, s)
        if s >= 256:
            buf = io.BytesIO()
            img.save(buf, "png")
            entries.append((s, buf.getvalue()))
        else:
            entries.append((s, _dib_entry(img)))

    out = bytearray(struct.pack("<HHH", 0, 1, len(entries)))
    offset = 6 + 16 * len(entries)
    blob = bytearray()
    for s, data in entries:
        out += struct.pack(
            "<BBBBHHII", s if s < 256 else 0, s if s < 256 else 0, 0, 0, 1, 32, len(data), offset
        )
        offset += len(data)
        blob += data
    out += blob

    with open(out_path, "wb") as fp:
        fp.write(out)
    print(f"wrote {out_path} ({len(entries)} entries: {', '.join(str(s) for s in sizes)})")


# --------------------------------------------------------------------- assets

# Sizes that `tauri icon` gets wrong for us: it downscales the 1024 master,
# which smears the glow. Re-render these with the compact/low-glow variant.
RETUNE_PNGS = {
    "32x32.png": 32,
    "Square30x30Logo.png": 30,
    "Square44x44Logo.png": 44,
}


def write_assets(concept, icons_dir, public_dir):
    write_ico(concept, os.path.join(icons_dir, "icon.ico"))
    for name, size in RETUNE_PNGS.items():
        path = os.path.join(icons_dir, name)
        if os.path.exists(path):
            render(concept, size).save(path)
            print(f"retuned {path} ({size}x{size})")
    fav = os.path.join(public_dir, "icon.png")
    render(concept, 256).save(fav)
    print(f"wrote {fav} (256x256)")


# ----------------------------------------------------------------- installers
#
# NSIS and WiX both want plain BMP (no alpha), at fixed sizes:
#   NSIS  headerImage       150x57    top-right strip on every inner page
#   NSIS  sidebarImage      164x314   welcome / finish page
#   WiX   bannerPath        493x58    top banner on the inner pages
#   WiX   dialogImagePath   493x312   welcome / exit dialog background
# The two "banner" images sit on the installer's white chrome, so they are
# built light. The two large ones are ours end to end, so they get the dark
# neon panel - but only on the left 164px of the WiX dialog, because WixUI
# draws its title and body text over the rest.

INSTALLER_SIZES = {
    "nsis-header.bmp": (150, 57),
    "nsis-sidebar.bmp": (164, 314),
    "wix-banner.bmp": (493, 58),
    "wix-dialog.bmp": (493, 312),
}

INK = (32, 34, 47)
PANEL_W = 164


def panel_gradient(w, h, c0, c1):
    """Diagonal gradient over an arbitrary rectangle."""
    grad = Image.new("RGB", (w, h))
    px = grad.load()
    span = max(1, (w - 1) + (h - 1))
    ramp = [
        (
            int(c0[0] + (c1[0] - c0[0]) * s / span),
            int(c0[1] + (c1[1] - c0[1]) * s / span),
            int(c0[2] + (c1[2] - c0[2]) * s / span),
        )
        for s in range(span + 1)
    ]
    for y in range(h):
        for x in range(w):
            px[x, y] = ramp[x + y]
    return grad


def glyph_art(concept, size, compact=False):
    """The glyph and its two glow passes, without the icon plate."""
    N = size * SS
    mask = CONCEPTS[concept](N, compact)
    grad = linear_gradient(N, CYAN, PURPLE, mask.getbbox())
    parts = (
        colorize(mask, CYAN, PURPLE, grad),
        colorize(mask.filter(ImageFilter.GaussianBlur(N * 0.045)), CYAN, PURPLE, grad),
        colorize(mask.filter(ImageFilter.GaussianBlur(N * 0.012)), CYAN, PURPLE, grad),
    )
    return tuple(p.resize((size, size), Image.LANCZOS) for p in parts)


def paste_glyph(bg_rgb, concept, size, xy, glow=1.0):
    """Screen the glow onto an RGB background, then composite the glyph."""

    def placed(layer):
        c = Image.new("RGBA", bg_rgb.size, (0, 0, 0, 0))
        c.paste(layer, xy)
        return c

    layer, wide, tight = glyph_art(concept, size)
    out = screen_over(bg_rgb, placed(wide), 0.55 * glow)
    out = screen_over(out, placed(tight), 0.75 * glow)
    return Image.alpha_composite(out.convert("RGBA"), placed(layer)).convert("RGB")


def _centered(draw, text, font, cx, y, fill):
    draw.text((cx - draw.textlength(text, font=font) / 2, y), text, font=font, fill=fill)


def _tracked(draw, text, font, cx, y, fill, tracking=2):
    """Letter-spaced small caps line; Pillow has no tracking option."""
    widths = [draw.textlength(ch, font=font) for ch in text]
    total = sum(widths) + tracking * (len(text) - 1)
    x = cx - total / 2
    for ch, w in zip(text, widths):
        draw.text((x, y), ch, font=font, fill=fill)
        x += w + tracking


def brand_panel(w, h, concept="dial"):
    """Dark neon panel: glowing glyph over a wordmark. Used by both big images."""
    img = panel_gradient(w, h, (18, 20, 30), (7, 8, 12))

    # Cyan bloom behind the glyph so the panel is not a flat rectangle.
    bloom = Image.new("L", (w, h), 0)
    r = int(w * 0.62)
    ImageDraw.Draw(bloom).ellipse([w // 2 - r, int(h * 0.30) - r, w // 2 + r, int(h * 0.30) + r], fill=52)
    bloom = bloom.filter(ImageFilter.GaussianBlur(w * 0.30))
    tint = Image.new("RGBA", (w, h), CYAN + (0,))
    tint.putalpha(bloom)
    img = screen_over(img, tint, 0.6)

    g = int(w * 0.56)
    img = paste_glyph(img, concept, g, ((w - g) // 2, int(h * 0.16)))

    d = ImageDraw.Draw(img)
    cx = w / 2
    base = int(h * 0.16) + g + int(h * 0.055)
    _centered(d, "GlowAudio", _font(19, bold=True), cx, base, (226, 249, 255))
    _tracked(d, "DESKTOP", _font(9), cx, base + 26, (120, 126, 145), tracking=3)

    # Short neon rule under the wordmark.
    rule_w, rule_y = int(w * 0.34), base + 48
    rule = Image.new("L", (w, h), 0)
    ImageDraw.Draw(rule).rectangle(
        [cx - rule_w / 2, rule_y, cx + rule_w / 2, rule_y + 1], fill=255
    )
    grad = linear_gradient(max(w, h), CYAN, PURPLE).resize((w, h))
    line = grad.convert("RGBA")
    line.putalpha(rule)
    img = screen_over(img, line, 0.9)
    return img


def installer_header(w, h):
    """White strip: wordmark on the left, app tile on the right."""
    img = Image.new("RGB", (w, h), (255, 255, 255))
    tile = render("dial", 40)
    img.paste(tile, (w - 40 - 10, (h - 40) // 2), tile)
    d = ImageDraw.Draw(img)
    d.text((12, h // 2 - 15), "GlowAudio", font=_font(14, bold=True), fill=INK)
    d.text((13, h // 2 + 3), "Smart audio router", font=_font(10), fill=(122, 128, 145))
    return img


def installer_wix_banner(w, h):
    """White banner: WixUI writes its title on the left, so brand on the right."""
    img = Image.new("RGB", (w, h), (255, 255, 255))
    tile = render("dial", 38)
    img.paste(tile, (w - 38 - 14, (h - 38) // 2), tile)
    d = ImageDraw.Draw(img)
    label = "GlowAudio"
    f = _font(15, bold=True)
    d.text((w - 38 - 24 - d.textlength(label, font=f), h // 2 - 10), label, font=f, fill=INK)
    return img


def installer_wix_dialog(w, h):
    """Dark art panel on the left; the rest stays white for WixUI's text."""
    img = Image.new("RGB", (w, h), (255, 255, 255))
    img.paste(brand_panel(PANEL_W, h), (0, 0))
    return img


def write_installer_art(out_dir):
    os.makedirs(out_dir, exist_ok=True)
    builders = {
        "nsis-header.bmp": installer_header,
        "nsis-sidebar.bmp": lambda w, h: brand_panel(w, h),
        "wix-banner.bmp": installer_wix_banner,
        "wix-dialog.bmp": installer_wix_dialog,
    }
    for name, (w, h) in INSTALLER_SIZES.items():
        img = builders[name](w, h)
        assert img.size == (w, h), f"{name}: expected {(w, h)}, got {img.size}"
        path = os.path.join(out_dir, name)
        img.convert("RGB").save(path)  # 24bpp BMP; NSIS/WiX reject alpha
        print(f"wrote {path} ({w}x{h} BMP)")


def main():
    ap = argparse.ArgumentParser(description="GlowAudio icon generator")
    ap.add_argument("--sheet", metavar="PATH", nargs="?", const="icon-concepts.png")
    ap.add_argument("--master", metavar="CONCEPT", choices=list(CONCEPTS))
    ap.add_argument(
        "--assets",
        metavar="CONCEPT",
        choices=list(CONCEPTS),
        help="rewrite icon.ico, the small PNGs and the web favicon (run after `tauri icon`)",
    )
    ap.add_argument(
        "--installer",
        action="store_true",
        help="write the NSIS/WiX installer bitmaps into src-tauri/installer/",
    )
    ap.add_argument(
        "--out",
        default=os.path.join(REPO_ROOT, "src-tauri", "icons", "icon-source.png"),
    )
    ap.add_argument("--size", type=int, default=1024)
    args = ap.parse_args()

    if args.sheet:
        contact_sheet(args.sheet)
    if args.master:
        write_master(args.master, args.out, args.size)
    if args.assets:
        write_assets(
            args.assets,
            os.path.join(REPO_ROOT, "src-tauri", "icons"),
            os.path.join(REPO_ROOT, "public"),
        )
    if args.installer:
        write_installer_art(os.path.join(REPO_ROOT, "src-tauri", "installer"))
    if not (args.sheet or args.master or args.assets or args.installer):
        ap.print_help()
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
