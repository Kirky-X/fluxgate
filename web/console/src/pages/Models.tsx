// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import { useCallback, useEffect, useState } from "react";
import { getModelCatalog, type ModelCatalogEntry } from "../api";
import { Button } from "../components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "../components/ui/table";
import { t } from "../i18n";

export default function ModelsPage() {
  const [entries, setEntries] = useState<ModelCatalogEntry[]>([]);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setError(null);
    try {
      setEntries(await getModelCatalog());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <Button size="sm" variant="outline" onClick={() => void refresh()}>
          {t("models.refresh")}
        </Button>
        {error && <p className="text-small text-danger-text">{error}</p>}
      </div>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>model_id</TableHead>
            <TableHead>{t("models.context")}</TableHead>
            <TableHead>{t("models.inputPrice")}</TableHead>
            <TableHead>{t("models.outputPrice")}</TableHead>
            <TableHead>{t("models.currency")}</TableHead>
            <TableHead>{t("models.maxOutput")}</TableHead>
            <TableHead>{t("models.tools")}</TableHead>
            <TableHead>{t("models.thinking")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {entries.map((m) => (
            <TableRow key={m.model_id}>
              <TableCell className="font-mono">{m.model_id}</TableCell>
              <TableCell>{m.context_window.toLocaleString()}</TableCell>
              <TableCell>{m.input_price}</TableCell>
              <TableCell>{m.output_price}</TableCell>
              <TableCell>{m.currency}</TableCell>
              <TableCell>{m.max_output_tokens?.toLocaleString() ?? "—"}</TableCell>
              <TableCell className={m.supports_tools ? "text-success-text" : "text-text-muted"}>
                {m.supports_tools ? "✓" : "—"}
              </TableCell>
              <TableCell className={m.supports_thinking ? "text-success-text" : "text-text-muted"}>
                {m.supports_thinking ? "✓" : "—"}
              </TableCell>
            </TableRow>
          ))}
          {entries.length === 0 && (
            <TableRow>
              <TableCell colSpan={8} className="text-center text-text-muted">
                {t("models.empty")}
              </TableCell>
            </TableRow>
          )}
        </TableBody>
      </Table>
    </div>
  );
}
