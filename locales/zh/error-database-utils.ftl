# zh/error-database-utils: src/error.rs + src/{database,utils} 用户可见消息。
# error-*: FluxgateError 变体本地化消息（IntoResponse 展示轨道，Display 恒英文规范串）。
error-config = 配置错误：{ $message }
error-protocol = 协议转换错误：{ $source }
error-vendor-unavailable = 厂商不可用：{ $vendor }
error-timeout = 请求超时：{ $vendor }
error-rate-limited = 限流：{ $vendor }
error-database = 数据库错误：{ $message }
error-validation = 请求验证失败：{ $field }
error-routing = 路由错误：{ $message }
error-all-vendors-throttled = 全部候选厂商被限流：预计 { $retry_after_secs }s 后可重试
# error-* -source: From 转换时写入 Database 变体 message 字段的前缀。
error-database-source = 数据库错误：{ $error }
error-io-source = IO 错误：{ $error }
# database-*: database.rs 初始化路径与 async_logger/pre_aggregator 运维消息。
database-create-dir = 创建数据库目录失败：{ $error }
database-connect = 连接数据库失败：{ $error }
database-session = 获取数据库会话失败：{ $error }
database-init-schema = 初始化 schema 失败：{ $error }
database-logger-shutdown = 日志写入器已关闭
database-logger-channel-closed = 异步日志写入器通道已关闭
database-logger-flush-failed = 写入请求日志失败（共 { $count } 条）：{ $error }
database-logger-secret-scan-dropped = 日志未通过出站密钥扫描已丢弃（累计 { $total } 条）：{ $error }
database-logger-console-secret-gate-dropped = console 日志未通过出站密钥门禁，已丢弃（不打印）
database-logger-join-failed = 等待异步日志写入器退出失败：{ $error }
database-preagg-flush-failed = token 聚合刷新失败：{ $error }
database-preagg-final-flush-failed = 停机前最终 token 聚合刷新失败：{ $error }
database-cost-usage-query-failed = 成本分账查询失败：{ $error }
# utils-*: metrics.rs 运维告警。
utils-metrics-concentration = 流量集中度告警：路由可能已收敛到单一厂商/模型
database-backend-missing = database.url 为空，path { $path } 的 sqlite 回退需要 db-sqlite feature：以 --features "full,db-sqlite" 重建，或在 [database] url 配置完整连接 URL
# repository.rs 缺列补列（V0.1 旧库迁移）。
database-repo-column-migrated = 迁移：已为表 { $table } 补齐缺失列 { $column }

# storage.rs 契约默认实现：数据面写前沿不承担 admin 查询，显式拒绝。
storage-log-query-unsupported = 此 RequestLogSink 实现为数据面写入前沿，不支持查询（admin 读取经存储后端实现）
storage-usage-stats-unsupported = 此 UsageSink 实现不提供统计查询（admin 读取经存储后端实现）

# vault (key-vault)：保密柜用户可见消息（Display 轨恒英文规范串，此处为展示轨）。
error-vault = 保密柜错误：{ $message }
error-vault-not-found = 保密柜密钥不存在：{ $id }
error-vault-unknown-vendor = 未知厂商：{ $vendor }
# vault 载荷键：经 error-vault = 保密柜错误：{ $message } 插值展示，展示轨本地化。
# file_store.rs
vault-home-not-found = 未找到主目录
vault-read-file-failed = 读取保密柜文件失败：{ $error }
vault-file-magic-mismatch = 保密柜文件魔数不匹配
vault-decode-entries-failed = 解析保密柜条目失败：{ $error }
vault-encode-entries-failed = 序列化保密柜条目失败：{ $error }
vault-seal-file-failed = 密封保密柜文件失败：{ $error }
vault-create-dir-failed = 创建保密柜目录失败：{ $error }
vault-create-tmp-failed = 创建保密柜临时文件失败：{ $error }
vault-write-file-failed = 写入保密柜文件失败：{ $error }
vault-sync-file-failed = 同步保密柜文件失败：{ $error }
vault-replace-file-failed = 替换保密柜文件失败：{ $error }
# crypto.rs
vault-seal-blob-failed = 密封失败：{ $error }
vault-open-blob-failed = 解密失败：{ $error } —— 当前主钥无法解密该条目（存入后 FLUXGATE_MASTER_KEY / 主钥文件已变更；主钥变更后条目不可恢复，请重新录入厂商密钥）
vault-decrypted-not-utf8 = 解密结果不是 UTF-8：{ $error }
# db_store.rs
vault-db-access-failed = 访问保密柜数据库失败：{ $error }
# master_key.rs
vault-master-key-hex-len = { $env } 必须恰好为 { $expected } 个十六进制字符（32 字节），实际 { $actual } 个
vault-master-key-hex-decode-failed = 十六进制解码失败：{ $error }
vault-master-key-create-dir-failed = 创建密钥目录失败：{ $error }
vault-master-key-write-failed = 写入密钥文件失败：{ $error }
vault-master-key-restrict-failed = 收紧密钥文件权限失败：{ $error }
storage-affinity-sticks-unsupported = 该 AffinityStickSink 实现是数据面写入前沿，不支持读取（启动恢复走存储后端实现）
database-affinity-persist-flush-failed = 会话粘性批落库失败（丢弃 { $count } 条）：{ $error }
database-affinity-persist-channel-closed = 会话粘性落盘通道已关闭：{ $error }
database-affinity-sticks-read-failed = 会话粘性读取失败：{ $error }

# ---- masking: 出站掩码（feature masking；错误面 + config 校验面）----
masking-unavailable = 请求被拒绝：出站掩码不可用（fail-closed，绝不原文出站）
masking-blocked = 请求被拦截：{ $kind } 类敏感数据禁止出站

# ---- cost-replay: 离线成本回放 CLI（bin fluxgate-replay）----
replay-cli-usage = 用法：fluxgate-replay --db <sqlite 路径|URL> --from <unix|RFC3339> --to <unix|RFC3339> [--config <路径>] [--scenario affinity|cache_ttl|all] [--json]
replay-cli-missing-arg = 缺少必需参数：{ $arg }
replay-cli-missing-value = 参数 { $arg } 需要一个取值
replay-cli-unknown-arg = 未知参数：{ $arg }
replay-cli-bad-time = 时间取值非法：{ $value }（需 unix 秒或 RFC3339）
replay-cli-bad-scenario = 场景取值非法：{ $value }（需 affinity|cache_ttl|all）
replay-cli-bad-window = --to 必须大于或等于 --from
replay-cli-config-load-failed = 加载 model_catalog 单价配置失败：{ $error }
replay-cli-db-open-failed = 打开回放数据库失败：{ $error }
replay-cli-load-failed = 加载 request_logs 回放数据失败：{ $error }
