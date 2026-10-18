#!/usr/bin/env python
"""把 design/frames/ 的 10 帧拼成 docs/design-overview.png（5×2 总览图）。

底色用 #F5F6FA 而不是纯白：窗口本体是 #EFF1F5，压在纯白上几乎融在一起，
描边删掉之后尤其明显。稍微压暗一点的"台面"能让投影把窗口托起来。
"""
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
FRAMES = ROOT / "design" / "frames"
OUT = ROOT / "docs" / "design-overview.png"

# mac 一行、win 一行：每行的语义是「同一组界面，两套窗控」。
# 视频播放态不在这里——它只有 HTML 原型一份（见 design/README.md）。
ORDER = [
    "mac-music-mode.png",
    "mac-settings.png",
    "mac-firstrun-recent.png",
    "mac-firstrun-empty.png",
    "mac-firstrun-drop.png",
    "win-music-mode.png",
    "win-settings.png",
    "win-firstrun-recent.png",
    "win-firstrun-empty.png",
    "win-firstrun-drop.png",
]

COLS = 5
CELL_W, CELL_H = 600, 430
PAD = 26          # 单元格内边距
GAP = 16          # 单元格之间
BG = (245, 246, 250)
LABEL_H = 0       # 需要标注时再改


def main() -> int:
    rows = (len(ORDER) + COLS - 1) // COLS
    sheet_w = COLS * CELL_W + (COLS + 1) * GAP
    sheet_h = rows * (CELL_H + LABEL_H) + (rows + 1) * GAP
    sheet = Image.new("RGB", (sheet_w, sheet_h), BG)
    draw = ImageDraw.Draw(sheet)

    for i, name in enumerate(ORDER):
        path = FRAMES / name
        if not path.exists():
            raise SystemExit(f"缺帧：{name}")
        im = Image.open(path).convert("RGBA")

        box_w, box_h = CELL_W - 2 * PAD, CELL_H - 2 * PAD
        r = min(box_w / im.width, box_h / im.height)
        im = im.resize((round(im.width * r), round(im.height * r)), Image.LANCZOS)

        col, row = i % COLS, i // COLS
        cell_x = GAP + col * (CELL_W + GAP)
        cell_y = GAP + row * (CELL_H + LABEL_H + GAP)

        # 单元格画一块极淡的白，让"台面"有层次，也标出图的实际占位
        draw.rectangle(
            [cell_x, cell_y, cell_x + CELL_W - 1, cell_y + CELL_H - 1],
            fill=(250, 251, 253),
        )
        sheet.paste(im, (cell_x + PAD, cell_y + PAD), im)

    sheet.save(OUT, optimize=True)
    print(f"{OUT.relative_to(ROOT)}: {sheet_w}x{sheet_h}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
