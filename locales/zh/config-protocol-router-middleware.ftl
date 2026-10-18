# zh/config-protocol-router-middleware: src/{config,protocol,router,middleware} 用户可见消息
# 通道：Config/Validation/Protocol 错误进 HTTP 响应与 SSE error 帧；其余为 tracing 运维日志。

# ---- config/loader.rs ----
config-file-not-found = 配置文件不存在: { $path }
config-load-failed = 加载配置文件失败: { $error }
config-parse-failed = 解析配置文件失败: { $error }
config-duplicate-vendor-id = 厂商 ID 重复: { $vendor }
config-duplicate-model-id = 厂商 { $vendor } 内的模型 ID 重复: { $model }
config-vendor-id-empty = 厂商 ID 为空（仅空白字符）——路由查找键不得为空白
config-model-id-empty = 厂商 { $vendor } 内的模型 ID 为空（仅空白字符）
config-abstract-id-empty = 厂商 { $vendor } 模型 { $model } 的 abstract_id 为空（仅空白字符）
config-embedding-group-model-mismatch = [vendors] 兼容组 "{ $group }" 内 upstream model_id 不一致：厂商 { $vendor_a } 为 "{ $model_a }"，厂商 { $vendor_b } 为 "{ $model_b }"（同组 model_id 必须大小写不敏感一致）
config-embedding-group-dimension-conflict = [vendors] 兼容组 "{ $group }" 内已声明 dimensions 冲突：厂商 { $vendor_a } 模型 "{ $model_a }" 声明 { $dims_a }，厂商 { $vendor_b } 模型 "{ $model_b }" 声明 { $dims_b }
config-embedding-group-cross-abstract = [vendors] 兼容组 "{ $group }" 跨两个 abstract_id："{ $abstract_a }"（厂商 { $vendor_a }）与 "{ $abstract_b }"（厂商 { $vendor_b }）；一个兼容组只能服务一个抽象嵌入模型
config-embedding-group-reserved-separator = [vendors] 兼容组 "{ $group }" 不得包含保留分隔符 "/::"——会与未声明条目的单例组键 "{vendor}/::{model}" 撞键，静默击穿「永不互换」安全默认
config-model-type-unknown = [vendors] 厂商 { $vendor } 模型 { $model } 的 model_type 非法: "{ $model_type }"（合法值: chat / embedding / rerank）
config-rerank-api-base-required = [vendors] 厂商 { $vendor } 的 type_ = "rerank" 必须显式声明 api_base（重排无官方默认端点）
config-rerank-score-normalization-unknown = [routing] rerank_score_normalization 非法: "{ $value }"（合法值: none / sigmoid / minmax）
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
router-embedding-group-exhausted = 嵌入 pin 组 "{ $group }" 对模型 { $model } 已无可用厂商：给厂商声明 compat_group 可启用组内故障迁移，或 POST /admin/embedding-pins/{ $model }/reset 按权重重新粘着
router-rerank-no-capable-vendor = 模型 "{ $model }" 无可用的 rerank 候选：请为厂商声明 type_ = "rerank" 且为条目声明 model_type = "rerank"，并确认厂商与模型均已启用

# ---- middleware/auth.rs ----
middleware-missing-api-key = 缺少 API Key
middleware-invalid-api-key = API Key 无效

# ---- router/difficulty (config validation) ----
config-difficulty-weights-all-zero = [routing.difficulty] w_clf 与 w_rule 不可同时为 0
config-difficulty-threshold-order = [routing.difficulty] score_low（{ $low }）必须小于 score_high（{ $high }）
config-difficulty-policy-invalid = [routing.difficulty] mid_band_policy「{ $policy }」非法（可选 strong/cheap/nearest）
config-difficulty-clf-feature-missing = [routing.difficulty] enabled 且 w_clf>0 需要编译期启用 difficulty-classifier feature
config-difficulty-tier-empty = [routing.difficulty.tiers] { $tier } 档抽象模型名为空
config-difficulty-tier-collision = [routing.difficulty] { $kind }「{ $name }」与真实 model_id 撞名（档位/分组只能指向抽象模型名）
config-difficulty-tier-no-candidate = [routing.difficulty.tiers] { $tier } 档「{ $name }」无可用的 enabled 候选（需至少一个 enabled vendor 提供 model_id/abstract_id 匹配项）
config-difficulty-group-collision = [routing.difficulty] 分组名「{ $name }」与真实 model_id 撞名
config-difficulty-tiers-identical = [routing.difficulty] 各档候选集完全相同，定档对厂商选择无效果（仅提示）
config-difficulty-field-invalid = [routing.difficulty] 字段 { $field } 的值 { $value } 非法（权重须非负有限、阈值须 ∈ [0,1]、timeout_ms ∈ [1,60000]、failure_threshold ≥ 1、engine 须为 candle 或 onnx）
config-difficulty-lexicon-entry-invalid = [routing.difficulty.lexicon] { $field } 第 { $index } 项非法（须非空、无首尾空白/控制字符，且不超过 { $max } 字符）
config-difficulty-lexicon-duplicate = [routing.difficulty.lexicon] { $field } 存在重复项「{ $entry }」
config-difficulty-lexicon-too-many = [routing.difficulty.lexicon] { $field } 条目数 { $count } 超过上限 { $max }
config-difficulty-scoring-invalid = [routing.difficulty.scoring] 字段 { $field } 的值 { $value } 非法（权重须为 [0,1] 内有限值；封顶须为正有限值；length_cap ∈ [1,8000]）

# ---- router/difficulty/orchestrator.rs 熔断内分类降级运维日志 ----
# F5/F6 降级留痕：全部为 tracing DEBUG，仅熔断关闭恢复时升 INFO。
difficulty-classifier-recovered = 定档分类器已恢复（熔断器关闭）
difficulty-classifier-failed = 定档分类器调用失败
difficulty-classifier-task-panicked = 定档分类器任务 panic
difficulty-classifier-timeout = 定档分类器超时
# ---- POST /admin/route/difficulty-explain 的 classifier.error 诊断串 ----
# $p 为分类器返回的越界概率原值；$error / $join_error 为 Debug 透传。
difficulty-explain-invalid-probability = 概率越界：{ $p }
difficulty-explain-task-panicked = 任务 panic：{ $join_error }
difficulty-explain-timeout = 超时
difficulty-explain-not-wired = 分类器未接线
difficulty-explain-breaker-open = 熔断器开启
difficulty-explain-not-consulted = 未咨询
# ---- router/difficulty/vecboost_engine.rs 引擎载荷（经 explain 的 error 字段透出）----
difficulty-laya-engine-load-failed = laya 引擎加载失败：{ $error }
difficulty-laya-decide-failed = laya 决策失败：{ $error }
difficulty-laya-request-rejected = 决策请求被拒：{ $error }
config-responses-retention-hours-out-of-range = [responses] retention_hours 越界: { $value }（合法范围: 1-8760 小时）
config-responses-max-response-bytes-out-of-range = [responses] max_response_bytes 越界: { $value }（合法范围: 1024-16777216 字节）
config-responses-max-background-jobs-out-of-range = [responses] max_background_jobs 越界: { $value }（合法范围: 1-256）
config-vendor-api-style-unknown = [vendors] 厂商 { $vendor } 的 api_style 非法: "{ $value }"（合法值: chat / responses）
config-vendor-api-style-requires-openai = [vendors] 厂商 { $vendor } 声明 api_style = "responses" 时 type_ 必须为 "openai"（当前: "{ $type }"）
config-vendor-api-style-chat-path-conflict = [vendors] 厂商 { $vendor } 不能同时声明 api_style = "responses" 与 chat_path（路径语义歧义）
config-vendor-gemini-chat-path-conflict = [vendors] type_ = "gemini" 的厂商 { $vendor } 不能声明 chat_path（路径由 model + action 构造）
config-vendor-anthropic-version-invalid = [vendors] 厂商 { $vendor } 的 anthropic_version 非法: "{ $value }"（应为 YYYY-MM-DD）
router-responses-no-native-vendor = 模型 "{ $model }" 无 Responses 原生候选：请为厂商声明 type_ = "openai" 且 api_style = "responses"，并确认厂商与模型均已启用
