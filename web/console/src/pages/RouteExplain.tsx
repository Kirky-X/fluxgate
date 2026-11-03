// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import { useState } from "react";
import { explainRoute, type RouteExplainResult } from "../api";
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

function decisionVariant(c: { decision: string; reason_code: string }): "success" | "warning" | "destructive" | "outline" {
  if (c.decision === "eligible") return "success";
  if (["throttled", "cooling_down", "queue_over_limit"].includes(c.reason_code))
    return "warning";
  if (["disabled", "quota_depleted", "weight_zero"].includes(c.reason_code))
    return "destructive";
  return "outline";
}

export default function RouteExplainPage() {
  const [model, setModel] = useState("");
  const [sessionId, setSessionId] = useState("");
  const [result, setResult] = useState<RouteExplainResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    if (!model.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      setResult(await explainRoute(model.trim(), sessionId.trim() || undefined));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-4">
      <form onSubmit={(e) => { e.preventDefault(); void submit(); }} className="flex items-end gap-2">
        <label className="block space-y-1">
          <span className="text-xs text-text-muted">model（必填）</span>
          <Input className="w-56" value={model} onChange={(e) => setModel(e.target.value)} />
        </label>
        <label className="block space-y-1">
          <span className="text-xs text-text-muted">session_id（可选）</span>
          <Input
            className="w-56"
            value={sessionId}
            onChange={(e) => setSessionId(e.target.value)}
          />
        </label>
        <Button type="submit" disabled={busy}>
          解释
        </Button>
      </form>
      {error && <p className="text-small text-danger-text">{error}</p>}

      {result && (
        <>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>vendor</TableHead>
                <TableHead>model</TableHead>
                <TableHead>decision</TableHead>
                <TableHead>reason_code</TableHead>
                <TableHead>weight</TableHead>
                <TableHead>quota.tie_break</TableHead>
                <TableHead>quota.degrade_tier</TableHead>
                <TableHead>detail</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {result.candidates.map((c) => (
                <TableRow key={`${c.vendor}/${c.model}`}>
                  <TableCell className="font-mono">{c.vendor}</TableCell>
                  <TableCell className="font-mono">{c.model}</TableCell>
                  <TableCell>
                    <Badge variant={decisionVariant(c)}>{c.decision}</Badge>
                  </TableCell>
                  <TableCell className="font-mono text-xs">{c.reason_code}</TableCell>
                  <TableCell>{c.weight !== undefined ? c.weight.toFixed(3) : "—"}</TableCell>
                  <TableCell>
                    {c.quota ? (c.quota.tie_break ? "true" : "false") : "—"}
                  </TableCell>
                  <TableCell>{c.quota?.degrade_tier ?? "—"}</TableCell>
                  <TableCell className="text-xs text-text-secondary">{c.detail}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          <div>
            <h3 className="mb-2 text-sm text-text-muted">完整 JSON（含 affinity 决策块）</h3>
            <pre className="max-h-[420px] overflow-auto rounded-md bg-bg-inset p-3 font-mono text-xs text-text-secondary">
              {JSON.stringify(result, null, 2)}
            </pre>
          </div>
        </>
      )}
    </div>
  );
}
