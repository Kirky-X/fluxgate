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
# 非 Critical 违规不拒绝启动，但必须留痕（错误显性化）。
config-non-critical-admin-secret-violation = 管理面密钥非致命规则违规：{ $message }

# ---- config/manager.rs ----
config-snapshot-serialize-failed = 配置快照跳过：旧配置序列化失败
config-snapshot-failed = 配置快照失败（不阻塞更新）: { $error }
config-snapshot-saved = 已留配置快照: { $path }
config-serialize-failed = 配置序列化失败: { $error }
config-path-unknown = 配置路径未知: ConfigManager 非经 ConfigLoader 创建；拒绝落盘更新
config-disk-modified-refusing-write = { $path } 自上次加载或写回后被进程外修改；拒绝覆盖写回——请先热重载或重启以应用该外部编辑（待生效编辑可能落在 vendor 拓扑等仅重启生效的段）
config-tmp-write-failed = 写入配置临时文件 { $path } 失败: { $error }
config-atomic-replace-failed = 原子替换 { $path } 失败: { $error }

# ---- protocol/{anthropic,openai}_converter.rs、sse_frame.rs ----
protocol-unknown-error = 未知错误
protocol-anthropic-error = Anthropic 错误: { $type } - { $message }
protocol-openai-error = OpenAI 错误: { $type } - { $message }
protocol-passthrough-stream-interrupted = 上游透传流中断
protocol-stream-error-frame = 上游流中断：已输出部分内容后终止流。
protocol-tool-buffer-malformed = 丢弃畸形工具缓冲：id 与 name 均空
# anthropic 自动缓存断点注入：注入计数与未知锚点跳过（调试留痕）。
protocol-anthropic-cache-breakpoints-injected = 已注入 anthropic 缓存断点
protocol-anthropic-unknown-cache-anchor = 未知缓存断点锚点，已跳过

# ---- router/router.rs ----
router-model-not-found = 模型 { $model } 未找到
router-global-model-disabled = 模型 { $model } 未找到：全局模型路由已关闭（[global_model].enabled=false）
router-vendor-unavailable = 指定的厂商 { $vendor } 不可用或未配置
router-fail-open-engaged = 全部候选冷却中；已启用 fail-open 回落
router-global-fail-open-engaged = 全局候选全部冷却中；已启用 fail-open 回落
router-explain-not-in-catalog = vendor { $vendor } 不提供模型 '{ $model }'
router-explain-throttled = 限流窗口耗尽（window 无预算）
router-explain-cooling-down = vendor 状态 { $status }（非 Available）
router-explain-key-pool-exhausted = key 池耗尽（{ $n }/{ $n } 把 key 全部在黑名单：{ $prints }）
router-explain-affinity-bound-elsewhere = 会话亲和绑定于 '{ $bound }'（hard 模式排他）
router-explain-queue-over-limit = 队列深度 { $queue_len } ≥ 饱和点 { $saturation }
router-explain-weight-zero = 合成权重为 0（生产轮盘赌全零时仍均匀回退可能选中，仅诊断提示）
router-explain-eligible = 候选合格（selection={ $selection }）

# ---- middleware/auth.rs ----
middleware-missing-api-key = 缺少 API Key
middleware-invalid-api-key = API Key 无效
