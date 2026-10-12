# zh/error-database-utils: src/error.rs + src/{database,utils} 用户可见消息。
# error-*: NexusFlowError 变体本地化消息（IntoResponse 展示轨道，Display 恒英文规范串）。
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
database-logger-join-failed = 等待异步日志写入器退出失败：{ $error }
database-preagg-flush-failed = token 聚合刷新失败：{ $error }
database-preagg-final-flush-failed = 停机前最终 token 聚合刷新失败：{ $error }
database-cost-usage-query-failed = 成本分账查询失败：{ $error }
# utils-*: metrics.rs 运维告警。
utils-metrics-concentration = 流量集中度告警：路由可能已收敛到单一厂商/模型
database-backend-missing = database.url 为空，path { $path } 的 sqlite 回退需要 db-sqlite feature：以 --features "full,db-sqlite" 重建，或在 [database] url 配置完整连接 URL
# repository.rs 缺列补列（V0.1 旧库迁移）。
database-repo-column-migrated = 迁移：已为表 { $table } 补齐缺失列 { $column }
