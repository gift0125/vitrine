"""生成 Chroma 的应用图标（assets/chroma.ico + assets/chroma.png）。

图形语言直接取自设计稿：品牌 Mauve `#8839EF`（唯一强调色）+ 白色播放三角，
和界面里那颗 44px 的播放按钮是同一个符号。纯平面、无投影，符合设计稿的层级口径。

为什么要自己生成而不是找现成的图：
- ico 必须带 16/20/24/32 这些**小尺寸**，缩小的图不能靠等比缩放糊出来，
  16px 下三角的视觉重量要和 256 一致（所以按 1024 超采样再逐档 LANCZOS 落盘）；
- 资源要能被仓库跟踪、可复现，改品牌色只改这一个脚本。

用法：python scripts/make-icon.py
"""

from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"

# 品牌色（Catppuccin Latte Mauve，设计稿唯一强调色）
MAUVE = (0x88, 0x39, 0xEF, 0xFF)
WHITE = (0xFF, 0xFF, 0xFF, 0xFF)

# 超采样基准：1024 够画圆弧，再往下落盘不会出现台阶
S = 1024
# 圆角半径按 Windows 图标惯例（约 22.5% 边长），比 iOS 的 22.37% 略方一点
CORNER = 0.225 * S
# 播放三角：包围盒宽 34%、高 40%，圆角 4%
TRI_W = 0.34 * S
TRI_H = 0.40 * S
TRI_R = 0.045 * S

ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def rounded_polygon(points, radius):
    """把折线的每个顶点倒成圆弧，返回稠密点列。

    PIL 的 `polygon` 没有圆角，直接画出来在 256px 下四个尖角会显得很硬。
    做法：每个顶点沿着两条相邻边各退 `radius`，再用二次贝塞尔（这里退化成
    圆弧采样）把两点连起来。
    """
    import math

    out = []
    n = len(points)
    for i, cur in enumerate(points):
        prev = points[i - 1]
        nxt = points[(i + 1) % n]

        def unit(a, b):
            dx, dy = b[0] - a[0], b[1] - a[1]
            length = math.hypot(dx, dy) or 1.0
            return dx / length, dy / length

        # 顶点 → 两侧的点（沿边后退 radius，短边自动截短）
        p_in = unit(cur, prev)
        p_out = unit(cur, nxt)
        # 退让距离不能超过边长的一半，否则相邻圆角会互相吃掉
        back = min(
            radius,
            math.dist(cur, prev) / 2,
            math.dist(cur, nxt) / 2,
        )
        a = (cur[0] + p_in[0] * back, cur[1] + p_in[1] * back)
        b = (cur[0] + p_out[0] * back, cur[1] + p_out[1] * back)

        # 圆弧：以 a / b 为端点、顶点为控制点的二次贝塞尔采样
        steps = 12
        for step in range(steps + 1):
            t = step / steps
            x = (1 - t) ** 2 * a[0] + 2 * (1 - t) * t * cur[0] + t**2 * b[0]
            y = (1 - t) ** 2 * a[1] + 2 * (1 - t) * t * cur[1] + t**2 * b[1]
            out.append((x, y))
    return out


def draw_icon() -> Image.Image:
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # 底：圆角方块。留 1px 透明边，任务栏/资源管理器缩放时不会切到色块边缘
    pad = 2
    d.rounded_rectangle(
        (pad, pad, S - pad, S - pad), radius=CORNER, fill=MAUVE
    )

    # 白色播放三角：按**重心**居中（包围盒居中会看着偏左），再右移 0.5% 做光学修正
    cx, cy = 0.5 * S, 0.5 * S
    x0 = cx - TRI_W / 3 + 0.005 * S
    top = cy - TRI_H / 2
    bottom = cy + TRI_H / 2
    tri = [
        (x0, top),
        (x0 + TRI_W, cy),
        (x0, bottom),
    ]
    d.polygon(rounded_polygon(tri, TRI_R), fill=WHITE)
    return img


def main():
    ASSETS.mkdir(exist_ok=True)
    master = draw_icon()

    png = ASSETS / "chroma.png"
    master.resize((256, 256), Image.LANCZOS).save(png)

    ico = ASSETS / "chroma.ico"
    master.save(ico, format="ICO", sizes=[(s, s) for s in ICO_SIZES])

    print(f"saved {png.relative_to(ROOT)} 256x256")
    print(f"saved {ico.relative_to(ROOT)} sizes={ICO_SIZES}")


main()
