#!/usr/bin/env python
"""把 Ardot 导出的原始帧归一化写进 design/frames/。

为什么需要这一步：`export_nodes` 的输出**留白不固定**（主窗四周 96px、Win 侧 64px、
小窗 88px，改动阴影半径还会跟着变），而且**阴影 alpha>0 的像素会被算进内容边界**，
直接量尺寸得到的不是窗口大小。所以这里不用「猜偏移量」，而是用 `alpha>=250` 找出
真正的不透水区域（= 窗口内容矩形），再按 `设计尺寸 × 2` 裁出来。

另一点：裁完之后画面外的透明像素虽然 alpha=0，RGB 往往还留着被阴影染过的底色，
不显式清零的话 `Image.convert('RGB')` 会泛一层绿/灰。

用法：
  python scripts/normalize-frames.py <raw_dir>
raw_dir 里的文件名是节点 id 把 `:` 换成 `_`，例如 `21_162.png`。
"""
import json
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SPEC = ROOT / "design" / "spec.json"
OUT = ROOT / "design" / "frames"

OPAQUE = 250  # alpha 阈值：≥ 这个值才算"窗口本体"，阴影/抗锯齿边缘都排除在外


def window_box(im: Image.Image) -> tuple[int, int, int, int]:
    """返回不透水内容的包围盒（左, 上, 右, 下）。"""
    alpha = im.getchannel("A")
    # point() 把 alpha 二值化成 0/255，再把 255 当白色求 bbox —— 比逐像素快得多
    mask = alpha.point(lambda v: 255 if v >= OPAQUE else 0)
    box = mask.getbbox()
    if box is None:
        raise SystemExit("整张图都没有不透水像素，导出多半是空的")
    return box


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    raw_dir = Path(sys.argv[1])

    spec = json.loads(SPEC.read_text(encoding="utf-8"))
    OUT.mkdir(parents=True, exist_ok=True)

    bad = []
    for frame in spec["frames"]:
        node = frame["node"]
        src = raw_dir / (node.replace(":", "_") + ".png")
        if not src.exists():
            bad.append(f"缺文件 {src.name}")
            continue

        im = Image.open(src).convert("RGBA")
        left, top, right, bottom = window_box(im)

        want_w, want_h = frame["size"][0] * 2, frame["size"][1] * 2
        got_w, got_h = right - left, bottom - top
        if (got_w, got_h) != (want_w, want_h):
            # 差 1px 是抗锯齿边界常事；差多了说明设计稿尺寸和 spec 对不上了
            print(
                f"! {frame['file']}: 窗口 {got_w}x{got_h} != spec {want_w}x{want_h}"
            )
            if abs(got_w - want_w) > 2 or abs(got_h - want_h) > 2:
                bad.append(f"{node} 尺寸 {got_w}x{got_h} ≠ {want_w}x{want_h}")

        cropped = im.crop((left, top, left + want_w, top + want_h))

        # 透明处 RGB 清零：不这么做，转 RGB 时会把阴影底色带出来
        clean = Image.new("RGBA", cropped.size, (0, 0, 0, 0))
        clean.paste(cropped, (0, 0), cropped)
        clean.save(OUT / Path(frame["file"]).name, optimize=True)

        print(f"  {frame['file']}: {want_w}x{want_h}  (偏移 {left},{top})")

    if bad:
        print("\n失败：")
        for b in bad:
            print("  -", b)
        return 1
    print(f"\n完成 {len(spec['frames'])} 帧 → {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
