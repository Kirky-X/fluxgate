# 来源核实 — 建议 13（TimeWindow 时段门控跨午夜判定缺陷）

核实日期：2026-10-01 · 核实人：事实核查员 · 结论：**confirmed（两个来源属实、参照实现与配置语义逐字命中；gap 全部源码落点核实，跨午夜缺陷由代码逻辑直接可证）**

## 来源 1：LiteLLM router_utils/access_windows.py:35-75 — ✅ 属实（参照实现逐字命中）

- 命令：`curl api.github.com/repos/BerriAI/litellm/contents/litellm/router_utils/access_windows.py?ref=main`（76 行，解码）
- 跨午夜分支逐字命中（:38-40）：
  ```python
  if window.start < window.end:
      return window.start <= local < window.end
  return local >= window.start or local < window.end
  ```
- 每窗口自带 ZoneInfo 时区（:37）：`local = aware.astimezone(ZoneInfo(window.timezone)).time()` ✅
- team_ids 豁免（:58-59）：`if request_team_id is not None and any(request_team_id in window.team_ids for window in active): return None` ✅
- 引用区间 :35-75 覆盖 is_window_active(:35-40)、_reservation_blocking_window(:49-60)、filter_reserved_deployments(:63-75)——"完整参照实现"定位成立。

## 来源 2：Kong AI Gateway ai-rate-limiting-advanced 的 calendar/IANA 语义 — ✅ 属实（附一处客观限定）

- 命令：WebFetch `https://developer.konghq.com/ai-gateway/policies/ai-rate-limiting-advanced/`
- calendar 窗口存在：`window_type: calendar`，原文 "Aligns the window to a real calendar boundary in a specific time zone, instead of a rolling window_size"。
- IANA 时区支持：`config.policies[].timezone`，原文 "Set `window_type: calendar` on the policy and `timezone` to an IANA time zone, such as `America/New_York`"；reset 头指向下一个日历边界（"For calendar windows, X-AI-RateLimit-Reset and X-AI-RateLimit-Retry-After point to the next calendar boundary"）。
- **限定（如实记录）**：该页 calendar 窗口的 `period` 仅支持 `month`/`week`，未提供按小时自定义起止的日窗口（如 22:00–06:00），也未提及跨午夜。建议引用的是"calendar 按 IANA 时区对齐真实日历边界"的语义（真实存在），且未声称 Kong 支持跨午夜日窗口——引用与页面一致，不构成 refuted；但若以 Kong 为跨午夜日窗口的直接参照，其 calendar 粒度不够，LiteLLM 实现才是完整参照。

## gap 本地代码引用 — ✅ 全部命中（本会话逐一读取）

- src/vendor/rate_limiter.rs:152-157 `check()` TimeWindow 分支：:155 `let now = Local::now().time();`（chrono::Local 硬编码）、:156 `now >= *start && now < *end`——非跨午夜判定精确命中。缺陷逻辑直接可证：start=22:00、end=06:00、now=02:00 时 `02:00 >= 22:00` 为 false → check 返回 false。
- rate_limiter.rs:173-175 `accounts_tokens()` → `matches!(self, Self::Token { .. })`——TimeWindow 变体 accounts_tokens=false（无配额语义），建议 :173-175 精确命中。
- manager.rs:288-296 worker 路径：:289 `if !instance.rate_limiter.check().await` → :292-295 `response_tx.send(Err(NexusFlowError::RateLimited{..}))` ✅。
- manager.rs:707-709 get_status：`if !instance.rate_limiter.check().await { return VendorStatus::Throttled }` ✅ 精确命中。
- manager.rs:1276-1282 get_window_info TimeWindow 分支：:1277 `chrono::Local::now().time()`（同样硬编码 Local）、:1278 `if now < *start || now >= *end`（窗口外判定同样非跨午夜感知）→ 返回 `window_end: None`，window_factor 走中性/惩罚路径 ✅ 精确命中。
- router.rs:314 `if ...get_status(...) != VendorStatus::Available { continue; }` ✅（:273 于建议 1 核实时已确认同款过滤）→ Throttled vendor 被路由剔除。
- 后果链完整成立：跨午夜配置在其应生效时段内 check() 恒 false → worker 拒绝 + get_status Throttled + window_factor 惩罚 → vendor 被剔出路由。

## 旁注出入（如实记录，不影响结论）

- gap 称 "papers-litellm 笔记 §1.9 将其标注为「本轮找到的 NexusFlow 里最实的一个逻辑缺口」"：out/research/ 目录下**无 papers-litellm.md 文件**（有 projects-litellm.md），grep "最实" 全目录零命中；该缺陷的实际首轮记录在 **out/research/module-vendor.md:190**（"TimeWindow 跨午夜不支持：now>=start && now<end 在 start>end 时永假"）。缺陷本身的首轮发现属实，但笔记文件名与引文措辞与实际不符——此为 gap 内部引用瑕疵，不在 sources 核实范围。

## 无法复核项

- 无（就 sources 而言）。两项来源均已打开并逐字核对；本地代码全部落点已读取。未运行运行时复现（缺陷为纯条件表达式逻辑，代码直读即可判定）。
