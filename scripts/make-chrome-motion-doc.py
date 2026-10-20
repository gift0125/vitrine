"""生成 docs/chrome-dissolve.png：控制层退场的粒子消散方向对照。

两段：
  上 = 同一相位（0.42）下三档方向并排：锋面 / 横流 / 四周散
  下 = 锋面档从 0.08 → 0.88 的进程（每行左沿逐格后移 = 从左右推进）

全 1:1（粒子只有 1~2px，一缩就没了），裁切按胶囊位置固定。

重拍命令（全部带 seed=7，保证三档是同一批粒子）：
  chrome --headless=new --hide-scrollbars --force-device-scale-factor=2 \
    --window-size=1280,800 --virtual-time-budget=5000 \
    --screenshot=target/lg/g_front.png \
    "file:///.../preview/index.html?bare=1&idle=1&seed=7&dust=0.42&dustflow=front"
  相位进程把 &dustflow 去掉、改 &dust=<0.08|0.24|0.40|0.56|0.72|0.88>，存成 h_<相位>.png
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
LG = ROOT / 'target' / 'lg'
OUT = ROOT / 'docs' / 'chrome-dissolve.png'

# 2x 裸窗口里胶囊那一带。胶囊实际逻辑 y=688..779（物理 1376..1558，实测 rectInWin），
# 上下各留一点余量放"飘出去"的粒子——只裁到 1520 会把整个下沿切掉。
BOX = (460, 1330, 2120, 1590)
PANEL_W, PANEL_H = BOX[2] - BOX[0], BOX[3] - BOX[1]
LABEL_W = 170

BG = (13, 14, 18)
FG = (236, 238, 244)
DIM = (138, 142, 156)
ACCENT = (198, 160, 246)
RULE = (38, 40, 48)

F_TITLE = ImageFont.truetype('C:/Windows/Fonts/msyh.ttc', 26)
F_LABEL = ImageFont.truetype('C:/Windows/Fonts/msyh.ttc', 21)
F_SMALL = ImageFont.truetype('C:/Windows/Fonts/msyh.ttc', 17)

TIERS = [
    ('g_front.png', '锋面', 'front', '默认 · 贴轮廓'),
    ('g_side.png', '横流', 'side', '整块被抽走'),
    ('g_out.png', '四周散', 'out', '上一版 · 沿法线'),
]
PHASES = ['0.08', '0.24', '0.40', '0.56', '0.72', '0.88']

GAP = 8
HEAD = 88
SUB = 44
ROW = PANEL_H

H = HEAD + len(TIERS) * (ROW + GAP) + SUB + len(PHASES) * (ROW + GAP) + 16
canvas = Image.new('RGB', (LABEL_W + PANEL_W, H), BG)
d = ImageDraw.Draw(canvas)

d.text((20, 20), '控制层退场 · 粒子消散方向', font=F_TITLE, fill=FG)
d.text((20, 56), '同一批粒子（seed=7）· scatter 640ms', font=F_SMALL, fill=DIM)

y = HEAD
for name, cn, key, note in TIERS:
    canvas.paste(Image.open(LG / name).convert('RGB').crop(BOX), (LABEL_W, y))
    d.text((20, y + 8), cn, font=F_LABEL, fill=ACCENT if key == 'front' else FG)
    d.text((20, y + 36), key, font=F_SMALL, fill=DIM)
    d.text((20, y + 60), note, font=F_SMALL, fill=DIM)
    y += ROW + GAP

d.line([(0, y + 10), (canvas.width, y + 10)], fill=RULE, width=2)
y += SUB

d.text((20, y - 32), '锋面 · 相位进程', font=F_SMALL, fill=DIM)
for p in PHASES:
    canvas.paste(Image.open(LG / ('h_%s.png' % p)).convert('RGB').crop(BOX), (LABEL_W, y))
    d.text((20, y + 8), p, font=F_LABEL, fill=FG)
    y += ROW + GAP

OUT.parent.mkdir(parents=True, exist_ok=True)
canvas.save(OUT)
print('wrote %s  %dx%d' % (OUT, canvas.width, canvas.height))
