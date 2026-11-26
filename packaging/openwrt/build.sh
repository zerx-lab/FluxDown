#!/bin/sh
# 手工构建 OpenWrt 安装包（不依赖 OpenWrt SDK），两种格式共用同一份载荷与服务脚本：
#   ipk：OpenWrt 24.10 及更早（opkg）。外层 tar.gz(debian-binary + control.tar.gz + data.tar.gz)，
#        纯 tar 即可打包。
#   apk：OpenWrt 25.12 起（apk-tools v3）。ADB 格式，只能由 `apk mkpkg` 生成，需 apk-tools v3
#        （CI 在 alpine 容器内运行本脚本）；元数据与 /lib/apk/packages/ 下的文件清单、
#        conffiles 对齐 OpenWrt include/package-pack.mk。
#
# 用法：
#   build.sh <ipk|apk> server <version> <bindir> <arch,arch,...> <out_dir>
#     <bindir> 目录内须同时含 fluxdown-agent 与 fluxdownd 两个二进制
#   build.sh <ipk|apk> luci   <version> <out_dir>
#
# server 子命令对同一份载荷按 arch 列表出多个包（opkg / apk 都严格校验 aarch64 子架构，
# 静态 musl 二进制本身通用，只是包元数据里的架构标签不同）。
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

MAINTAINER="FluxDown <https://fluxdown.com>"
URL="https://fluxdown.com"
LICENSE="AGPL-3.0"
SERVER_DESC="Blazing fast multi-protocol download manager (headless server). HTTP/HTTPS/FTP/BitTorrent/HLS downloads with intelligent segmentation, served with a full web UI on port 17800. Statically linked (musl)."
LUCI_DESC="LuCI support for FluxDown download manager. Service configuration page and a shortcut to the FluxDown web interface."

die() {
	echo "$*" >&2
	exit 1
}

# ── 服务脚本（两种格式共用；IPKG_INSTROOT 非空 = 构建固件镜像时离线安装，不碰服务）──

server_postinst() {
	cat <<-'EOF'
	#!/bin/sh
	[ -n "$IPKG_INSTROOT" ] || {
		/etc/init.d/fluxdown enable
		/etc/init.d/fluxdown start
	}
	exit 0
	EOF
}

# apk 升级不跑 pre-deinstall / post-install，只跑 post-upgrade：保留用户的自启选择，
# 已启用则重启以加载新二进制（同 OpenWrt default_postinst 的升级语义）。
server_postupgrade() {
	cat <<-'EOF'
	#!/bin/sh
	[ -n "$IPKG_INSTROOT" ] || {
		/etc/init.d/fluxdown enabled && /etc/init.d/fluxdown restart
	}
	exit 0
	EOF
}

server_prerm() {
	cat <<-'EOF'
	#!/bin/sh
	[ -n "$IPKG_INSTROOT" ] || {
		/etc/init.d/fluxdown stop
		/etc/init.d/fluxdown disable
	}
	exit 0
	EOF
}

luci_postinst() {
	cat <<-'EOF'
	#!/bin/sh
	[ -n "$IPKG_INSTROOT" ] || {
		rm -rf /tmp/luci-indexcache* /tmp/luci-modulecache 2>/dev/null
		/etc/init.d/rpcd reload 2>/dev/null
	}
	exit 0
	EOF
}

# ── 载荷 ──

stage_server_data() {
	# $1=bindir $2=data 目录
	# fluxdown-agent 自带 Web UI（编译期内嵌），载荷里没有 webroot 目录；
	# fluxdownd 必须与 agent 同目录（agent 按同级路径拉起 daemon）。
	for b in fluxdown-agent fluxdownd; do
		[ -f "$1/$b" ] || die "binary not found: $1/$b"
	done
	mkdir -p "$2/usr/bin" "$2/etc/init.d" "$2/etc/config"
	cp "$1/fluxdown-agent" "$1/fluxdownd" "$2/usr/bin/"
	chmod 755 "$2/usr/bin/fluxdown-agent" "$2/usr/bin/fluxdownd"
	cp "$SCRIPT_DIR/files/fluxdown.init" "$2/etc/init.d/fluxdown"
	chmod 755 "$2/etc/init.d/fluxdown"
	cp "$SCRIPT_DIR/files/fluxdown.config" "$2/etc/config/fluxdown"
	chmod 644 "$2/etc/config/fluxdown"
}

stage_luci_data() {
	# $1=data 目录
	mkdir -p "$1"
	cp -r "$SCRIPT_DIR/luci/." "$1/"
	chmod 755 "$1/usr/libexec/fluxdown/luci-token" "$1/usr/libexec/fluxdown/luci-setup-code"
}

# ── ipk ──

# tar 归一化参数：root 属主、无扩展头，确保 opkg 可解
TAR="tar --format=gnu --owner=0 --group=0 --numeric-owner"

make_ipk() {
	# $1=staging 目录（含 control/ 与 data/ 两个子目录） $2=输出 ipk 路径
	echo "2.0" > "$1/debian-binary"
	$TAR -C "$1/control" -czf "$1/control.tar.gz" .
	$TAR -C "$1/data" -czf "$1/data.tar.gz" .
	$TAR -C "$1" -czf "$2" ./debian-binary ./control.tar.gz ./data.tar.gz
	echo "built: $2"
}

ipk_control() {
	# $1=包名 $2=版本 $3=依赖 $4=section $5=架构 $6=data 目录 $7=描述
	size=$(du -sk "$6" | cut -f1)
	cat <<-EOF
	Package: $1
	Version: $2
	Depends: $3
	Section: $4
	Architecture: $5
	Installed-Size: ${size}k
	Maintainer: $MAINTAINER
	License: $LICENSE
	Description: $7
	EOF
}

ipk_server() {
	VERSION=$1 BINDIR=$2 ARCHES=$3 OUTDIR=$4
	data="$WORK/data"
	stage_server_data "$BINDIR" "$data"
	for arch in $(echo "$ARCHES" | tr ',' ' '); do
		staging="$WORK/pkg-$arch"
		mkdir -p "$staging/control"
		ln -s "$data" "$staging/data" 2>/dev/null || cp -r "$data" "$staging/data"
		ipk_control fluxdown-server "$VERSION" libc net "$arch" "$data" "$SERVER_DESC" > "$staging/control/control"
		echo /etc/config/fluxdown > "$staging/control/conffiles"
		server_postinst > "$staging/control/postinst"
		server_prerm > "$staging/control/prerm"
		chmod 755 "$staging/control/postinst" "$staging/control/prerm"
		make_ipk "$staging" "$OUTDIR/fluxdown-server_${VERSION}_${arch}.ipk"
	done
}

ipk_luci() {
	VERSION=$1 OUTDIR=$2
	staging="$WORK/pkg"
	mkdir -p "$staging/control"
	stage_luci_data "$staging/data"
	ipk_control luci-app-fluxdown "$VERSION" "luci-base, fluxdown-server" luci all "$staging/data" "$LUCI_DESC" \
		> "$staging/control/control"
	luci_postinst > "$staging/control/postinst"
	chmod 755 "$staging/control/postinst"
	make_ipk "$staging" "$OUTDIR/luci-app-fluxdown_${VERSION}_all.ipk"
}

# ── apk ──

# 发布版本 → apk 版本：apk 不接受 `-rc.1` 这类 semver 预发布写法，
# 改成 apk 的预发布后缀（`1.2.0-rc.1` → `1.2.0_rc1`，排序仍早于 `1.2.0`）。
apk_version() {
	case "$1" in
		*-*)
			base=${1%%-*}
			pre=$(printf '%s' "${1#*-}" | tr -d .)
			v="${base}_${pre}"
			;;
		*) v=$1 ;;
	esac
	printf '%s\n' "$v" | grep -Eq '^[0-9]+(\.[0-9]+)*(_(alpha|beta|pre|rc)[0-9]*)?$' \
		|| die "version not representable for apk: $1"
	printf '%s\n' "$v"
}

# 写 OpenWrt 约定的 /lib/apk/packages/<包>.{list,conffiles,conffiles_static}：
# .list 供 OpenWrt 工具按包列文件；conffiles 供 sysupgrade 保留与「配置已改」判定。
apk_package_meta() {
	# $1=data 目录 $2=包名 $3=conffiles（空格分隔，可为空）
	meta="$1/lib/apk/packages"
	mkdir -p "$meta"
	(cd "$1" && find . \( -type f -o -type l \) | sed 's|^\.||' | sort) > "$WORK/$2.list"
	mv "$WORK/$2.list" "$meta/$2.list"
	for f in $3; do
		echo "$f" >> "$meta/$2.conffiles"
		echo "$f $(sha256sum "$1$f" | cut -d' ' -f1)" >> "$meta/$2.conffiles_static"
	done
}

apk_mkpkg() {
	# $1=包名 $2=apk 版本 $3=依赖（空格分隔） $4=section $5=架构 $6=data 目录 $7=描述 $8=输出
	# 其余参数原样传给 mkpkg（--script ...）
	name=$1 ver=$2 deps=$3 section=$4 arch=$5 data=$6 desc=$7 out=$8
	shift 8
	SOURCE_DATE_EPOCH=0 $APK mkpkg \
		--info "name:$name" \
		--info "version:$ver" \
		--info "tags:openwrt:section=$section" \
		--info "description:$desc" \
		--info "arch:$arch" \
		--info "license:$LICENSE" \
		--info "origin:$name" \
		--info "url:$URL" \
		--info "maintainer:$MAINTAINER" \
		--info "depends:$deps" \
		"$@" \
		--files "$data" \
		--output "$out"
	echo "built: $out"
}

apk_server() {
	VERSION=$1 BINDIR=$2 ARCHES=$3 OUTDIR=$4
	ver=$(apk_version "$VERSION")
	data="$WORK/data"
	stage_server_data "$BINDIR" "$data"
	apk_package_meta "$data" fluxdown-server /etc/config/fluxdown
	scripts="$WORK/scripts"
	mkdir -p "$scripts"
	server_postinst > "$scripts/post-install"
	server_postupgrade > "$scripts/post-upgrade"
	server_prerm > "$scripts/pre-deinstall"
	for arch in $(echo "$ARCHES" | tr ',' ' '); do
		apk_mkpkg fluxdown-server "$ver" libc net "$arch" "$data" "$SERVER_DESC" \
			"$OUTDIR/fluxdown-server_${VERSION}_${arch}.apk" \
			--script "post-install:$scripts/post-install" \
			--script "post-upgrade:$scripts/post-upgrade" \
			--script "pre-deinstall:$scripts/pre-deinstall"
	done
}

apk_luci() {
	VERSION=$1 OUTDIR=$2
	ver=$(apk_version "$VERSION")
	data="$WORK/data"
	stage_luci_data "$data"
	apk_package_meta "$data" luci-app-fluxdown ""
	scripts="$WORK/scripts"
	mkdir -p "$scripts"
	luci_postinst > "$scripts/post-install"
	apk_mkpkg luci-app-fluxdown "$ver" "luci-base fluxdown-server" luci noarch "$data" "$LUCI_DESC" \
		"$OUTDIR/luci-app-fluxdown_${VERSION}_all.apk" \
		--script "post-install:$scripts/post-install" \
		--script "post-upgrade:$scripts/post-install"
}

# apk mkpkg 按文件真实属主写入包；非 root 构建经 fakeroot 让载荷归 root。
apk_tool() {
	apk --version 2>/dev/null | grep -q '^apk-tools 3\.' \
		|| die "apk-tools v3 (apk mkpkg) not found; run inside an alpine:3.24+ container"
	if [ "$(id -u)" -eq 0 ]; then
		APK=apk
	elif command -v fakeroot >/dev/null 2>&1; then
		APK="fakeroot apk"
	else
		die "building apk as non-root needs fakeroot"
	fi
}

usage() {
	echo "usage: $0 <ipk|apk> server <version> <bindir> <arches> <outdir>" >&2
	echo "       $0 <ipk|apk> luci <version> <outdir>" >&2
	exit 2
}

[ $# -ge 2 ] || usage
format=$1 cmd=$2
shift 2
case "$format" in
	ipk) ;;
	apk) apk_tool ;;
	*) usage ;;
esac
case "$cmd" in
	server) [ $# -eq 4 ] || usage ;;
	luci) [ $# -eq 2 ] || usage ;;
	*) usage ;;
esac

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
case "$cmd" in
	server) mkdir -p "$4" ;;
	luci) mkdir -p "$2" ;;
esac
"${format}_${cmd}" "$@"
