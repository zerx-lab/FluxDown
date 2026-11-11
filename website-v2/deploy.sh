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
#   - 服务器直连 github.com 常超时：按 DEPLOY_GIT_MIRRORS（空格分隔的 URL 前缀，
#     拼在仓库地址前）依次拉取，全部失败再直连 origin。镜像是第三方反代，
#     设 DEPLOY_EXPECT_SHA（调用方在可信网络上查到的 main 提交）时拉到的提交
#     必须与之相等，否则换下一个来源：git 提交哈希覆盖全部内容，相等即未被篡改
# ─────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

REPO_DIR="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
SUBDIR="$(git -C "${SCRIPT_DIR}" rev-parse --show-prefix)"
SUBDIR="${SUBDIR%/}"
BRANCH="${DEPLOY_BRANCH:-main}"
GIT_MIRRORS="${DEPLOY_GIT_MIRRORS-https://gh-proxy.com/ https://ghfast.top/}"
EXPECT_SHA="${DEPLOY_EXPECT_SHA-}"
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
# bash 边读边执行：reset 若改写了本脚本，继续跑会执行新旧拼接的内容，故 reset 后
# 重新执行一次新版本；重入的那次（DEPLOY_REEXEC=1）已在最新提交上，跳过拉取。
if [ -z "${DEPLOY_REEXEC-}" ]; then
  echo "[1/4] 拉取最新代码..."
  ORIGIN_URL="$(git -C "${REPO_DIR}" remote get-url origin)"
  FETCHED=""
  for SOURCE in ${GIT_MIRRORS} ""; do
    URL="${SOURCE}${ORIGIN_URL}"
    echo "      来源 ${URL}"
    # 20s 内低于 1KB/s 视为卡死，换下一个来源
    if ! git -C "${REPO_DIR}" -c http.lowSpeedLimit=1024 -c http.lowSpeedTime=20 \
      fetch --no-tags "${URL}" "+refs/heads/${BRANCH}:refs/remotes/origin/${BRANCH}"; then
      echo "      拉取失败，换下一个来源"
      continue
    fi
    GOT="$(git -C "${REPO_DIR}" rev-parse "refs/remotes/origin/${BRANCH}")"
    if [ -n "${EXPECT_SHA}" ] && [ "${GOT}" != "${EXPECT_SHA}" ]; then
      echo "      拉到 ${GOT:0:8}，与期望 ${EXPECT_SHA:0:8} 不符，换下一个来源"
      continue
    fi
    FETCHED=1
    break
  done
  if [ -z "${FETCHED}" ]; then
    echo "      所有来源均拉取失败" >&2
    exit 1
  fi
  git -C "${REPO_DIR}" reset --hard "origin/${BRANCH}"
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
