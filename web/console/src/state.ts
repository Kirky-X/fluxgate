// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import { createElement, useSyncExternalStore, type ReactNode } from "react";
import { setApiKey } from "./api";

// 登录 key 内存态：模块级单值 + 微订阅。不做任何持久化
// （localStorage/sessionStorage/cookie 均不触碰），刷新页面即丢失。

type Listener = () => void;

let apiKey: string | null = null;
const listeners = new Set<Listener>();

function emit() {
  listeners.forEach((l) => l());
}

export function login(key: string) {
  apiKey = key;
  setApiKey(key);
  emit();
}

export function logout() {
  apiKey = null;
  setApiKey(null);
  emit();
}

function subscribe(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getSnapshot(): string | null {
  return apiKey;
}

// 登录门：未登录渲染 fallback（登录页），已登录渲染子树。
// 纯 .ts 文件用 createElement 而非 JSX（JSX 需 .tsx 后缀）。
export function AuthGate({ fallback, children }: { fallback: ReactNode; children: ReactNode }) {
  const key = useSyncExternalStore(subscribe, getSnapshot, () => null);
  if (key === null) return createElement("span", null, fallback);
  return createElement("span", null, children);
}
