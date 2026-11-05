// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import { Fragment, useCallback, useEffect, useState } from "react";
import { getLogs, type RequestLog } from "../api";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import { Badge } from "../components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "../components/ui/table";
import { localeTag, t } from "../i18n";

function statusVariant(code: number): "success" | "warning" | "destructive" | "outline" {
  if (code >= 200 && code < 300) return "success";
  if (code >= 400 && code < 500) return "warning";
  if (code >= 500) return "destructive";
  return "outline";
}

function fmtTime(unix: number): string {
  return new Date(unix * 1000).toLocaleString(localeTag(), { hour12: false });
}

export default function LogsPage() {
  const [logs, setLogs] = useState<RequestLog[]>([]);
  const [vendor, setVendor] = useState("");
  const [model, setModel] = useState("");
  const [limit, setLimit] = useState(100);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<number | null>(null);

  const query = useCallback(
    async (endTime?: number) => {
      setError(null);
      try {
        const rows = await getLogs({
          vendor: vendor || undefined,
          model: model || undefined,
          limit,
          end_time: endTime,
        });
        if (endTime !== undefined) {
          // 游标翻页：按 end_time 回看更早一批，去重后拼接
          setLogs((prev) => {
            const seen = new Set(prev.map((r) => r.id));
            const older = rows.filter((r) => !seen.has(r.id));
            return [...prev, ...older];
          });
        } else {
          setLogs(rows);
        }
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [vendor, model, limit],
  );

  useEffect(() => {
    void query();
    // 初次加载；后续查询由按钮/回车触发，避免输入即刷
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const oldest = logs.length > 0 ? logs[logs.length - 1].timestamp : undefined;

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end gap-2">
        <label className="block space-y-1">
          <span className="text-xs text-text-muted">vendor</span>
          <Input className="w-40" value={vendor} onChange={(e) => setVendor(e.target.value)} />
        </label>
        <label className="block space-y-1">
          <span className="text-xs text-text-muted">model</span>
          <Input className="w-40" value={model} onChange={(e) => setModel(e.target.value)} />
        </label>
        <label className="block space-y-1">
          <span className="text-xs text-text-muted">limit</span>
          <Input
            className="w-24"
            type="number"
            min={1}
            value={String(limit)}
            onChange={(e) => setLimit(Number(e.target.value) || 100)}
          />
        </label>
        <Button size="default" onClick={() => void query()}>
          {t("logs.query")}
        </Button>
        {oldest !== undefined && (
          <Button size="default" variant="outline" onClick={() => void query(oldest)}>
            {t("logs.older")}
          </Button>
        )}
        {error && <p className="text-small text-danger-text">{error}</p>}
      </div>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>{t("logs.time")}</TableHead>
            <TableHead>request_id</TableHead>
            <TableHead>{t("logs.model")}</TableHead>
            <TableHead>vendor</TableHead>
            <TableHead>{t("logs.status")}</TableHead>
            <TableHead>tokens</TableHead>
            <TableHead>{t("logs.duration")}</TableHead>
            <TableHead>{t("logs.cost")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {logs.map((log) => (
            <Fragment key={log.id}>
              <TableRow
                className="cursor-pointer"
                onClick={() => setExpanded(expanded === log.id ? null : log.id)}
              >
                <TableCell className="text-xs">{fmtTime(log.timestamp)}</TableCell>
                <TableCell className="font-mono text-xs">
                  {log.request_id.slice(0, 8)}
                </TableCell>
                <TableCell className="font-mono text-xs">
                  {log.abstract_model ?? "—"} / {log.actual_model ?? "—"}
                </TableCell>
                <TableCell className="font-mono text-xs">{log.actual_vendor ?? "—"}</TableCell>
                <TableCell>
                  <Badge variant={statusVariant(log.status_code)}>{log.status_code}</Badge>
                </TableCell>
                <TableCell>{log.total_tokens ?? "—"}</TableCell>
                <TableCell>{log.duration_ms !== null ? `${log.duration_ms} ms` : "—"}</TableCell>
                <TableCell>{log.cost_usd !== null ? `$${log.cost_usd.toFixed(6)}` : "—"}</TableCell>
              </TableRow>
              {expanded === log.id && (
                <TableRow>
                  <TableCell colSpan={8} className="bg-bg-inset">
                    <pre className="max-h-64 overflow-auto font-mono text-xs text-text-secondary">
                      {log.error_message ?? log.messages ?? t("logs.noDetail")}
                    </pre>
                  </TableCell>
                </TableRow>
              )}
            </Fragment>
          ))}
          {logs.length === 0 && (
            <TableRow>
              <TableCell colSpan={8} className="text-center text-text-muted">
                {t("logs.empty")}
              </TableCell>
            </TableRow>
          )}
        </TableBody>
      </Table>
    </div>
  );
}
