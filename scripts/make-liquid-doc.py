"""生成 docs/liquid-glass.png：L3 / +边缘折射 / +高光色散 / 位移场 四行对照。

数据来自 target/lg 下 2x 截图（?bare=1 的窗口本身）。
前三行裁整条胶囊（同框同尺寸，可直接上下比对）；第四行是位移场，
只取左半段并放大 1.7x —— 标出来是放大图，避免和前几行混淆。
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

LG = Path('target/lg')
OUT = Path('docs/liquid-glass.png')

BAND = (462, 1336, 2094, 1584)        # 整条胶囊（2x）：520-58 .. 2040+54
FIELD = (520, 1352, 1430, 1600)       # 左半段，用于放大展示位移场
FIELD_Z = 1.72

FONT = ImageFont.truetype('C:/Windows/Fonts/msyh.ttc', 20)
LABEL_H = 36
GAP = 12

ROWS = [
    ('r_off.png',  BAND, 1.0, 'L3  真模糊 32px —— 现在的定稿', (208, 176, 128)),
    ('r_edge.png', BAND, 1.0, 'L4-a  +边缘折射（位移 6.5px / 带 14px；边圈改 7px 清亮）', (150, 200, 255)),
    ('r_full.png', BAND, 1.0, 'L4-b  +高光与色散（Fresnel 贴边 5px + R/B 分离 7%）', (200, 168, 244)),
    ('r_dbg.png',  FIELD, FIELD_Z, '位移场 · 左半段放大 1.7x（白 = 贴边 6.5px，黑 = 带内沿 0）', (150, 156, 172)),
]

rows = []
for fn, box, z, tag, color in ROWS:
    im = Image.open(LG / fn).convert('RGB').crop(box)
    if z != 1.0:
        im = im.resize((round(im.width * z), round(im.height * z)), Image.LANCZOS)
    rows.append((im, tag, color))

W = max(r[0].width for r in rows)
built = []
for im, tag, color in rows:
    row = Image.new('RGB', (W, im.height + LABEL_H), (17, 18, 23))
    row.paste(im, ((W - im.width) // 2, LABEL_H))
    ImageDraw.Draw(row).text((18, 8), tag, font=FONT, fill=color)
    built.append(row)

H = sum(r.height for r in built) + GAP * (len(built) - 1)
out = Image.new('RGB', (W, H), (10, 10, 13))
y = 0
for r in built:
    out.paste(r, (0, y))
    y += r.height + GAP

if out.width > 1560:
    out = out.resize((1560, round(out.height * 1560 / out.width)), Image.LANCZOS)
out.save(OUT, optimize=True)
print(OUT, out.size, OUT.stat().st_size // 1024, 'KB')
