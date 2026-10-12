#===============================================================================
# 智能 LLM 路由代理系统 - 生产级 Dockerfile
# 基于 Docker 官方最佳实践和 2026 年最新优化方案
#===============================================================================

#------------------------------------------------------------------------------
# 阶段 1: 构建阶段 (Build Stage)
# 使用 Rust Alpine 镜像进行编译
#------------------------------------------------------------------------------
# 基础镜像版本策略（按上游支持矩阵核对，日常升级交给 Dependabot docker
# ecosystem，见 .github/dependabot.yml）：
# - rust 取当前受支持的 stable（1.99）。Rust 无 LTS，6 周滚动发布，不存在
#   可选的长期维护分支；原钉的 1.97 已退出官方镜像重建窗口（末次推送
#   2026-07-16，base 冻结在 Alpine 3.24 初版），继续钉旧 minor 拿不到 base
#   安全重建。MSRV 下限 1.97.1 由 Cargo.toml 的 rust-version 声明，CI 按 MSRV
#   工具链校验兼容性，镜像侧用当前 stable 构建即可。
# - alpine 取主仓仍在支持的分支（3.24 支持至 2028-06；原钉的 3.21 支持期只到
#   2026-11-01，到期后仅剩按需的有限安全修复）。
# - 两个 FROM 的 alpine 后缀仍建议同分支同步升级：产物为 musl 静态链接（实测
#   ldd 只留 ld-musl 一条，sqlite 已静态内嵌），因此不存在 sqlite 符号跨版本
#   风险；对齐两侧是为了运行期 loader、apk 包与构建头文件/静态库不跳分支。
#   运行阶段另有 ldd 门禁兜底：产物的每条动态依赖在运行 base 不可解即构建失败。
# - 写全字面量而非 ${ARG} 拼接：dependabot-core 的 docker 解析器只对 FROM 行
#   做正则匹配、不展开 ARG，拼接写法会让基础镜像脱离自动跟踪。
FROM docker.io/library/rust:1.99-alpine3.24 AS build

# 构建参数
ARG APP_NAME=nexusflow
ARG APP_VERSION=0.1.0

# 设置工作目录
WORKDIR /app

# 安装构建依赖：sqlite-dev 提供头文件与 libsqlite3.a（musl 目标下静态内嵌，
# 运行阶段不需配套 libsqlite3.so；glibc 发布产物则动态链接系统库，见
# release.yml）；musl-dev 提供 libc 头，clang/lld 供 ring 等 C 依赖在 musl 目标
# 下编译链接。自研库已全量走 crates.io registry，无 git 依赖，故不装 git。
# 不装 libressl-dev/openssl-dev：Cargo.lock 内无 openssl/openssl-sys/native-tls，
# TLS 全栈走 rustls（db-* 透传 runtime-tokio-rustls），装了只会拖入 community
# 仓依赖（libressl-dev 在 3.24 属 community，仅支持到下一个 stable）并误导
# 读者以为应用需要 OpenSSL。
RUN apk add --no-cache \
    clang \
    lld \
    musl-dev \
    pkgconfig \
    sqlite-dev

# 使用 BuildKit 缓存挂载优化构建速度
# 缓存 Cargo 依赖和编译产物
# --locked：锁定依赖图，防止 crates.io 侧静默漂移；失配即显性失败，依赖版本
# 变更时以 `cargo update` 刷新 Cargo.lock（自研库版本以 Cargo.toml 的
# version 声明为准）。
# Cargo.lock 以只读 bind mount 引入再拷为可写副本：bind 默认只读，避免
# 解析器意外改写锁文件时因文件系统只读而报次生错误（实测）
# locales 必须挂载：src/i18n/catalog.rs 以 include_str! 编译期内嵌
# ../locales/{en,zh}/*.ftl（路径在 src 之外，漏挂编译期即报文件不存在）
# examples 必须挂载：根 Cargo.toml 把 examples 列为 [workspace] 成员，cargo
# 加载 workspace 时要读每个成员的 manifest，漏挂即在解析期报
# “failed to load manifest for workspace member /app/examples”（实测）。
# 这里只需要 manifest 存在，不会编译示例（构建目标是 --bin nexusflow）。
# 后续新增 workspace 成员时必须同步扩展这里的 bind mount 清单。
RUN --mount=type=bind,source=src,target=src \
    --mount=type=bind,source=locales,target=locales \
    --mount=type=bind,source=examples,target=examples \
    --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=/tmp/Cargo.lock.ro \
    --mount=type=cache,target=/app/target/ \
    --mount=type=cache,target=/usr/local/cargo/registry/ \
    --mount=type=cache,target=/root/.cache/ \
    cp /tmp/Cargo.lock.ro ./Cargo.lock && \
    cargo build --locked --release --bin nexusflow && \
    cp ./target/release/nexusflow /bin/server

#------------------------------------------------------------------------------
# 阶段 2: 运行阶段 (Runtime Stage)
# 使用最小化 Alpine 镜像，只包含运行时依赖
#------------------------------------------------------------------------------
FROM docker.io/library/alpine:3.24 AS final

# 标签信息
# base.name 为元数据，必须跟随上面的 runtime FROM 同步修改
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
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.base.name="docker.io/library/alpine:3.24"

# 安装运行时依赖：产物 musl 静态链接，本身**不需要**任何额外库；下列包
# 全为运维与根证书用途：sqlite/sqlite-libs 供容器内用 sqlite3 CLI 直查
# /app/data 的库文件，ca-certificates 供 rustls-native-certs 读 /etc/ssl/certs。
# 不再显式声明 libssl3/libcrypto3：依赖图无 OpenSSL 绑定（纯 rustls）。注：
# 实测 alpine:3.24 基础镜像本身已带这两个包（随 apk-tools/musl-utils 依赖链），
# 移除它们只是不再声称应用需要它，不减体积也不增风险。
RUN apk add --no-cache \
    sqlite \
    sqlite-libs \
    ca-certificates

# 创建非特权用户 (最佳安全实践)
# 注意：busybox adduser 不支持 shadow 长选项（--disabled-password 等在 3.21
# 实测打印 usage 退出 1），统一用短选项：-D 禁密码 / -H 不建家目录 / -G 主组
# / -s 登录 shell（短选项在 3.22~3.24 busybox 上行为一致，换 base 无需改）
ARG UID=10001
ARG GID=10001

RUN addgroup -g ${GID} appgroup && \
    adduser -D -H -u "${UID}" -G appgroup -s /sbin/nologin appuser

# 创建数据目录并设置权限
RUN mkdir -p /app/data && \
    chown -R appuser:appgroup /app

# 从构建阶段复制编译好的二进制文件
COPY --from=build /bin/server /bin/server

# 运行期链接门禁：busybox ldd 会解析产物的每条动态依赖，两类失败都打
# "Error" 开头（实测 alpine:3.24）：
#   - "Error loading shared library ..."  缺少某个库
#   - "Error relocating ... symbol not found"  库在但版本偏旧
# 当前 musl 静态产物只依赖 ld-musl（实测 ldd 仅这一条），因此这道门禁是
# 回验而非 ABI 守卫：若将来改用 glibc/musl-dynamic 基座、或依赖图开始
# 动态拉入 libssl/libsqlite3，而运行阶段没装对应包，这里会直接构建失败，
# 而不是镜像带病发布到容器启动时才炸
RUN set -eux; \
    ldd /bin/server; \
    if ldd /bin/server 2>&1 | grep -q 'Error'; then \
      echo 'FATAL: /bin/server 有未满足的动态依赖（检查运行阶段 apk add 清单）' >&2; \
      exit 1; \
    fi

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
# 镜像层为唯一健康检查定义：docker run 直接继承，compose 不重复声明以免两处
# 参数漂移（原 Dockerfile start-period 5s / compose 10s 不一致）
HEALTHCHECK --interval=30s --timeout=10s --start-period=10s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:8080/api/v1/health || exit 1

# 启动应用
CMD ["/bin/server"]
