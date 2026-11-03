# Fluxweir Console — DESIGN.md

> 轻档产出（预授权降级）：本文件即设计系统单源。模板取向 = 数据仪表盘型（运维控制台语境），深色优先——网关运维场景多为长时驻留、夜间值守与暗机房投屏，深色基底降低视觉疲劳并突出状态色信号。

## Overview（设计立场）

1. **状态即颜色**：厂商健康、请求成败、路由去留都收敛到语义色 token，禁止用装饰色表达状态——`color.success`/`color.danger`/`color.warning` 是全站唯一的语义信号源（落地于 Badge/VendorStatus/日志 status 列）。
2. **密度优先于装饰**：运维表格与 JSON 树是主内容，布局用紧凑档 spacing 与等宽字号，留白只为分组服务（落地于 `spacing.*` 与 `font-size.mono`）。
3. **一个壳、五个工作台**：侧边导航 + 顶栏登录态是唯一全局壳；五页各自是独立数据面板，页间不共享内容区布局（落地于五页布局段的统一壳/差异区切分）。

## Tokens

### color.bg（背景层，深色优先）

a11y：bg.base 与 bg.elevated 之上叠加 color.text.primary（#E6E9EF）对比度 ≥ 12:1，text.muted（#8A93A5）≥ 4.6:1——正文与辅助文本均满足 WCAG AA 4.5:1。

| token | 值 | 用途 |
|---|---|---|
| color.bg.base | `#0E1116` | 页面基底（最深） |
| color.bg.elevated | `#161B22` | 卡片/表格面板 |
| color.bg.overlay | `#1C2330` | 弹层/下拉遮罩面 |
| color.bg.inset | `#0A0D11` | 代码块/JSON 树凹面 |
| color.bg.hover | `#20293A` | 行悬停/选中态 |

### color.text（文本层）

a11y：四档在 color.bg.base / color.bg.elevated 上对比度依次为 12.5 / 8.2 / 4.9 / 3.9:1——muted 仅用于 ≥ 14px 的辅助说明，caption 禁用 muted（保证正文可读下限）。

| token | 值 | 用途 |
|---|---|---|
| color.text.primary | `#E6E9EF` | 正文/表头 |
| color.text.secondary | `#B8C0CE` | 次级字段值 |
| color.text.muted | `#8A93A5` | 辅助说明/时间戳 |
| color.text.disabled | `#555E6E` | 禁用态 |

### color.primary（品牌/操作层）

a11y：primary.solid 底 + 深色文字（#0E1116）对比度 7.8:1；primary.solid 上按钮文字用 bg.base 色，不用白色（白字对比度仅 3.2:1，不达标）。

| token | 值 | 用途 |
|---|---|---|
| color.primary.solid | `#3B82F6` | 主按钮/链接/焦点环 |
| color.primary.hover | `#2F6FE0` | 主操作悬停 |
| color.primary.subtle | `#1B2A44` | 选中行底/标签底 |
| color.primary.border | `#2E4A73` | 选中描边/输入框聚焦描边 |

### color.success（健康/成功语义）

| token | 值 | 用途 |
|---|---|---|
| color.success.solid | `#22C55E` | VendorStatus Available 徽章底 |
| color.success.subtle | `#12291C` | 成功徽章淡底 |
| color.success.text | `#7EE2A8` | 成功文本（深底可读） |

### color.danger（失败/危险语义）

| token | 值 | 用途 |
|---|---|---|
| color.danger.solid | `#EF4444` | VendorStatus Unavailable/Throttled、删除确认按钮 |
| color.danger.subtle | `#2C1414` | 失败徽章淡底 |
| color.danger.text | `#F09A9A` | 失败文本/错误提示 |

### color.warning（中间态语义）

| token | 值 | 用途 |
|---|---|---|
| color.warning.solid | `#F59E0B` | Busy/冷却中徽章 |
| color.warning.subtle | `#2B2110` | 警示淡底 |
| color.warning.text | `#F2C879` | 警示文本 |

### font-size

| token | 值 | 用途 |
|---|---|---|
| font-size.display | `22px / 700` | 页标题 |
| font-size.body | `14px / 400` | 表格正文/表单 |
| font-size.small | `12px / 400` | 徽章/辅助说明 |
| font-size.mono | `13px / 400, JetBrains Mono` | 模型 ID、JSON 树、key 指纹 |

### spacing（4px 基准）

| token | 值 | 用途 |
|---|---|---|
| spacing.1 | `4px` | 图标-文字间隙 |
| spacing.2 | `8px` | 表格单元格内距、徽章内距 |
| spacing.3 | `12px` | 卡片内距（紧凑档） |
| spacing.4 | `16px` | 区块间距、侧边导航项内距 |
| spacing.6 | `24px` | 页内容区外距 |

### radius

| token | 值 | 用途 |
|---|---|---|
| radius.sm | `4px` | 徽章/输入框 |
| radius.md | `6px` | 按钮/卡片 |
| radius.lg | `8px` | 弹层面板 |
| radius.full | `9999px` | 状态点 |

### 其他系统级约束

- 触控/点击目标 ≥ 44px：按钮高 40px + 2px 焦点余量、表格行高 ≥ 44px、导航项高 44px。
- 焦点可见：所有可交互元素 `focus-visible` 用 `color.primary.border` 2px 外环。
- 动效 ≤ 200ms（hover/面板出现），尊重 `prefers-reduced-motion`。
- 错误提示出现于表单下方，`color.danger.text` + small 字号。

## 壳与导航（全站共用）

- 左侧固定侧边导航 200px：五个导航项（仪表盘 / Vendors / 请求日志 / 路由解释 / 模型目录），当前项用 `color.primary.subtle` 底 + 左侧 2px `color.primary.solid` 竖条。
- 顶栏 48px：左侧当前页标题（display 档），右侧登录态（「已连接」点 + 登出按钮）。
- 移动端（<768px）不做首版适配目标（运维控制台桌面语境），但导航在窄屏折为顶部横排。

## 五页布局

### 1. 仪表盘（/）

- **数据源**：`GET /api/v1/health`（状态点：ok → success）、`GET /admin/metrics`（摘要卡数据）。
- **布局**：顶部一行四张摘要卡（总请求 total_requests / 成功 successful_requests / 错误率 error_rate ×100% / 平均延迟 avg_duration_ms）；其下厂商指标表（vendors[]：vendor、requests、successes、failures、ewma_latency_ms、last_error）。
- **关键交互**：手动刷新按钮重新拉取两端点；error_rate > 5% 时错误率卡片徽章转 `color.danger`。

### 2. Vendors（/vendors）

- **数据源**：列表 `GET /admin/vendors/config`（配置：id、type_、api_base、enabled、priority、weight）；状态徽章 `GET /admin/vendors`（status：Available/Busy/Throttled/Unavailable、queue_length、key_pool.size）；两表按 vendor id 关联合并展示。
- **布局**：单一合并表格——列为「ID / 类型 / 状态徽章 / 队列 / key 池 / 优先级 / 权重 / 操作」。
- **关键交互**：
  - 每行「编辑」：行内展开或弹层表单（id 只读；可改 api_base、enabled、priority、timeout_ms），提交 `PUT /admin/vendors/{id}`（整替语义：body 必须为完整 VendorConfig，前端基于 config 数据回填后仅改编辑字段）。
  - 每行「重置」：`confirm()` 确认后 `POST /admin/vendors/{id}/reset`，成功后刷新状态。
  - 状态徽章色映射：Available → success、Busy → warning、Throttled → warning、Unavailable → danger。

### 3. 请求日志（/logs）

- **数据源**：`GET /admin/logs?limit&vendor&model`（响应为 RequestLog 数组，倒序）。
- **布局**：顶部过滤条（vendor 输入、model 输入、limit 数字选择、查询按钮）；主表格列「时间(timestamp) / request_id(截断 8 位) / abstract_model / actual_vendor / actual_model / status_code / tokens(total) / duration_ms / cost_usd」。
- **关键交互**：
  - limit 分页语义 = 服务端 limit 截断，前端提供「更早一批」按钮（offset 语义用 timestamp 游标：取本批最旧 timestamp 再查一次并排除重复）。
  - status_code 2xx → success 徽章，4xx → warning，5xx → danger。
  - 行点击展开详情（messages/error_message 原文，`<pre>` 展示，mono 档）。

### 4. 路由解释（/route-explain）

- **数据源**：`POST /admin/route/explain`（body：model 必填、session_id 可选）。
- **布局**：上半部表单（model 输入 + session_id 输入 + 提交按钮）；下半部结果区：候选表（vendor / model / decision 徽章 / reason_code / detail）+ 完整 JSON 树。
- **关键交互**：
  - JSON 树用 `<pre>` + `JSON.stringify(result, null, 2)` 缩进展示（mono 档，`color.bg.inset` 凹面），quota 块的 `tie_break`（布尔）与 `degrade_tier`（tier1/tier2/null）直接可见于原文。
  - decision 徽章：eligible → success，其余 reason_code → warning（throttled/cooling_down）或 danger（disabled/quota_depleted）。
  - 400 validation_error（未知模型）时表单下方 danger 文本展示 `error.message`。

### 5. 模型目录（/models）

- **数据源**：`GET /admin/models`（ModelCatalogEntry 数组）。
- **布局**：只读表格——列「model_id / context_window / input_price / output_price / currency / max_output_tokens / supports_tools / supports_thinking」；能力位用 ✓/— 文本符号（success/文本 muted）。
- **关键交互**：无编辑操作（PUT/DELETE 目录条目不在首版范围）；手动刷新按钮。

## 登录页（无壳态，/login）

- 居中单卡：标题「Fluxweir 控制台」+ API Key 输入框（type=password）+「连接」按钮。
- 提交逻辑：key 存内存 state（不写 localStorage/sessionStorage），跳转 `/`。
- 验证方式：登录后首个请求（GET /admin/metrics）401 时回登录页并提示「key 无效」。

## 组件清单（shadcn 风格手写）

| 组件 | API 形状 | 使用点 |
|---|---|---|
| Button | `variant: default/destructive/outline/ghost`, `size: default/sm`, onClick, disabled, loading | 全站 |
| Card | Card/CardHeader/CardTitle/CardContent 组合 | 仪表盘摘要、登录卡 |
| Input | value/onChange/type/placeholder, 受控 | 登录、过滤条、编辑表单 |
| Table | Table/TableHeader/TableRow/TableHead/TableBody/TableCell 组合 | 全部列表页 |
| Badge | `variant: default/success/warning/destructive/outline` | 状态/decision/status_code |
| Dialog（弹层） | open/onOpenChange + 表单插槽 | Vendors PUT 编辑 |

## 布局图（文字版）

```
┌────────┬──────────────────────────────────┐
│ 侧边导航│ 顶栏：页标题          登录态/登出 │
│ 仪表盘  ├──────────────────────────────────┤
│ Vendors │                                  │
│ 请求日志 │         页内容区                  │
│ 路由解释 │   （五页各自的表格/表单/卡片）      │
│ 模型目录 │                                  │
└────────┴──────────────────────────────────┘
```
