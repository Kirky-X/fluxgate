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
    if (!window.confirm(`确认重置厂商 ${id} 的密钥池黑名单与冷却状态？`)) return;
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
          刷新
        </Button>
        {error && <p className="text-small text-danger-text">{error}</p>}
      </div>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>ID</TableHead>
            <TableHead>类型</TableHead>
            <TableHead>状态</TableHead>
            <TableHead>队列</TableHead>
            <TableHead>key 池</TableHead>
            <TableHead>优先级</TableHead>
            <TableHead>API Base</TableHead>
            <TableHead>操作</TableHead>
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
                    {st?.status ?? "未知"}
                  </Badge>
                </TableCell>
                <TableCell>{st?.queue_length ?? "—"}</TableCell>
                <TableCell>{st?.key_pool ? String(st.key_pool.size) : "—"}</TableCell>
                <TableCell>{c.priority}</TableCell>
                <TableCell className="font-mono text-xs">{c.api_base || "—"}</TableCell>
                <TableCell className="space-x-2">
                  <Button size="sm" variant="outline" onClick={() => setEditing({ ...c })}>
                    编辑
                  </Button>
                  <Button size="sm" variant="destructive" onClick={() => void doReset(c.id)}>
                    重置
                  </Button>
                </TableCell>
              </TableRow>
            );
          })}
          {configs.length === 0 && (
            <TableRow>
              <TableCell colSpan={8} className="text-center text-text-muted">
                暂无厂商
              </TableCell>
            </TableRow>
          )}
        </TableBody>
      </Table>

      <Dialog
        open={editing !== null}
        onOpenChange={(open) => !open && setEditing(null)}
        title={`编辑厂商 ${editing?.id ?? ""}`}
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
              <span className="text-sm text-text-muted">优先级</span>
              <Input
                type="number"
                value={String(editing.priority)}
                onChange={(e) => patchEditing({ priority: Number(e.target.value) })}
              />
            </label>
            <label className="block space-y-1">
              <span className="text-sm text-text-muted">超时 (ms)</span>
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
              <span className="text-sm">启用</span>
            </label>
            <div className="flex justify-end gap-2 pt-2">
              <Button type="button" variant="ghost" onClick={() => setEditing(null)}>
                取消
              </Button>
              <Button type="submit">保存（PUT 整替）</Button>
            </div>
          </form>
        )}
      </Dialog>
    </div>
  );
}
