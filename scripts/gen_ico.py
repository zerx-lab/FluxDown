#!/usr/bin/env python3
"""
gen_ico.py — 将高分辨率 PNG 转换为 Windows 多分辨率 ICO 文件

用法:
    python scripts/gen_ico.py

源文件: assets/logo/fluxdown_logo.png
输出:   windows/runner/resources/app_icon.ico

ICO 包含的分辨率（Windows 标准）:
    16×16   任务栏最小图标
    32×32   任务栏 / 窗口标题栏
    48×48   文件资源管理器默认视图
    64×64   中等图标视图
    256×256 桌面大图标 / 高 DPI 缓存（PNG 压缩）

依赖: Pillow（pip install Pillow）
"""

import io
import struct
import sys
from pathlib import Path

from PIL import Image

sys.stdout.reconfigure(encoding="utf-8")

SIZES = [16, 32, 48, 64, 256]

REPO_ROOT = Path(__file__).resolve().parent.parent
SRC = REPO_ROOT / "assets" / "logo" / "fluxdown_logo.png"
DST = REPO_ROOT / "windows" / "runner" / "resources" / "app_icon.ico"


def build_ico(src_img: Image.Image, sizes: list[int], dst: Path) -> None:
    """
    手动组装 ICO 二进制格式，每帧以 PNG 压缩存储。

    ICO 布局：
      [6B  文件头]
      [16B × N 目录条目]
      [各帧 PNG 数据]
    """
    frames: list[bytes] = []
    for size in sizes:
        resized = src_img.resize((size, size), Image.LANCZOS)
        buf = io.BytesIO()
        resized.save(buf, format="PNG", optimize=True)
        frames.append(buf.getvalue())

    n = len(sizes)
    # 文件头：reserved=0, type=1(ICO), count=n
    header = struct.pack("<HHH", 0, 1, n)

    # 每个图像数据的起始偏移 = 6（头）+ 16*n（目录）
    data_offset = 6 + 16 * n
    offsets: list[int] = []
    for data in frames:
        offsets.append(data_offset)
        data_offset += len(data)

    # 目录条目（16 字节 × n）
    directory = bytearray()
    for size, data, offset in zip(sizes, frames, offsets):
        # width/height: 0 代表 256（ICO 规范，单字节无法存 256）
        w = h = size if size < 256 else 0
        directory += struct.pack(
            "<BBBBHHII",
            w, h,       # width, height
            0,          # color count (0 = 真彩色)
            0,          # reserved
            1,          # planes
            32,         # bits per pixel (RGBA)
            len(data),  # 数据大小（字节）
            offset,     # 数据偏移
        )

    dst.parent.mkdir(parents=True, exist_ok=True)
    with dst.open("wb") as f:
        f.write(header)
        f.write(directory)
        for data in frames:
            f.write(data)


def main() -> None:
    if not SRC.exists():
        raise FileNotFoundError(f"源文件不存在: {SRC}")

    src_img = Image.open(SRC).convert("RGBA")
    print(f"源文件: {SRC.relative_to(REPO_ROOT)}  ({src_img.width}×{src_img.height})")
    print(f"目标:   {DST.relative_to(REPO_ROOT)}")
    print(f"分辨率: {', '.join(f'{s}×{s}' for s in SIZES)}\n")

    build_ico(src_img, SIZES, DST)

    size_kb = DST.stat().st_size / 1024
    print(f"文件大小: {size_kb:.1f} KB")

    # 验证
    ico = Image.open(DST)
    print("验证帧:")
    try:
        idx = 0
        while True:
            ico.seek(idx)
            print(f"  帧 {idx}: {ico.size[0]}×{ico.size[1]}")
            idx += 1
    except EOFError:
        pass

    print("\n完成。")


if __name__ == "__main__":
    main()
