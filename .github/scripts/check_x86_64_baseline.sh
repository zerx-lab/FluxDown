#!/usr/bin/env bash
# 在 qemu64（x86-64 基线，无 SSE4.2 / AVX）CPU 模型下启动发布二进制，拦截非法指令回归。
# 背景：ubuntu-26.04 runner 镜像启用 amd64v3 变体，gcc 默认 AVX2，mimalloc 等 C 依赖
# 编进无运行时分派的 AVX2 代码，0.6.4 x64 服务端 / CLI 在 J4125 等 NAS CPU 上启动即 SIGILL。
#
# 用法：check_x86_64_baseline.sh <binary> [args...]
#   进程在 6 秒内存活（timeout 124）或正常退出（0）即通过；其余退出码（SIGILL=132）失败。
set -uo pipefail

bin=$1
shift
data_dir=$(mktemp -d)
FLUXDOWN_DATA_DIR="$data_dir" FLUXDOWN_BIND=127.0.0.1:0 \
  timeout 6 qemu-x86_64 -cpu qemu64 "$bin" "$@" >"$data_dir/out.log" 2>&1
status=$?
if [ "$status" -ne 0 ] && [ "$status" -ne 124 ]; then
  echo "::error::$bin exited with $status under qemu64 (x86-64 baseline); likely compiled with instructions beyond x86-64"
  cat "$data_dir/out.log"
  rm -rf "$data_dir"
  exit 1
fi
rm -rf "$data_dir"
echo "$bin runs on x86-64 baseline (status $status)"
