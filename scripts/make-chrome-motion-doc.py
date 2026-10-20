"""生成 docs/chrome-dissolve.png：控制栏「散成尘」的相位 + 标题带收走的端点。

五条横带，全 1:1（粒子只有 1~2px，一缩就没了）：
  常态（整条）→ 0.06 / 0.22 / 0.40（只裁胶囊带）→ 闲置稳态（整条）

重拍命令：
  chrome --headless=new --hide-scrollbars --force-device-scale-factor=2 \
    --window-size=1280,800 --virtual-time-budget=5000 \
    --screenshot=target/lg/m_0.22.png \
    "file:///.../preview/index.html?bare=1&idle=1&dust=0.22"
  （常态去掉 &idle=1；闲置稳态去掉 &dust=）
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

LG = Path('target/lg')
OUT = Path('docs/chrome-dissolve.png')

FONT = ImageFont.truetype('C:/Windows/Fonts/msyh.ttc', 18)
LABEL_H = 34
BG = (10, 10, 13)
BAR = (17, 18, 23)

# 1280×800 逻辑 → 2x 像素：字幕 1144..1194，胶囊 1376..1558（含投影到 ~1590）
FULL = (460, 1210, 2100, 1610)   # 1640×400
BAND = (460, 1300, 2100, 1620)   # 1640×320，粒子飘散够用


def label(im, text, color):
    row = Image.new('RGB', (im.width, im.height + LABEL_H), BAR)
    row.paste(im, (0, LABEL_H))
    ImageDraw.Draw(row).text((14, 8), text, font=FONT, fill=color)
    return row


def crop(name, box):
    return Image.open(LG / name).convert('RGB').crop(box)


rows = [label(crop('m_rest.png', FULL),
              '常态 · 标题带 + 控制栏（L4 液态玻璃）', (150, 214, 255))]

for name, cap in [
    ('m_0.05.png', '0.05 · 轮廓先松：12px 带内先起尘，中心那块最厚、最后走'),
    ('m_0.22.png', '0.22 · 沿 SDF 法线外推：上下两条边分别向上 / 向下拉开'),
    ('m_0.40.png', '0.40 · 叠浮力：整体向上飘，切向摆让它像"气"而不是射线'),
]:
    rows.append(label(crop(name, BAND), cap, (232, 196, 120)))

rows.append(label(crop('m_idle.png', FULL),
                  '闲置稳态 · 标题带向上收走 12px + 淡出，顶部压暗一并淡掉', (150, 214, 255)))

out = Image.new('RGB', (1640, sum(r.height for r in rows) + 12 * (len(rows) - 1)), BG)
y = 0
for r in rows:
    out.paste(r, (0, y))
    y += r.height + 12
out.save(OUT)
print(OUT, out.size)
