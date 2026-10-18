"""生成两张对照图（数据都来自 target/lg 下的 2x 截图）：

  docs/liquid-glass.png         胶囊：L3 / 默认档（+边缘折射）/ +高光色散 / 位移场
  docs/liquid-glass-panels.png  面板：播放列表（浅底）与视频信息（暗底）各一组开/关

重拍命令（headless，两倍像素）：
  chrome --headless=new --hide-scrollbars --force-device-scale-factor=2 \
    --window-size=1280,800 --virtual-time-budget=6000 \
    --screenshot=target/lg/r_edge.png "file:///.../preview/index.html?bare=1"
  （off / full / glassdbg 分别加 &liquid=off / &liquid=full / &liquid=full&glassdbg=1）
  面板用整页 --window-size=1500,1000 + ?panel=playlist|osd。
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

LG = Path('target/lg')
FONT_S = ImageFont.truetype('C:/Windows/Fonts/msyh.ttc', 18)
LABEL_H = 34


def label(im, text, color, pad=14):
    row = Image.new('RGB', (im.width, im.height + LABEL_H), (17, 18, 23))
    row.paste(im, (0, LABEL_H))
    ImageDraw.Draw(row).text((pad, 8), text, font=FONT_S, fill=color)
    return row


def stack(rows, gap=12):
    W = max(r.width for r in rows)
    H = sum(r.height for r in rows) + gap * (len(rows) - 1)
    out = Image.new('RGB', (W, H), (10, 10, 13))
    y = 0
    for r in rows:
        out.paste(r, ((W - r.width) // 2, y))
        y += r.height + gap
    return out


def pair(a, box, b, ta, tb, scale, ca, cb):
    ia = Image.open(LG / a).convert('RGB').crop(box)
    ib = Image.open(LG / b).convert('RGB').crop(box)
    if scale != 1:
        size = (round(ia.width * scale), round(ia.height * scale))
        ia = ia.resize(size, Image.LANCZOS)
        ib = ib.resize(size, Image.LANCZOS)
    row = Image.new('RGB', (ia.width * 2 + 14, ia.height + LABEL_H), (17, 18, 23))
    row.paste(ia, (0, LABEL_H))
    row.paste(ib, (ia.width + 14, LABEL_H))
    d = ImageDraw.Draw(row)
    d.text((14, 8), ta, font=FONT_S, fill=ca)
    d.text((ia.width + 28, 8), tb, font=FONT_S, fill=cb)
    return row


# ---------- 1. 胶囊 ----------
BAND = (462, 1336, 2094, 1584)        # 整条胶囊（2x）
FIELD = (520, 1352, 1430, 1600)       # 左半段，放大展示位移场
FIELD_Z = 1.72

CAP_ROWS = [
    ('r_off.png',  BAND, 1.0, 'L3 真模糊 32px —— 液态玻璃：关', (208, 176, 128)),
    ('r_edge.png', BAND, 1.0, '默认档 · +边缘折射（位移 6.5 / 带 14；体部 30 · 边圈 7）', (150, 200, 255)),
    ('r_full.png', BAND, 1.0, '加强档 · +高光与色散（Fresnel 贴边 + R/B 分离 7%）', (200, 168, 244)),
    ('r_dbg.png',  FIELD, FIELD_Z, '位移场 · 左半段放大 1.7x（白 = 贴边 6.5px，黑 = 带内沿 0）', (150, 156, 172)),
]

built = []
for fn, box, z, tag, color in CAP_ROWS:
    im = Image.open(LG / fn).convert('RGB').crop(box)
    if z != 1.0:
        im = im.resize((round(im.width * z), round(im.height * z)), Image.LANCZOS)
    built.append(label(im, tag, color))

out = stack(built)
if out.width > 1560:
    out = out.resize((1560, round(out.height * 1560 / out.width)), Image.LANCZOS)
out.save('docs/liquid-glass.png', optimize=True)
print('docs/liquid-glass.png', out.size, Path('docs/liquid-glass.png').stat().st_size // 1024, 'KB')

# ---------- 2. 面板 ----------
# 整页坐标：窗口左上角 (110,28)，1280×800；歌单 right:24/top:16/320×633；OSD left:24/bottom:132
PL_BOX = (2085, 80, 2740, 1365)
OSD_BOX = (255, 985, 775, 1440)

pl = pair('p_pl.png', PL_BOX, 'p_ploff.png',
          '播放列表 · 默认档（浅底：奶白体部 + 亮边圈）', '播放列表 · 液态玻璃关',
          0.66, (150, 200, 255), (208, 176, 128))
osd = pair('p_osd.png', OSD_BOX, 'p_osdoff.png',
           '视频信息 · 默认档（暗底：自适应压暗）', '视频信息 · 液态玻璃关',
           0.85, (150, 200, 255), (208, 176, 128))

panels = stack([pl, osd])
if panels.width > 1560:
    panels = panels.resize((1560, round(panels.height * 1560 / panels.width)), Image.LANCZOS)
panels.save('docs/liquid-glass-panels.png', optimize=True)
print('docs/liquid-glass-panels.png', panels.size,
      Path('docs/liquid-glass-panels.png').stat().st_size // 1024, 'KB')
