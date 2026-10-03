#===============================================================================
# 智能 LLM 路由代理系统 - 生产级 Dockerfile
# 基于 Docker 官方最佳实践和 2026 年最新优化方案
#===============================================================================

#------------------------------------------------------------------------------
# 阶段 1: 构建阶段 (Build Stage)
# 使用 Rust Alpine 镜像进行编译
#------------------------------------------------------------------------------
# 工具链跟随兄弟库 main 的 MSRV：rc.6 起 confers/dbnexus 等声明 rustc 1.97.1
# （浮动 main 实测），锁 1.92 会在依赖解析期显性失败
ARG RUST_VERSION=1.97

FROM docker.io/library/rust:${RUST_VERSION}-alpine AS build

# 重新声明构建参数
ARG APP_NAME=nexusflow
ARG APP_VERSION=0.1.0
# 兄弟库引用：Cargo.toml 以 ../<lib> path 依赖集成 7 个兄弟库，独立仓库
# 构建时克隆到 /（即 /app 的上级）满足路径解析。
# 逐仓钉住与本仓 Cargo.lock 同源的提交（即本地兄弟库 HEAD，931 测试验证
# 过的状态）：浮动 main 已实测发生 API 漂移（limiteron circuit 移动
# CircuitCallError、MSRV 升 1.97.1），克隆 main 编不过本仓源码。
# 升级流程：本地更新兄弟库 + 刷新 Cargo.lock → 把新 HEAD 填入下方对应项。
# GitHub 支持按完整 40 位 SHA fetch（allowReachableSHA1InWant），短 SHA 不行。
ARG SIBLING_PINS="confers=154db5f65e863550ff3e496e35db06062d8a3efb dbnexus=66e94916dd5079d59f360961c32fb8e69e97618a inklog=60ab3972de7ab75af874a88a83edb44926c3fc01 limiteron=c1e300452ba809cdba8989a02e6207fce8cbe869 oxcache=36ad374eec86e125d65443ea004be50b242ba04c sdforge=9788496963fa4abc4f175175f22084e34b6d8bf7 trait-kit=17984c9c9016c36c048e17c22ef235686823d577"

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

# 克隆 7 个兄弟库到 /app 的上级目录（Cargo.toml 的 ../<lib> 相对路径
# 以 /app/Cargo.toml 为基准解析到 /<lib>；[patch.crates-io] 同样指向这里）
# 浅克隆默认分支后再按钉住的完整 SHA 二次浅取并检出（depth=1 首克隆不含
# 历史 SHA 对象）
RUN set -eux; \
    for entry in ${SIBLING_PINS}; do \
      repo="${entry%%=*}"; pin="${entry##*=}"; \
      git init -q "/${repo}"; \
      git -C "/${repo}" remote add origin "https://github.com/Kirky-X/${repo}.git"; \
      git -C "/${repo}" fetch -q --depth 1 origin "${pin}"; \
      git -C "/${repo}" checkout -q --detach FETCH_HEAD; \
    done

# 使用 BuildKit 缓存挂载优化构建速度
# 缓存 Cargo 依赖和编译产物
# --locked：兄弟库已钉在与本仓 Cargo.lock 同源的提交上（见 SIBLING_PINS 注），
# 锁定依赖图防止 crates.io 侧静默漂移；失配即显性失败，升级时同步刷新 pins。
# Cargo.lock 以只读 bind mount 引入再拷为可写副本：bind 默认只读，避免
# 解析器意外改写锁文件时因文件系统只读而报次生错误（实测）
# locales 必须挂载：src/i18n/catalog.rs 以 include_str! 编译期内嵌
# ../locales/{en,zh}/*.ftl（路径在 src 之外，漏挂编译期即报文件不存在）
RUN --mount=type=bind,source=src,target=src \
    --mount=type=bind,source=locales,target=locales \
    --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=/tmp/Cargo.lock.ro \
    --mount=type=cache,target=/app/target/ \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    --mount=type=cache,target=/usr/local/cargo/registry/ \
    --mount=type=cache,target=/root/.cache/ \
    cp /tmp/Cargo.lock.ro ./Cargo.lock && \
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
      org.opencontainers.image.source="https://github.com/Kirky-X/NexusFlow" \
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
# 注意：alpine:3.21 的 busybox adduser 不支持 shadow 长选项（实测打印 usage
# 退出 1），必须用短选项：-D 禁密码 / -H 不建家目录 / -G 主组 / -s 登录 shell
ARG UID=10001
ARG GID=10001

RUN addgroup -g ${GID} appgroup && \
    adduser -D -H -u "${UID}" -G appgroup -s /sbin/nologin appuser

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

# 健康检查（实际注册路径带 sdforge 版本前缀：/api/v1/health）
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:8080/api/v1/health || exit 1

# 启动应用
CMD ["/bin/server"]
