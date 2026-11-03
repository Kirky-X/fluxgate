// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import { useCallback, useEffect, useState } from "react";
import { getHealth, getMetrics, type HealthResponse, type MetricsSnapshot } from "../api";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "../components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "../components/ui/table";
import { Badge } from "../components/ui/badge";

function MetricCard({ title, value }: { title: string; value: string }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
      </CardHeader>
      <CardContent>
        <span className="text-2xl font-semibold">{value}</span>
      </CardContent>
    </Card>
  );
}

export default function DashboardPage() {
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [metrics, setMetrics] = useState<MetricsSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setError(null);
    try {
      const [h, m] = await Promise.all([getHealth(), getMetrics()]);
      setHealth(h);
      setMetrics(m);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const errorRate = metrics ? (metrics.error_rate * 100).toFixed(1) + "%" : "—";
  const highError = metrics !== null && metrics.error_rate > 0.05;

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <span
          className={
            "inline-block h-2.5 w-2.5 rounded-full " +
            (health?.status === "ok" ? "bg-success-solid" : "bg-danger-solid")
          }
          aria-label={health?.status ?? "unknown"}
        />
        <span className="text-sm text-text-muted">
          {health ? `v${health.version}` : "未连接"}
        </span>
        <Button size="sm" variant="outline" onClick={() => void refresh()}>
          刷新
        </Button>
      </div>
      {error && <p className="text-small text-danger-text">{error}</p>}
      <div className="grid grid-cols-4 gap-3">
        <MetricCard title="总请求" value={metrics ? String(metrics.total_requests) : "—"} />
        <MetricCard
          title="成功"
          value={metrics ? String(metrics.successful_requests) : "—"}
        />
        <MetricCard title="错误率" value={errorRate} />
        <MetricCard
          title="平均延迟"
          value={metrics ? `${metrics.avg_duration_ms.toFixed(0)} ms` : "—"}
        />
      </div>
      {highError && (
        <Badge variant="destructive">错误率超过 5%</Badge>
      )}
      <Card>
        <CardHeader>
          <CardTitle>厂商指标</CardTitle>
        </CardHeader>
        <CardContent>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>vendor</TableHead>
                <TableHead>requests</TableHead>
                <TableHead>successes</TableHead>
                <TableHead>failures</TableHead>
                <TableHead>EWMA 延迟</TableHead>
                <TableHead>最近错误</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {(metrics?.vendors ?? []).map((v) => (
                <TableRow key={v.vendor}>
                  <TableCell className="font-mono">{v.vendor}</TableCell>
                  <TableCell>{v.requests}</TableCell>
                  <TableCell>{v.successes}</TableCell>
                  <TableCell>{v.failures}</TableCell>
                  <TableCell>{v.ewma_latency_ms.toFixed(0)} ms</TableCell>
                  <TableCell className="text-danger-text">{v.last_error ?? "—"}</TableCell>
                </TableRow>
              ))}
              {metrics?.vendors.length === 0 && (
                <TableRow>
                  <TableCell colSpan={6} className="text-center text-text-muted">
                    暂无数据
                  </TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </CardContent>
      </Card>
    </div>
  );
}
