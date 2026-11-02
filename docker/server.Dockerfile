# syntax=docker/dockerfile:1
# FluxDown headless 服务器镜像：fluxdown-agent --server + 同级 fluxdownd（Web SPA 编译期内嵌进 agent）。
# agent 作为 tini 的子进程拉起 fluxdownd。
# 多架构：linux/amd64 + linux/arm64。编译阶段固定跑在构建机原生架构
# （--platform=$BUILDPLATFORM）并按 TARGETARCH 交叉编译，避免 QEMU 模拟下的
# Rust 全量编译（数小时级）；仅最终运行时层按目标架构拉取。
#
# 构建上下文 = 仓库根目录（依赖根 .dockerignore 收窄上下文）：
#   docker build -f docker/server.Dockerfile -t fluxdown-server .
#   docker buildx build --platform linux/amd64,linux/arm64 -f docker/server.Dockerfile .
# 版本号 = 构建参数 FLUXDOWN_APP_VERSION（发布流水线传 tag 版本；不传则回落 crate 版本）。
#
# 运行（首次打开 Web 界面时自行设置访问密钥；也可用 -e FLUXDOWN_TOKEN=... 预置）：
#   docker run -d -p 17800:17800 -v fluxdown-data:/data fluxdown-server

# ── Stage 1: Web 前端（Vite SPA，bun 锁文件；产物架构无关，跑在构建机架构）──
FROM --platform=$BUILDPLATFORM oven/bun:1 AS web
WORKDIR /src/web
COPY web/package.json web/bun.lock ./
RUN bun install --frozen-lockfile
COPY web/ ./
# Vite 别名从仓库根引用共享文案、GPUI 主题解析器、内置文件图标包与国旗（与 GPUI 同一份事实源）
COPY assets/i18n/ /src/assets/i18n/
COPY assets/icon-packs/ /src/assets/icon-packs/
COPY assets/flags/ /src/assets/flags/
COPY website-v2/src/lib/gpui-theme/ /src/website-v2/src/lib/gpui-theme/
RUN bun run build

# ── Stage 2a: Rust 源码与工具链（server 构建与 server-check 共用，COPY 清单只此一份）──
# Linux 侧全 rustls（无 openssl），SQLite 由 sqlx 捆绑编译（cc 交叉工具链），无额外系统依赖。
# CI 构建 `--target server-check`（cargo check，无代码生成）在合入时暴露构建上下文缺失：
# workspace 成员、[patch] 目录、include_bytes! 资源、build.rs 读取的文件。
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS server-src
ARG TARGETARCH
WORKDIR /src
# aarch64 交叉链接器 / cc（libsqlite3-sys 等 build script 用）
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
RUN case "$TARGETARCH" in \
      amd64) echo x86_64-unknown-linux-gnu > /rust-target ;; \
      arm64) echo aarch64-unknown-linux-gnu > /rust-target \
        && rustup target add aarch64-unknown-linux-gnu \
        && apt-get update \
        && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
        && rm -rf /var/lib/apt/lists/* ;; \
      *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac
COPY Cargo.toml Cargo.lock ./
COPY .cargo/ .cargo/
# cargo 解析 workspace 时要读全部成员的 manifest，包括桌面开发工具。
COPY native/ native/
COPY crates/ crates/
COPY scripts/desktop-dev/ scripts/desktop-dev/
# 根 Cargo.toml 的 [patch.crates-io] 指向 third_party/librqbit-dualstack-sockets。
COPY third_party/ third_party/
# notification.rs 在 Linux 上 include_bytes! 应用图标。
COPY assets/logo/ assets/logo/
# Web SPA 在编译期由 fluxdown_agent（feature web-ui）按 FLUXDOWN_EMBED_WEBROOT
# 嵌入二进制（运行时层不再有 web/ 目录，也无需 FLUXDOWN_WEBROOT）。
COPY --from=web /src/web/dist /webroot
ENV FLUXDOWN_EMBED_WEBROOT=/webroot
# 编译期匿名统计 App-Key 注入（空值 = 未注入，统计整体禁用）
ARG FLUXDOWN_ANALYTICS_APP_KEY
ENV FLUXDOWN_ANALYTICS_APP_KEY=$FLUXDOWN_ANALYTICS_APP_KEY
# 产品版本（option_env!，发布流水线传 tag 版本；缺省回落 crate 版本）
ARG FLUXDOWN_APP_VERSION
ENV FLUXDOWN_APP_VERSION=$FLUXDOWN_APP_VERSION
# FluxCloud base URL 编译期烘焙进 agent（option_env!；运行期 FLUXCLOUD_BASE_URL 可覆盖；缺省 127.0.0.1:8720）
ARG FLUXCLOUD_BASE_URL
ENV FLUXCLOUD_BASE_URL=$FLUXCLOUD_BASE_URL
# cache mount（下方两个阶段）：本地重复构建增量编译；registry 缓存避免重复下载。
# id 必须按 TARGETARCH 隔离：buildx 多平台构建里 amd64/arm64 两个 server 阶段并发跑，
# 共享同一份 registry 缓存会让两个 cargo 同时解包同一个 crate（`.cargo-ok` File exists
# → error: failed to unpack package）。sharing=locked 再兜住同架构重试的并发。
# ARG 不跨阶段继承：子阶段须重新声明 TARGETARCH 才能用于 cache id。

# ── Stage 2b: 构建上下文检查（CI 门禁用；与 server 阶段同两条 cargo 命令，只做 check）──
FROM server-src AS server-check
ARG TARGETARCH
RUN --mount=type=cache,id=fluxdown-cargo-registry-$TARGETARCH,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=fluxdown-cargo-check-$TARGETARCH,target=/src/target,sharing=locked \
    cargo check --locked -p fluxdown_agent --features web-ui --target "$(cat /rust-target)" \
    && cargo check --locked -p fluxdown_daemon --target "$(cat /rust-target)"

# ── Stage 2c: Rust 服务器（编译 fluxdown_agent(web-ui) + fluxdown_daemon，按 TARGETARCH 交叉编译）──
FROM server-src AS server
ARG TARGETARCH
RUN --mount=type=cache,id=fluxdown-cargo-registry-$TARGETARCH,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=fluxdown-cargo-target-$TARGETARCH,target=/src/target,sharing=locked \
    cargo build --release --locked -p fluxdown_agent --features web-ui --target "$(cat /rust-target)" \
    && cargo build --release --locked -p fluxdown_daemon --target "$(cat /rust-target)" \
    && cp "target/$(cat /rust-target)/release/fluxdown-agent" "target/$(cat /rust-target)/release/fluxdownd" /usr/local/bin/

# ── Stage 3: 运行时（目标架构 debian-slim + ca-certificates，rustls 读系统根证书）──
# xz-utils：托管 ffmpeg 组件在 Linux 上是 .tar.xz，components/ffmpeg.rs 走系统 `tar -xJf` 解压（#649）
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tini xz-utils \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
# fluxdownd 必须与 fluxdown-agent 同目录（agent 按同级路径拉起 daemon）
COPY --from=server /usr/local/bin/fluxdown-agent /usr/local/bin/fluxdownd /app/
# FLUXDOWN_BIND / FLUXDOWN_DATABASE_URL / FLUXDOWN_DEMO / FLUXDOWN_LANG 等见 native/agent 的 server 模式说明
# FLUXDOWN_INSTALL_SOURCE：容器内替换二进制会随重建丢失，应用内更新改为提示拉取新镜像。
ENV FLUXDOWN_BIND=[::]:17800 \
    FLUXDOWN_DATA_DIR=/data \
    FLUXDOWN_INSTALL_SOURCE=docker
VOLUME /data
EXPOSE 17800
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD curl -fsS "http://127.0.0.1:${FLUXDOWN_BIND##*:}/ping" || exit 1
# tini 作 PID 1：转发 SIGTERM 给 agent（agent 会关停 daemon）并回收子进程
ENTRYPOINT ["/usr/bin/tini", "--", "/app/fluxdown-agent", "--server"]
