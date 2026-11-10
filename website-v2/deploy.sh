#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# deploy.sh — website-v2 服务器端部署脚本
#
# 拉取最新代码 → 重建镜像 → 重启容器 → 清理悬空镜像
#
# 用法（在服务器 website-v2/ 目录执行）:
#   ./deploy.sh
#
# 约定:
#   - 「是否需要重建」按 website-v2/ 子树哈希判断而不是比对 HEAD：
#     HEAD 前进不一定涉及本目录（手动触发、仅改无关路径等）
#   - 子树未变且容器在运行时快速退出，幂等可重复跑
#   - 失败立即中止，不会留下半启动状态
#   - 挂载前缀 SITE_BASE 默认空串 = 根部署（与 docker-compose.yml 的默认值一致）；
#     子路径部署时设 SITE_BASE=/xxx
# ─────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

REPO_DIR="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
SUBDIR="$(git -C "${SCRIPT_DIR}" rev-parse --show-prefix)"
SUBDIR="${SUBDIR%/}"
BRANCH="${DEPLOY_BRANCH:-main}"
export SITE_BASE="${SITE_BASE-}"
# 部署戳放在 .git 内：不进构建上下文、不会被 reset 清掉、不会被误提交
STAMP="$(git -C "${REPO_DIR}" rev-parse --absolute-git-dir)/fluxdown-${SUBDIR}.deployed"

# ── Docker 调用自适应 sudo ────────────────────
if docker info >/dev/null 2>&1; then
  DOCKER="docker"
else
  DOCKER="sudo docker"
fi

echo "=========================================="
echo "  FluxDown Website v2 — Deploy"
echo "  仓库   : ${REPO_DIR}"
echo "  分支   : ${BRANCH}"
echo "  前缀   : ${SITE_BASE:-/}"
echo "  Docker : ${DOCKER}"
echo "  时间   : $(date '+%Y-%m-%d %H:%M:%S')"
echo "------------------------------------------"

# ── 1. 拉取最新代码 ──────────────────────────
echo "[1/4] 拉取最新代码..."
git -C "${REPO_DIR}" fetch origin "${BRANCH}"
git -C "${REPO_DIR}" reset --hard "origin/${BRANCH}"
# bash 边读边执行：reset 若改写了本脚本，继续跑会执行新旧拼接的内容，重新执行一次新版本
if [ -z "${DEPLOY_REEXEC-}" ]; then
  DEPLOY_REEXEC=1 exec bash "${SCRIPT_DIR}/deploy.sh" "$@"
fi
HEAD_SHA="$(git -C "${REPO_DIR}" rev-parse HEAD)"
TREE="$(git -C "${REPO_DIR}" rev-parse "HEAD:${SUBDIR}")"
echo "      HEAD ${HEAD_SHA:0:8}，${SUBDIR}/ 子树 ${TREE:0:8}"

# 子树没变、前缀没变且容器在运行 → 无需任何操作
RUNNING="$(${DOCKER} compose ps -q website 2>/dev/null)"
if [ -n "${RUNNING}" ] && [ -f "${STAMP}" ] && [ "$(cat "${STAMP}")" = "${TREE} ${SITE_BASE}" ]; then
  echo "      ${SUBDIR}/ 无变更且容器在运行，无需部署。"
  exit 0
fi

# ── 2. 重建镜像 ──────────────────────────────
echo "[2/4] 重建 Docker 镜像..."
${DOCKER} compose build website

# ── 3. 重启容器 ──────────────────────────────
echo "[3/4] 启动/重启容器..."
${DOCKER} compose down --remove-orphans >/dev/null 2>&1 || true
# 兜底：清掉残留的同名容器（旧的 docker run / 其他 project 创建的）
${DOCKER} rm -f fluxdown-website-v2 >/dev/null 2>&1 || true
${DOCKER} compose up -d --wait website

# 官网经阿里云 ESA（中国内地）加速：HTML 为 max-age=0、/api/ 绕过缓存，只有带哈希的 /_astro 按源站
# immutable 长缓存，部署后无需清除边缘缓存。
echo "${TREE} ${SITE_BASE}" > "${STAMP}"

# ── 4. 清理悬空镜像 ──────────────────────────
echo "[4/4] 清理悬空镜像..."
${DOCKER} image prune -f >/dev/null 2>&1 || true

# ── IndexNow：只有根部署才提交（子路径预览整站 noindex）──
if [ -z "${SITE_BASE}" ]; then
  echo "[+] 提交 IndexNow..."
  sleep 5
  node "${SCRIPT_DIR}/scripts/indexnow-ping.mjs" || echo "      (IndexNow 提交跳过/失败,不影响部署)"
fi

echo "------------------------------------------"
echo "  ✓ 部署完成: ${HEAD_SHA:0:8}"
echo "=========================================="
