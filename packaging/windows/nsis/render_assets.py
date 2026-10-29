#!/usr/bin/env python3
from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
ICON = ROOT / "assets" / "app-icon" / "typ-edit-icon-1024.png"
FONT_BOLD = Path("C:/Windows/Fonts/msyhbd.ttc")

SCALE = 4

BOARD = "#E6E6E6"
CARD = "#FFFFFF"
RAISED = "#F6F6F6"
BORDER = "#DEDEDE"
SHADOW = "#D8D8D8"
STRONG = "#1E1E1E"


def font(path: Path, size: int, index: int = 0) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(str(path), size * SCALE, index=index)


def rounded(
    draw: ImageDraw.ImageDraw,
    box: tuple[int, int, int, int],
    radius: int,
    fill: str,
    outline: str | None = None,
    width: int = 1,
) -> None:
    draw.rounded_rectangle(
        tuple(v * SCALE for v in box),
        radius=radius * SCALE,
        fill=fill,
        outline=outline,
        width=width * SCALE,
    )


def load_icon(size: int) -> Image.Image:
    """Load and resize the application icon."""
    return Image.open(ICON).convert("RGBA").resize(
        (size * SCALE, size * SCALE),
        Image.Resampling.LANCZOS,
    )


def draw_centered_text(
    draw: ImageDraw.ImageDraw,
    text: str,
    y: int,
    fnt: ImageFont.FreeTypeFont,
    fill: str,
    canvas_width: int,
) -> None:
    bbox = draw.textbbox((0, 0), text, font=fnt)
    tw = bbox[2] - bbox[0]
    th = bbox[3] - bbox[1]

    x = (canvas_width * SCALE - tw) // 2
    draw.text(
        (x, y * SCALE - bbox[1]),
        text,
        font=fnt,
        fill=fill,
    )


def make_welcome() -> Image.Image:
    """
    Output size: 164 x 314

    Only contains:
    - Logo
    - Typst Editor
    """

    img = Image.new(
        "RGB",
        (164 * SCALE, 314 * SCALE),
        BOARD,
    )
    draw = ImageDraw.Draw(img)

    # Card shadow
    rounded(
        draw,
        (9, 9, 157, 307),
        10,
        SHADOW,
    )

    # Card
    rounded(
        draw,
        (8, 8, 156, 306),
        10,
        CARD,
        BORDER,
        1,
    )

    # ---------------------------------------------------------
    # Logo
    # ---------------------------------------------------------
    #
    # 原来是 84x84。
    # 放大到 108x108，并水平居中。
    #
    logo_size = 108
    icon = load_icon(logo_size)

    logo_x = (164 - logo_size) // 2
    logo_y = 55

    img.paste(
        icon,
        (logo_x * SCALE, logo_y * SCALE),
        icon,
    )

    draw = ImageDraw.Draw(img)

    # ---------------------------------------------------------
    # App name
    # ---------------------------------------------------------
    #
    # 只保留软件名称。
    #
    app_name_font = font(FONT_BOLD, 15)

    draw_centered_text(
        draw,
        "Typst Editor",
        190,
        app_name_font,
        STRONG,
        164,
    )

    return img.resize(
        (164, 314),
        Image.Resampling.LANCZOS,
    )


def make_header() -> Image.Image:
    """
    Output size: 150 x 57

    Only contains:
    - Logo
    - Typst Editor
    """

    img = Image.new(
        "RGB",
        (150 * SCALE, 57 * SCALE),
        RAISED,
    )
    draw = ImageDraw.Draw(img)

    # ---------------------------------------------------------
    # Logo
    # ---------------------------------------------------------
    #
    # 原来是 32x32。
    # 放大到 40x40。
    #
    logo_size = 40
    icon = load_icon(logo_size)

    logo_x = 12
    logo_y = (57 - logo_size) // 2

    img.paste(
        icon,
        (logo_x * SCALE, logo_y * SCALE),
        icon,
    )

    draw = ImageDraw.Draw(img)

    # ---------------------------------------------------------
    # App name
    # ---------------------------------------------------------

    app_name_font = font(FONT_BOLD, 13)

    bbox = draw.textbbox(
        (0, 0),
        "Typst Editor",
        font=app_name_font,
    )

    text_width = bbox[2] - bbox[0]
    text_height = bbox[3] - bbox[1]

    text_x = 58
    text_y = (57 * SCALE - text_height) // 2

    draw.text(
        (text_x * SCALE, text_y - bbox[1]),
        "Typst Editor",
        font=app_name_font,
        fill=STRONG,
    )

    return img.resize(
        (150, 57),
        Image.Resampling.LANCZOS,
    )


def main() -> None:
    welcome = make_welcome()
    header = make_header()

    # BMP
    welcome.convert("RGB").save(
        HERE / "welcome.bmp",
        format="BMP",
    )

    header.convert("RGB").save(
        HERE / "header.bmp",
        format="BMP",
    )

    # Preview PNG
    welcome.save(
        HERE / "welcome-preview.png",
        format="PNG",
    )

    header.save(
        HERE / "header-preview.png",
        format="PNG",
    )

    print("wrote", HERE / "welcome.bmp", welcome.size)
    print("wrote", HERE / "header.bmp", header.size)


if __name__ == "__main__":
    main()
