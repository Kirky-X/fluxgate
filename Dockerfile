#===============================================================================
# 智能 LLM 路由代理系统 - 生产级 Dockerfile
# 基于 Docker 官方最佳实践和 2026 年最新优化方案
#===============================================================================

#------------------------------------------------------------------------------
# 阶段 1: 构建阶段 (Build Stage)
# 使用 Rust Alpine 镜像进行编译
#------------------------------------------------------------------------------
ARG RUST_VERSION=1.92

FROM docker.io/library/rust:${RUST_VERSION}-alpine AS build

# 重新声明构建参数
ARG APP_NAME=nexusflow
ARG APP_VERSION=0.1.0

# 设置工作目录
WORKDIR /app

# 安装构建依赖 (SQLite 需要 musl-dev, OpenSSL 等)
RUN apk add --no-cache \
    clang \
    lld \
    musl-dev \
    libressl-dev \
    pkgconfig \
    sqlite-dev \
    git

# 使用 BuildKit 缓存挂载优化构建速度
# 缓存 Cargo 依赖和编译产物
RUN --mount=type=bind,source=src,target=src \
    --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=cache,target=/app/target/ \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    --mount=type=cache,target=/usr/local/cargo/registry/ \
    --mount=type=cache,target=/root/.cache/ \
    cargo build --locked --release --bin nexusflow && \
    cp ./target/release/nexusflow /bin/server

#------------------------------------------------------------------------------
# 阶段 2: 运行阶段 (Runtime Stage)
# 使用最小化 Alpine 镜像，只包含运行时依赖
#------------------------------------------------------------------------------
FROM docker.io/library/alpine:3.21 AS final

# 标签信息
ARG APP_NAME=nexusflow
ARG APP_VERSION=0.1.0
ARG BUILD_DATE
ARG VCS_REF

LABEL maintainer="NexusFlow" \
      name="${APP_NAME}" \
      version="${APP_VERSION}" \
      description="智能 LLM 路由代理系统 - 多厂商统一管理、格式转换、智能调度" \
      org.opencontainers.image.source="https://github.com/nexusflow/nexusflow" \
      org.opencontainers.image.version="${APP_VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.created="${BUILD_DATE}"

# 安装运行时依赖
RUN apk add --no-cache \
    libssl3 \
    libcrypto3 \
    sqlite \
    sqlite-libs \
    ca-certificates

# 创建非特权用户 (最佳安全实践)
ARG UID=10001
ARG GID=10001

RUN addgroup -g ${GID} appgroup && \
    adduser \
    --disabled-password \
    --gecos "" \
    --home "/nonexistent" \
    --shell "/sbin/nologin" \
    --no-create-home \
    --uid "${UID}" \
    --gid "${GID}" \
    appuser

# 创建数据目录并设置权限
RUN mkdir -p /app/data && \
    chown -R appuser:appgroup /app

# 从构建阶段复制编译好的二进制文件
COPY --from=build /bin/server /bin/server

# 设置工作目录
WORKDIR /app

# 切换到非特权用户
USER appuser

# 环境变量配置
ENV APP_NAME=${APP_NAME}
ENV APP_VERSION=${APP_VERSION}
ENV RUST_LOG=info
ENV RUST_BACKTRACE=1

# 暴露服务端口
EXPOSE 8080

# 健康检查
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:8080/health || exit 1

# 启动应用
CMD ["/bin/server"]
