#!/bin/sh
# 用 QDK（qnap-dev/QDK 的 qbuild）构建 QNAP QPKG。仅在 Linux CI 上运行。
#
# 用法：build_qpkg.sh <version> <qdk_arch> <binary> <webroot_dir> <out_qpkg_path>
#   qdk_arch: x86_64 | arm_64
#
# 前置：qbuild 已在 PATH（CI 内 clone QDK 后 ./InstallToUbuntu.sh install），
#       imagemagick（convert）用于从 assets/logo/fluxdown_logo.png 生成 QDK 要求的 gif 图标。
set -eu

[ $# -eq 5 ] || { echo "usage: $0 <version> <x86_64|arm_64> <binary> <webroot> <out_qpkg>" >&2; exit 2; }
VERSION=$1 ARCH=$2 BIN=$3 WEBROOT=$4 OUT=$5

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
LOGO="$REPO_ROOT/assets/logo/fluxdown_logo.png"

[ -f "$BIN" ] || { echo "binary not found: $BIN" >&2; exit 1; }
[ -d "$WEBROOT" ] || { echo "webroot not found: $WEBROOT" >&2; exit 1; }
command -v qbuild >/dev/null || { echo "qbuild not in PATH (install QDK first)" >&2; exit 1; }
command -v convert >/dev/null || { echo "imagemagick 'convert' not in PATH" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
pkg="$work/FluxDown"

# ── 组装包目录：模板 + 版本号 + 载荷 ──
mkdir -p "$pkg"
cp "$SCRIPT_DIR/qpkg.cfg" "$pkg/"
cp -r "$SCRIPT_DIR/shared" "$pkg/shared"
chmod 755 "$pkg/shared/fluxdown.sh"
sed -i "s/^QPKG_VER=.*/QPKG_VER=\"$VERSION\"/" "$pkg/qpkg.cfg"

cp "$SCRIPT_DIR/package_routines" "$pkg/"

# 二进制放入 arch 专属目录（qbuild --build-arch 只打对应架构），webroot 各架构共用
mkdir -p "$pkg/$ARCH"
cp "$BIN" "$pkg/$ARCH/fluxdown-server"
chmod 755 "$pkg/$ARCH/fluxdown-server"
cp -r "$WEBROOT" "$pkg/shared/web"

# ── 图标（QDK 要求 gif：64x64 / 80x80 / 64x64 灰度） ──
mkdir -p "$pkg/icons"
convert "$LOGO" -resize 64x64 "$pkg/icons/FluxDown.gif"
convert "$LOGO" -resize 80x80 "$pkg/icons/FluxDown_80.gif"
convert "$LOGO" -resize 64x64 -colorspace Gray "$pkg/icons/FluxDown_gray.gif"

# ── 构建 ──
(cd "$pkg" && qbuild --build-arch "$ARCH")

built=$(find "$pkg/build" -name '*.qpkg' | head -n 1)
[ -n "$built" ] || { echo "qbuild produced no .qpkg" >&2; exit 1; }
mkdir -p "$(dirname "$OUT")"
cp "$built" "$OUT"
echo "built: $OUT"
