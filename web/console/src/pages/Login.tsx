// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router-dom";
import { ApiError, getMetrics, setApiKey } from "../api";
import { login } from "../state";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "../components/ui/card";
import { Input } from "../components/ui/input";

export default function LoginPage() {
  const [key, setKey] = useState("");
  const [error, setError] = useState<string | null>(null);
  const navigate = useNavigate();

  // DESIGN 承诺：登录后首个请求（GET /admin/metrics）401 → 提示 key 无效、
  // 留在登录页，而不是进壳后再报错。
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!key.trim()) {
      setError("请输入 API Key");
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
          ? "key 无效或权限不足"
          : `连接失败：${err instanceof Error ? err.message : String(err)}`,
      );
    }
  };

  return (
    <div className="flex min-h-screen items-center justify-center">
      <Card className="w-96">
        <CardHeader>
          <CardTitle className="text-lg text-text-primary">Fluxgate 控制台</CardTitle>
        </CardHeader>
        <CardContent>
          <form onSubmit={submit} className="space-y-3">
            <Input
              type="password"
              placeholder="API Key"
              value={key}
              onChange={(e) => setKey(e.target.value)}
              autoComplete="new-password"
            />
            {error && <p className="text-small text-danger-text">{error}</p>}
            <Button type="submit" className="w-full">
              连接
            </Button>
            <p className="text-xs text-text-muted">
              Key 仅存于本页内存，刷新后需重新输入
            </p>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}
