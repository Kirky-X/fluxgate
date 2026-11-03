// fluxgate admin API 封装：单文件 fetch 客户端。key 只存内存 state
// （src/state.ts），任何响应/请求体不得回传或记录 key 原文。

export class ApiError extends Error {
  readonly status: number;
  readonly errorType: string;

  constructor(status: number, message: string, errorType: string) {
    super(message);
    this.status = status;
    this.errorType = errorType;
  }
}

let bearer: string | null = null;

export function setApiKey(key: string | null) {
  bearer = key;
}

interface FluxgateErrorShape {
  error?: { message?: string; type?: string };
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json" };
  if (bearer !== null) headers.Authorization = `Bearer ${bearer}`;
  if (init?.body !== undefined) headers["Content-Type"] = "application/json";

  const response = await fetch(path, { ...init, headers });
  const text = await response.text();

  if (!response.ok) {
    let message = response.statusText;
    let errorType = "http_error";
    if (text) {
      try {
        const parsed = JSON.parse(text) as FluxgateErrorShape;
        if (parsed.error?.message) {
          message = parsed.error.message;
          if (parsed.error.type) errorType = parsed.error.type;
        }
      } catch {
        // 非 JSON 错误体：保留 statusText
      }
    }
    throw new ApiError(response.status, message, errorType);
  }
  if (!text) return undefined as T;
  return JSON.parse(text) as T;
}

function get<T>(path: string): Promise<T> {
  return request<T>(path);
}

function post<T>(path: string, body?: unknown): Promise<T> {
  return request<T>(path, {
    method: "POST",
    body: body === undefined ? undefined : JSON.stringify(body),
  });
}

function put<T>(path: string, body: unknown): Promise<T> {
  return request<T>(path, { method: "PUT", body: JSON.stringify(body) });
}

// ---- 端点形状（对齐 docs/API_REFERENCE.md）----

export interface HealthResponse {
  status: string;
  version: string;
  timestamp: string;
}

export interface VendorMetric {
  vendor: string;
  requests: number;
  successes: number;
  failures: number;
  ewma_latency_ms: number;
  last_error: string | null;
}

export interface MetricsSnapshot {
  total_requests: number;
  successful_requests: number;
  failed_requests: number;
  error_rate: number;
  avg_duration_ms: number;
  vendors: VendorMetric[];
  affinity_hit_rate: number;
}

export const getHealth = () => get<HealthResponse>("/api/v1/health");

export const getMetrics = () => get<MetricsSnapshot>("/admin/metrics");

export interface VendorState {
  id: string;
  status: string;
  queue_length: number;
  key_pool: { size: number } | null;
}

export interface VendorConfigEntry {
  id: string;
  name: string;
  type_: string;
  api_base: string;
  enabled: boolean;
  priority: number;
  timeout_ms: number;
  models: Array<{
    model_id: string;
    abstract_id: string | null;
    weight: number;
    enabled: boolean;
  }>;
  [key: string]: unknown;
}

export const getVendorStates = () => get<VendorState[]>("/admin/vendors");

export const getVendorConfigs = () => get<VendorConfigEntry[]>("/admin/vendors/config");

// 整替语义：body 必须是完整 VendorConfig
export const updateVendor = (id: string, config: VendorConfigEntry) =>
  put<{ success: boolean; message: string }>(`/admin/vendors/${encodeURIComponent(id)}`, config);

export const resetVendor = (id: string) =>
  post<{ success: boolean; message: string }>(
    `/admin/vendors/${encodeURIComponent(id)}/reset`,
  );

export interface RequestLog {
  id: number;
  request_id: string;
  timestamp: number;
  endpoint: string;
  abstract_model: string | null;
  actual_vendor: string | null;
  actual_model: string | null;
  status_code: number;
  error_message: string | null;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  total_tokens: number | null;
  duration_ms: number | null;
  cost_usd: number | null;
  messages: string | null;
  // V9 上游键归因（NULL=未尝试/订阅凭证/env 回退/历史行）
  upstream_key_fingerprint: string | null;
  [key: string]: unknown;
}

export interface LogQuery {
  vendor?: string;
  model?: string;
  limit?: number;
  end_time?: number;
}

export function getLogs(query: LogQuery): Promise<RequestLog[]> {
  const params = new URLSearchParams();
  if (query.vendor) params.set("vendor", query.vendor);
  if (query.model) params.set("model", query.model);
  if (query.limit !== undefined) params.set("limit", String(query.limit));
  if (query.end_time !== undefined) params.set("end_time", String(query.end_time));
  const qs = params.toString();
  return get<RequestLog[]>(`/admin/logs${qs ? `?${qs}` : ""}`);
}

export interface RouteCandidate {
  vendor: string;
  model: string;
  decision: string;
  reason_code: string;
  detail: string;
  weight?: number;
  base_weight?: number;
  selection?: string;
  factors?: Record<string, number>;
  quota?: { tie_break: boolean; degrade_tier: string | null };
}

export interface RouteExplainResult {
  model: string;
  candidates: RouteCandidate[];
  affinity?: {
    decision: string;
    vendor?: string;
    worth_tokens?: number | null;
    ttl_secs?: number | null;
  };
  [key: string]: unknown;
}

export const explainRoute = (model: string, sessionId?: string) =>
  post<RouteExplainResult>("/admin/route/explain", {
    model,
    ...(sessionId ? { session_id: sessionId } : {}),
  });

export interface ModelCatalogEntry {
  model_id: string;
  context_window: number;
  input_price: number;
  output_price: number;
  cache_read_price: number;
  cache_creation_price: number;
  currency: string;
  max_output_tokens: number | null;
  supports_tools: boolean;
  supports_thinking: boolean;
}

export const getModelCatalog = () => get<ModelCatalogEntry[]>("/admin/models");
