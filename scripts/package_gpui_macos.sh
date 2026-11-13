#!/usr/bin/env bash
# GPUI 客户端 macOS 打包：构建 → FluxDown.app（agent 辅助 bundle）→ 签名 → DMG → 公证 → staple。
#
# 布局（与 native/agent/src/platform/mod.rs、crates/app/src/service_bootstrap.rs 约定一致）：
#   FluxDown.app/Contents/MacOS/fluxdown-desktop                       界面（Dock 图标）
#   FluxDown.app/Contents/Helpers/FluxDownAgent.app/Contents/MacOS/
#       fluxdown-agent, fluxdownd                                      常驻后台（LSUIElement，无 Dock 图标）
#   辅助 bundle id = 外层 bundle id + ".agent"（agent 据此还原外层 id 注册 .torrent / URL scheme）。
#
# 环境变量（均可选）：
#   ARCHS                  默认 "arm64 x86_64"（两者都有时 lipo 成 universal）
#   SKIP_BUILD=1           复用 target/<triple>/release 下已有产物
#   MACOS_CERT_P12_PATH + MACOS_CERT_PASSWORD   导入临时钥匙串做 Developer ID 签名
#   MACOS_SIGN_IDENTITY    已在默认钥匙串中的签名身份（与 P12 二选一）；都没有时 ad-hoc 签名
#   APPLE_API_KEY_PATH + APPLE_API_KEY_ID + APPLE_API_ISSUER_ID   提供时执行公证与 staple
#   FLUXCLOUD_BASE_URL     透传给 agent 编译期（见 native/agent/src/runtime.rs）
#   OUT_DIR                默认 build/gpui-macos
#   VERSION                产物版本（默认取 cargo metadata 的 fluxdown_ui_app.version；release 传 tag 版本，可带 -rc.N）
#   ARTIFACT_PREFIX        产物名前缀（默认 FluxDown-GPUI-$VERSION-macos-<suffix>；release 传
#                          FluxDown-$VERSION-macos-<x64|arm64>），生成 <prefix>.dmg 与 <prefix>.tar.gz
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO"

ARCHS=${ARCHS:-"arm64 x86_64"}
OUT_DIR=${OUT_DIR:-"$REPO/build/gpui-macos"}
HOST_ID=com.fluxdown.app
HELPER_ID="$HOST_ID.agent"
HELPER_NAME=FluxDownAgent.app
# GPUI 上游最低支持 10.15.7；Apple Silicon 由系统限定为 11.0 起。
MIN_X86=10.15.7
MIN_ARM=11.0
if [ -z "${VERSION:-}" ]; then
  if ! METADATA=$(cargo metadata --no-deps --format-version 1); then
    echo "failed to read cargo metadata for the default VERSION" >&2
    exit 1
  fi
  PACKAGE_INDEX=0
  while PACKAGE_NAME=$(plutil -extract "packages.$PACKAGE_INDEX.name" raw -o - - <<<"$METADATA" 2>/dev/null); do
    if [ "$PACKAGE_NAME" = fluxdown_ui_app ]; then
      if ! VERSION=$(plutil -extract "packages.$PACKAGE_INDEX.version" raw -o - - <<<"$METADATA"); then
        echo "failed to read fluxdown_ui_app.version from cargo metadata" >&2
        exit 1
      fi
      break
    fi
    PACKAGE_INDEX=$((PACKAGE_INDEX + 1))
  done
  if [ -z "${VERSION:-}" ]; then
    echo "cargo metadata did not provide a version for fluxdown_ui_app" >&2
    exit 1
  fi
  unset METADATA PACKAGE_INDEX PACKAGE_NAME
fi
# Info.plist 版本号只接受数字点分，去掉预发布后缀
PLIST_VERSION=${VERSION%%-*}

triple_of() {
  case "$1" in
    arm64) echo aarch64-apple-darwin ;;
    x86_64) echo x86_64-apple-darwin ;;
    *) echo "unsupported arch: $1" >&2; exit 2 ;;
  esac
}

min_of() { [ "$1" = x86_64 ] && echo "$MIN_X86" || echo "$MIN_ARM"; }

# ── 构建 ──
if [ "${SKIP_BUILD:-0}" != 1 ]; then
  for arch in $ARCHS; do
    triple=$(triple_of "$arch")
    echo "== build $triple (min macOS $(min_of "$arch"))"
    FLUXDOWN_APP_VERSION="${FLUXDOWN_APP_VERSION:-$VERSION}" MACOSX_DEPLOYMENT_TARGET=$(min_of "$arch") cargo build --locked --release --target "$triple" \
      -p fluxdown_ui_app -p fluxdown_agent -p fluxdown_daemon -p fluxdown_nmh \
      --features fluxdown_agent/desktop --bins
  done
fi

LSMIN=$MIN_ARM
SUFFIX=
for arch in $ARCHS; do
  [ "$arch" = x86_64 ] && LSMIN=$MIN_X86
  SUFFIX="${SUFFIX:+$SUFFIX-}$arch"
done
[ "$(wc -w <<<"$ARCHS")" -gt 1 ] && SUFFIX=universal

WORK="$OUT_DIR/work"
rm -rf "$WORK"
mkdir -p "$WORK/dmg_src"
APP="$WORK/dmg_src/FluxDown.app"
HELPER="$APP/Contents/Helpers/$HELPER_NAME"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$HELPER/Contents/MacOS" "$HELPER/Contents/Resources"

# ── 合并各架构产物 ──
place() {
  local bin=$1 dest=$2 inputs=()
  for arch in $ARCHS; do
    inputs+=("target/$(triple_of "$arch")/release/$bin")
  done
  if [ ${#inputs[@]} -gt 1 ]; then
    lipo -create "${inputs[@]}" -output "$dest"
  else
    cp "${inputs[0]}" "$dest"
  fi
  chmod 755 "$dest"
}
place fluxdown-desktop "$APP/Contents/MacOS/fluxdown-desktop"
place fluxdown-agent "$HELPER/Contents/MacOS/fluxdown-agent"
place fluxdownd "$HELPER/Contents/MacOS/fluxdownd"
# 浏览器扩展中继：agent 在自身可执行文件同目录查找（native/agent/src/nmh.rs::find_nmh_exe）
place fluxdown_nmh "$HELPER/Contents/MacOS/fluxdown_nmh"

# ── 图标 ──
ICONSET="$WORK/AppIcon.iconset"
mkdir -p "$ICONSET"
SRC_ICONS="assets/logo/macos"
for pair in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x 128:128x128 256:128x128@2x \
  256:256x256 512:256x256@2x 512:512x512 1024:512x512@2x; do
  cp "$SRC_ICONS/app_icon_${pair%%:*}.png" "$ICONSET/icon_${pair#*:}.png"
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
cp "$APP/Contents/Resources/AppIcon.icns" "$HELPER/Contents/Resources/AppIcon.icns"

# ── 外层 Info.plist：以 packaging/macos/Info.plist 为模板，含 URL scheme / .torrent 文档类型 / UTI ──
PL="$APP/Contents/Info.plist"
cp packaging/macos/Info.plist "$PL"
plutil -replace CFBundleDevelopmentRegion -string en "$PL"
plutil -replace CFBundleExecutable -string fluxdown-desktop "$PL"
plutil -replace CFBundleIconFile -string AppIcon "$PL"
plutil -replace CFBundleIdentifier -string "$HOST_ID" "$PL"
plutil -replace CFBundleName -string FluxDown "$PL"
plutil -replace CFBundleShortVersionString -string "$PLIST_VERSION" "$PL"
plutil -replace CFBundleVersion -string "$PLIST_VERSION" "$PL"
plutil -replace LSMinimumSystemVersion -string "$LSMIN" "$PL"
plutil -replace NSHumanReadableCopyright -string "Copyright © 2026 FluxDown" "$PL"
plutil -replace NSHighResolutionCapable -bool true "$PL"
plutil -lint "$PL"

# ── 辅助 bundle Info.plist：LSUIElement 让常驻 agent 不出现在 Dock ──
HPL="$HELPER/Contents/Info.plist"
cat >"$HPL" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>fluxdown-agent</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundleIdentifier</key>
	<string>$HELPER_ID</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>FluxDown</string>
	<key>CFBundleDisplayName</key>
	<string>FluxDown</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>$LSMIN</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSLocalNetworkUsageDescription</key>
	<string>FluxDown uses your local network to forward BT and eD2K ports on your router (UPnP / NAT-PMP) and to connect to paired devices and download hosts.</string>
</dict>
</plist>
EOF
plutil -lint "$HPL"

# ── 签名：由内到外，Hardened Runtime + 时间戳 ──
KC=
ORIG_KCS=()
SIGN_ARGS=()
cleanup() {
  # 先恢复用户搜索列表再删除临时钥匙串，避免残留指向已删除文件的条目。
  if [ ${#ORIG_KCS[@]} -gt 0 ]; then
    security list-keychains -d user -s "${ORIG_KCS[@]}" 2>/dev/null || true
  fi
  if [ -n "$KC" ]; then security delete-keychain "$KC" 2>/dev/null || true; fi
}
trap cleanup EXIT
if [ -n "${MACOS_CERT_P12_PATH:-}" ]; then
  KC="$WORK/signing.keychain-db"
  KC_PASS=$(uuidgen)
  security create-keychain -p "$KC_PASS" "$KC"
  security set-keychain-settings "$KC"
  security unlock-keychain -p "$KC_PASS" "$KC"
  security import "$MACOS_CERT_P12_PATH" -k "$KC" -P "${MACOS_CERT_PASSWORD:?MACOS_CERT_PASSWORD required}" -T /usr/bin/codesign >/dev/null
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KC_PASS" "$KC" >/dev/null
  # codesign 的证书链与私钥访问走用户搜索列表，临时钥匙串必须在其中。
  while IFS= read -r line; do
    line=${line//\"/}
    line=${line#"${line%%[![:space:]]*}"}
    [ -n "$line" ] && ORIG_KCS+=("$line")
  done < <(security list-keychains -d user)
  security list-keychains -d user -s "$KC" ${ORIG_KCS[@]+"${ORIG_KCS[@]}"}
  IDENTITY=$(security find-identity -v -p codesigning "$KC" | sed -nE 's/.*"(Developer ID Application: [^"]+)".*/\1/p' | head -1)
  [ -n "$IDENTITY" ] || { echo "no Developer ID Application identity in p12" >&2; exit 1; }
  SIGN_ARGS=(--keychain "$KC" --timestamp --options runtime)
elif [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
  IDENTITY=$MACOS_SIGN_IDENTITY
  SIGN_ARGS=(--timestamp --options runtime)
else
  IDENTITY=-
  echo "!! no signing identity: ad-hoc signing (not distributable)"
fi
# Apple 在线服务（codesign --timestamp 的时间戳服务、notarytool、stapler 的 CloudKit 票据）在 GitHub
# macOS runner 上偶发断连（NSURLErrorDomain -1005 / -1009、"A timestamp was expected but was not
# found"）：这些步骤都幂等，按 15s 递增退避，最多 5 次。
retry() {
  local attempt=1
  until "$@"; do
    if [ "$attempt" -ge 5 ]; then
      echo "$1 failed after $attempt attempts" >&2
      return 1
    fi
    echo "!! $1 failed (attempt $attempt/5), retrying in $((attempt * 15))s" >&2
    sleep $((attempt * 15))
    attempt=$((attempt + 1))
  done
}
# macOS 自带 bash 3.2 在 `set -u` 下展开空数组会报 unbound variable（ad-hoc 签名时 SIGN_ARGS 为空）。
sign() { retry codesign --force ${SIGN_ARGS[@]+"${SIGN_ARGS[@]}"} -s "$IDENTITY" "$@"; }

# 插件桌面界面（gpui-shell，Longbridge quickjs-jit）在界面进程内生成机器码：加固运行时下
# 主程序必须带 allow-jit。后台 helper（agent / daemon / nmh）不执行 JIT，不加。
ENTITLEMENTS="$WORK/desktop.entitlements"
cat > "$ENTITLEMENTS" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.cs.allow-jit</key>
  <true/>
</dict>
</plist>
PLIST

echo "== sign ($IDENTITY)"
sign -i "$HOST_ID.daemon" "$HELPER/Contents/MacOS/fluxdownd"
sign -i "$HOST_ID.nmh" "$HELPER/Contents/MacOS/fluxdown_nmh"
sign "$HELPER/Contents/MacOS/fluxdown-agent"
sign "$HELPER"
sign --entitlements "$ENTITLEMENTS" "$APP/Contents/MacOS/fluxdown-desktop"
sign --entitlements "$ENTITLEMENTS" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

# ── DMG ──
PREFIX=${ARTIFACT_PREFIX:-"FluxDown-GPUI-$VERSION-macos-$SUFFIX"}
DMG="$OUT_DIR/$PREFIX.dmg"
rm -f "$DMG"
ln -s /Applications "$WORK/dmg_src/Applications"
hdiutil create -quiet -volname "FluxDown $VERSION" -srcfolder "$WORK/dmg_src" -ov -format UDZO "$DMG"
[ "$IDENTITY" != - ] && retry codesign --force --timestamp ${KC:+--keychain "$KC"} -s "$IDENTITY" "$DMG"

# ── 公证 ──
stapler_retry() { retry xcrun stapler "$@"; }
if [ -n "${APPLE_API_KEY_PATH:-}" ]; then
  [ "$IDENTITY" != - ] || { echo "notarization requires Developer ID signing" >&2; exit 1; }
  echo "== notarize"
  NOTARY=(--key "$APPLE_API_KEY_PATH" --key-id "${APPLE_API_KEY_ID:?}" --issuer "${APPLE_API_ISSUER_ID:?}")
  # 提交与等待分开：`submit --wait` 轮询途中断网会丢掉整次公证（v0.5.9）。提交失败可安全重试
  # （新提交）；拿到 id 后断网只续等同一个提交，不重复上传。
  to_file() { local out=$1; shift; "$@" > "$out"; }
  retry to_file "$WORK/notary-submit.json" xcrun notarytool submit "$DMG" "${NOTARY[@]}" --output-format json
  SUB=$(plutil -extract id raw "$WORK/notary-submit.json")
  echo "notarization submission: $SUB"
  # wait 的非零退出可能是断网，也可能是公证被拒：结论一律以下面的 info 状态为准。
  if ! retry xcrun notarytool wait "$SUB" "${NOTARY[@]}" --timeout 2h; then
    echo "!! notarytool wait did not report success for $SUB; checking final status" >&2
  fi
  retry to_file "$WORK/notary.json" xcrun notarytool info "$SUB" "${NOTARY[@]}" --output-format json
  cat "$WORK/notary.json"
  if ! xcrun notarytool log "$SUB" "${NOTARY[@]}" "$WORK/notary-log.json"; then
    echo "!! notarytool log unavailable for $SUB" >&2
  fi
  [ "$(plutil -extract status raw "$WORK/notary.json")" = Accepted ] || { cat "$WORK/notary-log.json" >&2; exit 1; }
  stapler_retry staple "$DMG"
  stapler_retry validate "$DMG"
  spctl -a -t open --context context:primary-signature -vv "$DMG"
  # DMG 的公证票据覆盖其中 .app 的 cdhash：再给 .app 本体 staple，便携 tar.gz 离线也能过 Gatekeeper
  stapler_retry staple "$APP"
  spctl -a -t exec -vv "$APP"
fi

# ── 便携 tar.gz（自动更新的 DMG 缺失回退；解压得 <prefix>/FluxDown.app）──
rm -rf "${WORK:?}/$PREFIX"
mkdir -p "$WORK/$PREFIX"
ditto "$APP" "$WORK/$PREFIX/FluxDown.app"
tar -C "$WORK" -czf "$OUT_DIR/$PREFIX.tar.gz" "$PREFIX"

echo "DONE $DMG"
