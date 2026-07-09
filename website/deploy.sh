#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# deploy.sh — 服务器端自动部署脚本
#
# 拉取最新代码 → 重建镜像 → 滚动重启容器 → 清理悬空镜像
#
# 用法（在服务器 website/ 目录执行，或由 CI 远程调用）:
#   ./deploy.sh
#
# 约定:
#   - 仅当远端有新提交时才重建（无变更时快速退出，幂等可重复跑）
#   - 失败立即中止，不会留下半启动状态
# ─────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

# 仓库根目录（website 的上一级）
REPO_DIR="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
BRANCH="${DEPLOY_BRANCH:-main}"

# ── Docker 调用自适应 sudo ────────────────────
# 当前用户若不在 docker 组（无法免密调用 docker），自动回退到 sudo。
if docker info >/dev/null 2>&1; then
  DOCKER="docker"
else
  DOCKER="sudo docker"
fi

echo "=========================================="
echo "  FluxDown Website — Deploy"
echo "  仓库   : ${REPO_DIR}"
echo "  分支   : ${BRANCH}"
echo "  Docker : ${DOCKER}"
echo "  时间   : $(date '+%Y-%m-%d %H:%M:%S')"
echo "------------------------------------------"

# ── 1. 拉取最新代码 ──────────────────────────
echo "[1/4] 拉取最新代码..."
git -C "${REPO_DIR}" fetch origin "${BRANCH}"

LOCAL_SHA="$(git -C "${REPO_DIR}" rev-parse HEAD)"
REMOTE_SHA="$(git -C "${REPO_DIR}" rev-parse "origin/${BRANCH}")"

CODE_CHANGED=1
if [ "${LOCAL_SHA}" = "${REMOTE_SHA}" ]; then
  echo "      代码已是最新 (${LOCAL_SHA:0:8})。"
  CODE_CHANGED=0
else
  git -C "${REPO_DIR}" reset --hard "origin/${BRANCH}"
  echo "      ✓ 更新到 ${REMOTE_SHA:0:8}"
fi

# 检查容器是否在运行（解耦“代码更新”与“容器存活”）
RUNNING="$(${DOCKER} compose ps -q website 2>/dev/null)"

# 代码没变且容器已在运行 → 无需任何操作
if [ "${CODE_CHANGED}" -eq 0 ] && [ -n "${RUNNING}" ]; then
  echo "      容器已在运行，无需部署。"
  exit 0
fi

# ── 2. 重建镜像 ──────────────────────────────
echo "[2/4] 重建 Docker 镜像..."
${DOCKER} compose build website

# ── 3. 滚动重启 ──────────────────────────────
echo "[3/4] 启动/重启容器..."
# 先停掉本 compose project 自己的容器
${DOCKER} compose down --remove-orphans >/dev/null 2>&1 || true
# 兜底：清掉任何残留的同名容器（可能由旧的 docker run / 其他 project 创建，
# 不归当前 compose project 管，compose 无法复用其名字而报冲突）
${DOCKER} rm -f fluxdown-website >/dev/null 2>&1 || true
${DOCKER} compose up -d website

# ── 4. 清理悬空镜像 ──────────────────────────
echo "[4/4] 清理悬空镜像..."
${DOCKER} image prune -f >/dev/null 2>&1 || true

# ── 5. IndexNow 提交(容器起来后,线上 sitemap / key 文件已可访问)──
# 脚本从线上 sitemap 抓 URL,在宿主机直接跑(无需容器内 scripts/);
# 失败自身容错退 0,不影响部署结果。给站点几秒完成启动再提交。
echo "[5/5] 提交 IndexNow..."
sleep 5
node "${SCRIPT_DIR}/scripts/indexnow-ping.mjs" || echo "      (IndexNow 提交跳过/失败,不影响部署)"

echo "------------------------------------------"
echo "  ✓ 部署完成: ${REMOTE_SHA:0:8}"
echo "=========================================="
