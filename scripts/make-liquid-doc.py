"""生成 docs/liquid-glass.png：L3 / +边缘折射 / +高光色散 / 位移场 四行对照。

数据来自 target/lg 下 2x 截图（?bare=1 的窗口本身）。
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

LG = Path('target/lg')
OUT = Path('docs/liquid-glass.png')

BOX = (470, 1330, 2090, 1590)      # 胶囊带（2x 像素）
FIELD_BOX = (470, 1330, 2090, 1590)

FONT = ImageFont.truetype('C:/Windows/Fonts/msyh.ttc', 24)
FONT_S = ImageFont.truetype('C:/Windows/Fonts/msyh.ttc', 19)

ROWS = [
    ('r_off.png',  'L3  真模糊 32px —— 现在的定稿', (196, 154, 108)),
    ('r_edge.png', 'L4-a  +边缘折射（位移 6.5px / 带 14px，边圈 7px 清亮）', (147, 197, 253)),
    ('r_full.png', 'L4-b  +高光与色散（Fresnel 贴边 5px + R/B 分离 7%）', (196, 160, 240)),
    ('w_dbg.png',  '位移场（白=贴边 6.5px → 黑=带内沿 0）', (140, 145, 160)),
]

LABEL_H = 40
tape = Image.new('RGB', (BOX[2] - BOX[0], 0))
rows = []
for fn, tag, color in ROWS:
    box = FIELD_BOX if fn.startswith('w_dbg') else BOX
    im = Image.open(LG / fn).convert('RGB').crop(box)
    row = Image.new('RGB', (im.width, im.height + LABEL_H), (17, 18, 23))
    row.paste(im, (0, LABEL_H))
    d = ImageDraw.Draw(row)
    d.text((18, 9), tag, font=FONT_S, fill=color)
    rows.append(row)

GAP = 10
W = rows[0].width
H = sum(r.height for r in rows) + GAP * (len(rows) - 1)
out = Image.new('RGB', (W, H), (10, 10, 13))
y = 0
for r in rows:
    out.paste(r, (0, y))
    y += r.height + GAP

# 缩到 1560 宽（文档里够看，文件不至于太大）
if out.width > 1560:
    h = round(out.height * 1560 / out.width)
    out = out.resize((1560, h), Image.LANCZOS)
out.save(OUT, optimize=True)
print(OUT, out.size, OUT.stat().st_size // 1024, 'KB')
