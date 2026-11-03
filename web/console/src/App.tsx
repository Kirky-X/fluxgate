import { NavLink, Navigate, Route, Routes, useNavigate } from "react-router-dom";
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
  return (
    <div className="flex min-h-screen">
      <nav className="w-[200px] shrink-0 border-r border-text-disabled/30 bg-bg-elevated">
        <div className="px-4 py-4 text-base font-semibold">Fluxgate</div>
        <ul className="space-y-1 px-2">
          {NAV_ITEMS.map((item) => (
            <li key={item.to}>
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
      </nav>
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-12 shrink-0 items-center justify-between border-b border-text-disabled/30 px-6">
          <span className="text-sm text-text-muted">运维控制台</span>
          <button
            className="text-sm text-text-secondary hover:text-text-primary"
            onClick={() => {
              logout();
              navigate("/login");
            }}
          >
            登出
          </button>
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
