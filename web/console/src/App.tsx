// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import {
  NavLink,
  Navigate,
  Route,
  Routes,
  useLocation,
  useNavigate,
} from "react-router-dom";
import { BrowserRouter } from "react-router-dom";
import { logout, AuthGate } from "./state";
import DashboardPage from "./pages/Dashboard";
import VendorsPage from "./pages/Vendors";
import LogsPage from "./pages/Logs";
import RouteExplainPage from "./pages/RouteExplain";
import ModelsPage from "./pages/Models";
import LoginPage from "./pages/Login";

const NAV_ITEMS = [
  { to: "/", label: "仪表盘", end: true },
  { to: "/vendors", label: "Vendors" },
  { to: "/logs", label: "请求日志" },
  { to: "/route-explain", label: "路由解释" },
  { to: "/models", label: "模型目录" },
];

function Shell() {
  const navigate = useNavigate();
  const location = useLocation();
  // DESIGN 承诺：顶栏左侧显示当前页标题（display 档）
  const pageTitle =
    NAV_ITEMS.find((item) =>
      item.end
        ? location.pathname === item.to
        : location.pathname.startsWith(item.to),
    )?.label ?? "仪表盘";
  return (
    <div className="flex min-h-screen flex-col md:flex-row">
      {/* 窄屏（<768px）导航折为顶部横排（可横向滚动），≥md 恢复左侧 200px 固定列 */}
      <nav className="shrink-0 border-b border-text-disabled/30 bg-bg-elevated md:w-[200px] md:border-b-0 md:border-r">
        <div className="flex items-center gap-2 px-2 py-2 md:block md:px-0 md:py-0">
          <span className="px-2 text-base font-semibold">Fluxweir</span>
          <ul className="flex items-center gap-1 overflow-x-auto md:mt-1 md:block md:px-2 md:py-4">
            {NAV_ITEMS.map((item) => (
              <li key={item.to} className="whitespace-nowrap">
                <NavLink
                  to={item.to}
                  end={item.end}
                  className={({ isActive }) =>
                    "flex h-11 items-center rounded-md px-3 text-sm " +
                    (isActive
                      ? "bg-primary-subtle text-primary-solid"
                      : "text-text-secondary hover:bg-bg-hover hover:text-text-primary")
                  }
                >
                  {item.label}
                </NavLink>
              </li>
            ))}
          </ul>
        </div>
      </nav>
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-12 shrink-0 items-center justify-between border-b border-text-disabled/30 px-6">
          <span className="text-display text-text-primary">{pageTitle}</span>
          <div className="flex items-center gap-3">
            {/* DESIGN 承诺：右侧登录态（「已连接」点 + 登出按钮） */}
            <span className="flex items-center gap-1.5 text-small text-text-muted">
              <span
                className="h-2 w-2 rounded-full bg-success-solid"
                aria-hidden="true"
              />
              已连接
            </span>
            <button
              className="text-sm text-text-secondary hover:text-text-primary"
              onClick={() => {
                logout();
                navigate("/login");
              }}
            >
              登出
            </button>
          </div>
        </header>
        <main className="flex-1 overflow-auto p-6">
          <Routes>
            <Route path="/" element={<DashboardPage />} />
            <Route path="/vendors" element={<VendorsPage />} />
            <Route path="/logs" element={<LogsPage />} />
            <Route path="/route-explain" element={<RouteExplainPage />} />
            <Route path="/models" element={<ModelsPage />} />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Routes>
        </main>
      </div>
    </div>
  );
}

// 路由器选择：BrowserRouter——服务端以 /console/* 前缀挂 SPA fallback，
// 路径语义与 axum 挂载前缀对齐；HashRouter 会把状态挤进 # 片段、
// URL 不再反映服务端真实前缀。
export default function App() {
  return (
    <BrowserRouter basename="/console">
      <AuthGate fallback={<LoginPage />}>
        <Shell />
      </AuthGate>
    </BrowserRouter>
  );
}
