// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 控制台双语文案层（zh 基线 / en 对齐）。
//!
//! 语言按浏览器 `navigator.language` 检测：`zh*` → 中文，其余 → 英文。
//! 约定：JSON 键名/路由参数/api 字段名保持英文原样（不做翻译）；
//! 只有人类可读的展示文案进本表。插值占位用 `{name}` 形态。

export type Lang = "zh" | "en";

const zh = {
  "app.nav.dashboard": "仪表盘",
  "app.nav.logs": "请求日志",
  "app.nav.route": "路由解释",
  "app.nav.models": "模型目录",
  "app.nav.vendors": "厂商",
  "app.connected": "已连接",
  "app.logout": "登出",

  "login.title": "Fluxweir 控制台",
  "login.placeholder": "API Key",
  "login.empty": "请输入 API Key",
  "login.invalid": "Key 无效或权限不足",
  "login.connectFailed": "连接失败：{error}",
  "login.connect": "连接",
  "login.hint": "Key 仅存于本页内存，刷新后需重新输入",

  "dash.healthUnknown": "未连接",
  "dash.refresh": "刷新",
  "dash.totalRequests": "总请求",
  "dash.success": "成功",
  "dash.errorRate": "错误率",
  "dash.avgLatency": "平均延迟",
  "dash.errorAlert": "错误率超过 5%",
  "dash.vendorMetrics": "厂商指标",
  "dash.ewma": "EWMA 延迟",
  "dash.recentErrors": "最近错误",
  "dash.empty": "暂无数据",

  "vendors.confirmReset": "确认重置厂商 {id} 的密钥池黑名单与冷却状态？",
  "vendors.refresh": "刷新",
  "vendors.type": "类型",
  "vendors.status": "状态",
  "vendors.queue": "队列",
  "vendors.keyPool": "key 池",
  "vendors.priority": "优先级",
  "vendors.actions": "操作",
  "vendors.unknown": "未知",
  "vendors.edit": "编辑",
  "vendors.reset": "重置",
  "vendors.empty": "暂无厂商",
  "vendors.editTitle": "编辑厂商 {id}",
  "vendors.timeout": "超时 (ms)",
  "vendors.enable": "启用",
  "vendors.cancel": "取消",
  "vendors.save": "保存（PUT 整替）",

  "logs.query": "查询",
  "logs.older": "更早一批",
  "logs.time": "时间",
  "logs.model": "模型（抽象/实际）",
  "logs.status": "状态",
  "logs.duration": "耗时",
  "logs.cost": "成本",
  "logs.noDetail": "（无详情）",
  "logs.empty": "暂无日志",

  "models.refresh": "刷新",
  "models.context": "上下文窗口",
  "models.inputPrice": "输入价",
  "models.outputPrice": "输出价",
  "models.currency": "币种",
  "models.maxOutput": "最大输出",
  "models.tools": "工具",
  "models.thinking": "思考",
  "models.empty": "目录为空",

  "route.modelLabel": "model（必填）",
  "route.sessionLabel": "session_id（可选）",
  "route.submit": "解释",
  "route.result": "完整 JSON（含 affinity 决策块）",
} as const;

export type StringKey = keyof typeof zh;

const en: Record<StringKey, string> = {
  "app.nav.dashboard": "Dashboard",
  "app.nav.logs": "Request Logs",
  "app.nav.route": "Route Explain",
  "app.nav.models": "Model Catalog",
  "app.nav.vendors": "Vendors",
  "app.connected": "Connected",
  "app.logout": "Log out",

  "login.title": "Fluxweir Console",
  "login.placeholder": "API Key",
  "login.empty": "Enter an API Key",
  "login.invalid": "Invalid key or insufficient permissions",
  "login.connectFailed": "Connection failed: {error}",
  "login.connect": "Connect",
  "login.hint": "The key stays in this page's memory only; re-enter after a refresh",

  "dash.healthUnknown": "Disconnected",
  "dash.refresh": "Refresh",
  "dash.totalRequests": "Total requests",
  "dash.success": "Success",
  "dash.errorRate": "Error rate",
  "dash.avgLatency": "Avg latency",
  "dash.errorAlert": "Error rate above 5%",
  "dash.vendorMetrics": "Vendor metrics",
  "dash.ewma": "EWMA latency",
  "dash.recentErrors": "Recent errors",
  "dash.empty": "No data",

  "vendors.confirmReset": "Reset the key-pool blacklist and cooldown state for vendor {id}?",
  "vendors.refresh": "Refresh",
  "vendors.type": "Type",
  "vendors.status": "Status",
  "vendors.queue": "Queue",
  "vendors.keyPool": "Key pool",
  "vendors.priority": "Priority",
  "vendors.actions": "Actions",
  "vendors.unknown": "unknown",
  "vendors.edit": "Edit",
  "vendors.reset": "Reset",
  "vendors.empty": "No vendors",
  "vendors.editTitle": "Edit vendor {id}",
  "vendors.timeout": "Timeout (ms)",
  "vendors.enable": "Enabled",
  "vendors.cancel": "Cancel",
  "vendors.save": "Save (full PUT)",

  "logs.query": "Query",
  "logs.older": "Older batch",
  "logs.time": "Time",
  "logs.model": "Model (abstract/actual)",
  "logs.status": "Status",
  "logs.duration": "Latency",
  "logs.cost": "Cost",
  "logs.noDetail": "(no detail)",
  "logs.empty": "No logs",

  "models.refresh": "Refresh",
  "models.context": "Context window",
  "models.inputPrice": "Input price",
  "models.outputPrice": "Output price",
  "models.currency": "Currency",
  "models.maxOutput": "Max output",
  "models.tools": "Tools",
  "models.thinking": "Thinking",
  "models.empty": "Catalog is empty",

  "route.modelLabel": "model (required)",
  "route.sessionLabel": "session_id (optional)",
  "route.submit": "Explain",
  "route.result": "Full JSON (with affinity decision block)",
};

function detectLang(): Lang {
  const nav =
    typeof navigator !== "undefined"
      ? navigator.language.toLowerCase()
      : "zh";
  return nav.startsWith("zh") ? "zh" : "en";
}

const current: Lang = detectLang();

// 日期/数字格式化用的 BCP-47 标签（与界面语言同源，避免时间列固定 zh-CN）
export function localeTag(): string {
  return current === "en" ? "en-US" : "zh-CN";
}

export function t(
  key: StringKey,
  vars?: Record<string, string | number>,
): string {
  let text: string = (current === "en" ? en : zh)[key] ?? zh[key] ?? key;
  if (vars) {
    for (const [name, value] of Object.entries(vars)) {
      text = text.split(`{${name}}`).join(String(value));
    }
  }
  return text;
}
