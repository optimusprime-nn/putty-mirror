# Draws deck.ico: a dark terminal with a rust-coloured session rail on its left
# and a prompt. Needs Pillow. Run it here, from the assets folder.
from PIL import Image, ImageDraw

INK = (24, 27, 32, 255)
EDGE = (58, 63, 71, 255)
RUST = (214, 88, 32, 255)
SLOT = (120, 46, 16, 255)
CREAM = (246, 238, 226, 255)
SCALE = 8
SIZES = [256, 64, 48, 40, 32, 24, 20, 16]


def draw(size):
    n = size * SCALE
    u = n / 256
    img = Image.new('RGBA', (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    small = size <= 24
    inset = 0 if small else 8 * u
    box = (inset, inset, n - inset - 1, n - inset - 1)
    radius = (40 if size > 32 else 52) * u
    d.rounded_rectangle(box, radius, fill=INK)

    # The rail is clipped to the tile, so it keeps the rounded corners
    rail = Image.new('RGBA', (n, n), (0, 0, 0, 0))
    r = ImageDraw.Draw(rail)
    width = 80 * u if small else 72 * u
    r.rectangle((0, 0, inset + width, n), fill=RUST)
    # Session rows, where there's room for them
    if size >= 32:
        for i in range(3):
            top = (56 + i * 52) * u
            r.rounded_rectangle((inset + 18 * u, top, inset + width - 18 * u, top + 24 * u), 8 * u, fill=SLOT if i else CREAM)
    mask = Image.new('L', (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle(box, radius, fill=255)
    img.paste(rail, (0, 0), Image.composite(rail, Image.new('RGBA', (n, n)), mask).getchannel('A'))
    # A dark tile would vanish on a dark title bar without its edge
    ImageDraw.Draw(img).rounded_rectangle(box, radius, outline=EDGE, width=SCALE if small else int(4 * u))
    if small:
        return pixel_prompt(img.resize((size, size), Image.LANCZOS))

    # The prompt
    stroke = 26 * u
    x = 112 * u
    top, bottom = 78 * u, 154 * u
    tip = (x + 54 * u, (top + bottom) / 2)
    d = ImageDraw.Draw(img)
    d.line([(x, top), tip, (x, bottom)], fill=CREAM, width=int(stroke), joint='curve')
    for p in [(x, top), tip, (x, bottom)]:
        d.ellipse((p[0] - stroke / 2, p[1] - stroke / 2, p[0] + stroke / 2, p[1] + stroke / 2), fill=CREAM)
    under = 188 * u
    d.rectangle((x - stroke / 2, under - stroke / 2, n - 44 * u, under + stroke / 2), fill=RUST)
    return img.resize((size, size), Image.LANCZOS)


def pixel_prompt(img):
    """Small sizes get the prompt pixel by pixel, as smoothing would blur it."""
    size = img.width
    left, top, half, thick, under, end = {16: (6, 3, 3, 2, 11, 13), 20: (8, 4, 4, 2, 14, 16), 24: (9, 5, 5, 3, 18, 20)}[size]
    for y in range(top, top + half * 2 + 1):
        offset = half - abs(y - top - half)
        for x in range(left + offset, left + offset + thick):
            img.putpixel((x, y), CREAM)
    for y in (under, under + 1):
        for x in range(left, end):
            img.putpixel((x, y), RUST)
    return img


images = [draw(s) for s in SIZES]
images[0].save('deck.ico', format='ICO', sizes=[(s, s) for s in SIZES], append_images=images[1:])
