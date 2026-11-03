// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

/** @type {import('tailwindcss').Config} */
// 色值与 DESIGN.md token 表一一对应（深色优先单主题，无亮色切换）
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        bg: {
          base: "#0E1116",
          elevated: "#161B22",
          overlay: "#1C2330",
          inset: "#0A0D11",
          hover: "#20293A",
        },
        text: {
          primary: "#E6E9EF",
          secondary: "#B8C0CE",
          muted: "#8A93A5",
          disabled: "#555E6E",
        },
        primary: {
          solid: "#3B82F6",
          hover: "#2F6FE0",
          subtle: "#1B2A44",
          border: "#2E4A73",
        },
        success: {
          solid: "#22C55E",
          subtle: "#12291C",
          text: "#7EE2A8",
        },
        danger: {
          solid: "#EF4444",
          subtle: "#2C1414",
          text: "#F09A9A",
        },
        warning: {
          solid: "#F59E0B",
          subtle: "#2B2110",
          text: "#F2C879",
        },
      },
      fontFamily: {
        mono: ["JetBrains Mono", "SFMono-Regular", "Menlo", "monospace"],
      },
    },
  },
  plugins: [],
};
