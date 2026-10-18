"""生成 Chroma 的应用图标（assets/chroma.ico + assets/chroma.png）。

图形语言取自设计稿的 **Logo C（双层盘）**，尺寸表见 `2:564`–`2:576`：
- 外盘：竖直渐变 `#7321D9 → #5E12B8`；
- 内盘：直径 80%（每边内缩 10%），渐变 `#B37AFF → #8F45F5`；
- 外沿 1px 白 18% 描边（只在 ≥64px 有意义）+ Mauve 30% 投影（0/12/28 @160，按比例）；
- 白色播放三角：宽 37.6%、高 47% 直径，圆角 4%。

比例来自设计稿矢量（三角 39%×48%）。用 `design/icons/chroma-{16,32,64,128,256}.png`
栅格反推会得到 0.375×0.47——那是抗锯齿边缘被阈值切掉的差，不是真值；
16px 也是照同一比例缩的，没有单独的 16px 重绘。
画布上另有一个 `2:528`（16px 圆角方块 + 小三角），实测 16px 下反而认不出来，
**不用于 ico**；本脚本按尺寸表走。

为什么按 16 倍超采样逐档重画而不是一个 master 缩小：
ico 的每一档都要单独落地，Pillow 的等比缩放会在 16/20 处把 1px 白环糊成灰边。

用法：python scripts/make-icon.py
"""

import math
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"

# 双层盘的端点色（Mauve 一族的明度台阶）
OUTER_TOP = (0x73, 0x21, 0xD9)
OUTER_BOTTOM = (0x5E, 0x12, 0xB8)
INNER_TOP = (0xB3, 0x7A, 0xFF)
INNER_BOTTOM = (0x8F, 0x45, 0xF5)
WHITE = (0xFF, 0xFF, 0xFF, 255)

# 几何比例（相对图标边长，实测自设计稿尺寸表）
INNER_RATIO = 0.80
RING_ALPHA = 0.18
TRI_W, TRI_H, TRI_R = 0.39, 0.48, 0.04
STROKE_UNIT = 160           # 设计稿的 1px 环 / 投影都按 160 边长为基准，其余尺寸等比

ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
SS = 16                     # 超采样倍数


def vgradient(size, top, bottom):
    """竖直渐变（设计稿的 gradientTransform 都是顶→底）。"""
    col = Image.new("RGB", (1, size))
    px = col.load()
    for y in range(size):
        t = y / max(size - 1, 1)
        px[0, y] = tuple(round(top[i] + (bottom[i] - top[i]) * t) for i in range(3))
    return col.resize((size, size), Image.BILINEAR)


def rounded_polygon(points, radius):
    """折线每个顶点倒圆弧，返回稠密点列（PIL 的 polygon 没有圆角）。"""
    out = []
    n = len(points)
    for i, cur in enumerate(points):
        prev, nxt = points[i - 1], points[(i + 1) % n]

        def unit(a, b):
            dx, dy = b[0] - a[0], b[1] - a[1]
            length = math.hypot(dx, dy) or 1.0
            return dx / length, dy / length

        p_in, p_out = unit(cur, prev), unit(cur, nxt)
        back = min(radius, math.dist(cur, prev) / 2, math.dist(cur, nxt) / 2)
        a = (cur[0] + p_in[0] * back, cur[1] + p_in[1] * back)
        b = (cur[0] + p_out[0] * back, cur[1] + p_out[1] * back)

        for step in range(17):
            t = step / 16
            out.append((
                (1 - t) ** 2 * a[0] + 2 * (1 - t) * t * cur[0] + t**2 * b[0],
                (1 - t) ** 2 * a[1] + 2 * (1 - t) * t * cur[1] + t**2 * b[1],
            ))
    return out


def add_triangle(img, size):
    d = ImageDraw.Draw(img)
    cx, cy = 0.5 * size, 0.5 * size
    tri_w, tri_h = TRI_W * size, TRI_H * size
    # 按**重心**居中（包围盒居中会看着偏左），再右移 0.5% 做光学修正
    x0 = cx - tri_w / 3 + 0.005 * size
    tri = [(x0, cy - tri_h / 2), (x0 + tri_w, cy), (x0, cy + tri_h / 2)]
    d.polygon(rounded_polygon(tri, TRI_R * size), fill=WHITE)


def build(px):
    S = px * SS
    # 不外挂投影：投影的模糊会被画布边缘裁成方形灰晕（浅色任务栏上很明显），
    # 系统自己会给任务栏图标做投影；设计稿里那层 Mauve 投影是画布上的展示效果。
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    pad = max(1, round(0.004 * S))              # 留一点透明边，缩放不切色块
    box = (pad, pad, S - pad - 1, S - pad - 1)
    dia = box[2] - box[0]

    # 外盘
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).ellipse(box, fill=255)
    img.paste(vgradient(S, OUTER_TOP, OUTER_BOTTOM), (0, 0), mask)

    # 内盘（每边内缩 10%）
    inset = round(dia * (1 - INNER_RATIO) / 2)
    inner = (box[0] + inset, box[1] + inset, box[2] - inset, box[3] - inset)
    inner_mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(inner_mask).ellipse(inner, fill=255)
    img.paste(vgradient(S, INNER_TOP, INNER_BOTTOM), (0, 0), inner_mask)

    # 外沿白环（18%）：16px 下只剩 0.1px，靠 anti-alias 自然淡掉，不做特判
    ring = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(ring).ellipse(
        box, outline=(255, 255, 255, round(255 * RING_ALPHA)),
        width=max(1, round(S / STROKE_UNIT)),
    )
    img.alpha_composite(ring)

    add_triangle(img, S)
    return img.resize((px, px), Image.LANCZOS)


def main():
    ASSETS.mkdir(exist_ok=True)

    png = ASSETS / "chroma.png"
    build(256).save(png)
    print(f"saved {png.relative_to(ROOT)} 256x256")

    # ico 每档单独画，不是缩放出来的
    frames = [build(s) for s in ICO_SIZES]
    ico = ASSETS / "chroma.ico"
    frames[-1].save(ico, format="ICO", append_images=frames[:-1],
                    sizes=[(s, s) for s in ICO_SIZES])
    print(f"saved {ico.relative_to(ROOT)} sizes={ICO_SIZES}")


main()
