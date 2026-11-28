#!/bin/sh
# FluxDown QPKG 生命周期脚本（QTS App Center start/stop/restart 入口）。
CONF=/etc/config/qpkg.conf
QPKG_NAME="FluxDown"
QPKG_ROOT=$(/sbin/getcfg "$QPKG_NAME" Install_Path -f "$CONF")
PIDFILE="$QPKG_ROOT/fluxdown.pid"

is_running() {
	[ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null
}

start() {
	ENABLED=$(/sbin/getcfg "$QPKG_NAME" Enable -u -d FALSE -f "$CONF")
	if [ "$ENABLED" != "TRUE" ]; then
		echo "$QPKG_NAME is disabled."
		exit 1
	fi
	if is_running; then
		echo "$QPKG_NAME is already running."
		exit 0
	fi
	mkdir -p "$QPKG_ROOT/data"
	# 首次设置引导码的用户可见副本：放进 Public 共享文件夹，File Station 里可直接打开；设置成功后 agent 删除。
	setup_code_copy=""
	[ -d /share/Public ] && setup_code_copy="/share/Public/FluxDown-setup-code.txt"
	# FLUXDOWN_INSTALL_SOURCE：二进制归 App Center 管理，应用内更新只提示安装新 qpkg
	# MIMALLOC_PURGE_DELAY=0：释放的内存立即归还系统（daemon 继承）
	FLUXDOWN_DATA_DIR="$QPKG_ROOT/data" \
	FLUXDOWN_BIND="[::]:17800" \
	FLUXDOWN_INSTALL_SOURCE=qnap \
	FLUXDOWN_SETUP_CODE_COPY="$setup_code_copy" \
	MIMALLOC_PURGE_DELAY=0 \
		"$QPKG_ROOT/fluxdown-agent" --server >> "$QPKG_ROOT/data/server.log" 2>&1 &
	echo $! > "$PIDFILE"
}

stop() {
	if [ -f "$PIDFILE" ]; then
		kill "$(cat "$PIDFILE")" 2>/dev/null
		# SIGTERM → agent 优雅退出并关停 fluxdownd；最多等 10s
		i=0
		while is_running && [ $i -lt 10 ]; do
			sleep 1
			i=$((i + 1))
		done
		is_running && kill -9 "$(cat "$PIDFILE")" 2>/dev/null
		rm -f "$PIDFILE"
	fi
}

case "$1" in
	start) start ;;
	stop) stop ;;
	restart)
		stop
		start
		;;
	*)
		echo "Usage: $0 {start|stop|restart}"
		exit 1
		;;
esac
exit 0
