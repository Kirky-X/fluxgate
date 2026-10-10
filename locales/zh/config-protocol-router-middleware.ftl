# zh/config-protocol-router-middleware: src/{config,protocol,router,middleware} 用户可见消息
# 通道：Config/Validation/Protocol 错误进 HTTP 响应与 SSE error 帧；其余为 tracing 运维日志。

# ---- config/loader.rs ----
config-file-not-found = 配置文件不存在: { $path }
config-load-failed = 加载配置文件失败: { $error }
config-parse-failed = 解析配置文件失败: { $error }
config-duplicate-vendor-id = 厂商 ID 重复: { $vendor }
config-duplicate-model-id = 厂商 { $vendor } 内的模型 ID 重复: { $model }
config-api-key-env-missing = 厂商 { $vendor } 的 API Key 环境变量未设置（配置值长度 { $length }）——请核对是否误将 key 原文当作环境变量名
config-api-key-pool-env-missing = 厂商 { $vendor } 的 API Key 池环境变量 #{ $index }（值长度 { $length }）未设置——请核对是否误将 key 原文当作环境变量名
config-duplicate-catalog-model-id = 模型目录 id 重复：{ $model }
config-catalog-context-window-zero = 模型目录条目 { $model } 的 context_window = 0；登记条目必须声明真实窗口（0 会使 TUI auto-compact 静默不装配）
config-catalog-too-many = 模型目录条目数 { $count } 超过上限 { $cap }
config-catalog-model-id-empty = 模型目录条目的 model_id 为空（仅空白字符）
config-catalog-price-invalid = 模型目录条目 { $model } 的 { $field } = { $value } 非法；价格必须为有限非负数
config-circuit-open-timeout-too-small = resilience.circuit_open_timeout_ms ({ $value }ms) 低于下限 100ms：窗口过短会使熔断 Open 态立即转入半开，故障期退化为探针风暴
config-quota-window-too-small = quota.window_secs（{ $value }s）须 ≥1：0 窗口每次访问即翻转，配额永不满
middleware-quota-exceeded = 配额已耗尽，请稍后重试
middleware-quota-body-too-large = 请求体超过配额判限处理上限（4 MiB）
# quota.enabled 开而 [auth] 关/缺段的组合告警：租户键取自客户端自报头（L9）
config-quota-without-auth = [quota].enabled 已开启但 [auth] 关闭或缺段：配额租户键取自未经校验的客户端自报头，计费控制可被伪造头旁路（请以反代/网络层隔离管理面，或开启认证）
# $details 为固定序条目明细：位置 + 规则结论（本仓库形态契约或 confers
# JwtSecretValidator 规则内核），只含长度数字，不含条目原文
config-weak-admin-secret = 管理面密钥校验未通过，拒绝启动（条目必须为小写 SHA256 hex 摘要）: { $details }
config-admin-secret-not-hex-digest = 不是小写 SHA256 hex 摘要（应为 64 个小写十六进制字符，实际 { $length } 字节）

# ---- config/manager.rs ----
config-snapshot-serialize-failed = 配置快照跳过：旧配置序列化失败
config-snapshot-failed = 配置快照失败（不阻塞更新）: { $error }
config-snapshot-saved = 已留配置快照: { $path }
config-serialize-failed = 配置序列化失败: { $error }
config-path-unknown = 配置路径未知: ConfigManager 非经 ConfigLoader 创建；拒绝落盘更新
config-tmp-write-failed = 写入配置临时文件 { $path } 失败: { $error }
config-atomic-replace-failed = 原子替换 { $path } 失败: { $error }

# ---- protocol/{anthropic,openai}_converter.rs、sse_frame.rs ----
protocol-unknown-error = 未知错误
protocol-anthropic-error = Anthropic 错误: { $type } - { $message }
protocol-openai-error = OpenAI 错误: { $type } - { $message }
protocol-passthrough-stream-interrupted = 上游透传流中断
protocol-stream-error-frame = 上游流中断：已输出部分内容后终止流。

# ---- router/router.rs ----
router-model-not-found = 模型 { $model } 未找到
router-vendor-unavailable = 指定的厂商 { $vendor } 不可用或未配置
router-fail-open-engaged = 全部候选冷却中；已启用 fail-open 回落
router-global-fail-open-engaged = 全局候选全部冷却中；已启用 fail-open 回落

# ---- middleware/auth.rs ----
middleware-missing-api-key = 缺少 API Key
middleware-invalid-api-key = API Key 无效
