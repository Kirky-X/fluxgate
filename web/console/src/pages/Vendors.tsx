// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import { useCallback, useEffect, useState, type FormEvent } from "react";
import {
  getVendorConfigs,
  getVendorStates,
  resetVendor,
  updateVendor,
  type VendorConfigEntry,
  type VendorState,
} from "../api";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import { Badge } from "../components/ui/badge";
import { Dialog } from "../components/ui/dialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "../components/ui/table";
import { t } from "../i18n";

function statusVariant(status: string): "success" | "warning" | "destructive" | "outline" {
  switch (status) {
    case "Available":
      return "success";
    case "Busy":
    case "Throttled":
      return "warning";
    case "Unavailable":
      return "destructive";
    default:
      return "outline";
  }
}

export default function VendorsPage() {
  const [configs, setConfigs] = useState<VendorConfigEntry[]>([]);
  const [states, setStates] = useState<Map<string, VendorState>>(new Map());
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<VendorConfigEntry | null>(null);

  const refresh = useCallback(async () => {
    setError(null);
    try {
      const [c, s] = await Promise.all([getVendorConfigs(), getVendorStates()]);
      setConfigs(c);
      setStates(new Map(s.map((v) => [v.id, v])));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const doReset = async (id: string) => {
    if (!window.confirm(t("vendors.confirmReset", { id }))) return;
    try {
      await resetVendor(id);
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const doSave = async (e: FormEvent) => {
    e.preventDefault();
    if (!editing) return;
    try {
      // PUT 为整替语义：提交回填后的完整 config
      await updateVendor(editing.id, editing);
      setEditing(null);
      await refresh();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const patchEditing = (patch: Partial<VendorConfigEntry>) => {
    setEditing((prev) => (prev ? { ...prev, ...patch } : prev));
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <Button size="sm" variant="outline" onClick={() => void refresh()}>
          {t("vendors.refresh")}
        </Button>
        {error && <p className="text-small text-danger-text">{error}</p>}
      </div>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>ID</TableHead>
            <TableHead>{t("vendors.type")}</TableHead>
            <TableHead>{t("vendors.status")}</TableHead>
            <TableHead>{t("vendors.queue")}</TableHead>
            <TableHead>{t("vendors.keyPool")}</TableHead>
            <TableHead>{t("vendors.priority")}</TableHead>
            <TableHead>API Base</TableHead>
            <TableHead>{t("vendors.actions")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {configs.map((c) => {
            const st = states.get(c.id);
            return (
              <TableRow key={c.id}>
                <TableCell className="font-mono">{c.id}</TableCell>
                <TableCell>{c.type_}</TableCell>
                <TableCell>
                  <Badge variant={statusVariant(st?.status ?? "")}>
                    {st?.status ?? t("vendors.unknown")}
                  </Badge>
                </TableCell>
                <TableCell>{st?.queue_length ?? "—"}</TableCell>
                <TableCell>{st?.key_pool ? String(st.key_pool.size) : "—"}</TableCell>
                <TableCell>{c.priority}</TableCell>
                <TableCell className="font-mono text-xs">{c.api_base || "—"}</TableCell>
                <TableCell className="space-x-2">
                  <Button size="sm" variant="outline" onClick={() => setEditing({ ...c })}>
                    {t("vendors.edit")}
                  </Button>
                  <Button size="sm" variant="destructive" onClick={() => void doReset(c.id)}>
                    {t("vendors.reset")}
                  </Button>
                </TableCell>
              </TableRow>
            );
          })}
          {configs.length === 0 && (
            <TableRow>
              <TableCell colSpan={8} className="text-center text-text-muted">
                {t("vendors.empty")}
              </TableCell>
            </TableRow>
          )}
        </TableBody>
      </Table>

      <Dialog
        open={editing !== null}
        onOpenChange={(open) => !open && setEditing(null)}
        title={t("vendors.editTitle", { id: editing?.id ?? "" })}
      >
        {editing && (
          <form onSubmit={(e) => void doSave(e)} className="space-y-3">
            <label className="block space-y-1">
              <span className="text-sm text-text-muted">API Base</span>
              <Input
                value={editing.api_base}
                onChange={(e) => patchEditing({ api_base: e.target.value })}
              />
            </label>
            <label className="block space-y-1">
              <span className="text-sm text-text-muted">{t("vendors.priority")}</span>
              <Input
                type="number"
                value={String(editing.priority)}
                onChange={(e) => patchEditing({ priority: Number(e.target.value) })}
              />
            </label>
            <label className="block space-y-1">
              <span className="text-sm text-text-muted">{t("vendors.timeout")}</span>
              <Input
                type="number"
                value={String(editing.timeout_ms)}
                onChange={(e) => patchEditing({ timeout_ms: Number(e.target.value) })}
              />
            </label>
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={editing.enabled}
                onChange={(e) => patchEditing({ enabled: e.target.checked })}
              />
              <span className="text-sm">{t("vendors.enable")}</span>
            </label>
            <div className="flex justify-end gap-2 pt-2">
              <Button type="button" variant="ghost" onClick={() => setEditing(null)}>
                {t("vendors.cancel")}
              </Button>
              <Button type="submit">{t("vendors.save")}</Button>
            </div>
          </form>
        )}
      </Dialog>
    </div>
  );
}
