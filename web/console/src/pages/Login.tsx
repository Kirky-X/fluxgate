// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router-dom";
import { ApiError, getMetrics, setApiKey } from "../api";
import { login } from "../state";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "../components/ui/card";
import { Input } from "../components/ui/input";
import { t } from "../i18n";

export default function LoginPage() {
  const [key, setKey] = useState("");
  const [error, setError] = useState<string | null>(null);
  const navigate = useNavigate();

  // DESIGN 承诺：登录后首个请求（GET /admin/metrics）401 → 提示 key 无效、
  // 留在登录页，而不是进壳后再报错。
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!key.trim()) {
      setError(t("login.empty"));
      return;
    }
    setError(null);
    setApiKey(key.trim());
    try {
      await getMetrics();
      login(key.trim());
      navigate("/");
    } catch (err) {
      setApiKey(null);
      setError(
        err instanceof ApiError && err.status === 401
          ? t("login.invalid")
          : t("login.connectFailed", {
              error: err instanceof Error ? err.message : String(err),
            }),
      );
    }
  };

  return (
    <div className="flex min-h-screen items-center justify-center">
      <Card className="w-96">
        <CardHeader>
          <CardTitle className="text-lg text-text-primary">{t("login.title")}</CardTitle>
        </CardHeader>
        <CardContent>
          <form onSubmit={submit} className="space-y-3">
            <Input
              type="password"
              placeholder={t("login.placeholder")}
              value={key}
              onChange={(e) => setKey(e.target.value)}
              autoComplete="new-password"
            />
            {error && <p className="text-small text-danger-text">{error}</p>}
            <Button type="submit" className="w-full">
              {t("login.connect")}
            </Button>
            <p className="text-xs text-text-muted">{t("login.hint")}</p>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}
