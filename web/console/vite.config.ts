import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// 基准路径必须用 /console/ 绝对前缀：HTML 相对 URL 按文档 URL 解析，
// /console（无尾斜杠）的基路径是 /，相对资产（./assets/...）会越出挂载
// 前缀落到 /assets/*（404 → 真实浏览器白屏）。绝对前缀在有无尾斜杠、
// 任意深度的 SPA fallback 下均正确。
export default defineConfig({
  base: "/console/",
  plugins: [react()],
  build: {
    outDir: "dist",
    sourcemap: false,
  },
});
