import { ErrorBoundary } from "./components/ErrorBoundary";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isTauri } from "./tauri";
import { Dashboard } from "./pages/Dashboard";
import { Providers } from "./pages/Providers";
import { Models } from "./pages/Models";
import { Sessions } from "./pages/Sessions";
import { Logs } from "./pages/Logs";
import { Settings } from "./pages/Settings";
import { Routing } from "./pages/Routing";
import { Health } from "./pages/Health";
import { Requests } from "./pages/Requests";
import { Analytics } from "./pages/Analytics";
import { Debug } from "./pages/Debug";
import { ToolProfiles } from "./pages/ToolProfiles";
import { ImportExport } from "./pages/ImportExport";
import { Update } from "./pages/Update";
import { Onboarding } from "./pages/Onboarding";
import { GettingStarted } from "./pages/GettingStarted";
import { setApiBase, api } from "./api";
import type { StatusResponse } from "./types";
import { ToastProvider } from "./components/Toast";
import { Switcher } from "./components/Switcher";
import {
  BrandMark,
  IconDashboard,
  IconProviders,
  IconSessions,
  IconLogs,
  IconSettings,
  IconQuit,
  IconSun,
  IconMoon,
  IconRoute,
  IconHeart,
  IconList,
  IconChart,
  IconBug,
  IconWrench,
  IconBox,
  IconDownload,
} from "./components/Icons";

type Page =
  | "dashboard"
  | "providers"
  | "models"
  | "sessions"
  | "routing"
  | "health"
  | "requests"
  | "analytics"
  | "debug"
  | "tool-profiles"
  | "import-export"
  | "update"
  | "settings"
  | "logs"
  | "switcher";

type Theme = "auto" | "dark" | "light";

const NAV: { id: Page; label: string; shortcut: string; Icon: (p: any) => JSX.Element }[] = [
  { id: "dashboard", label: "Dashboard", shortcut: "1", Icon: IconDashboard },
  { id: "providers", label: "Providers", shortcut: "2", Icon: IconProviders },
  { id: "models", label: "Models", shortcut: "3", Icon: IconBox },
  { id: "sessions", label: "Sessions", shortcut: "4", Icon: IconSessions },
  { id: "routing", label: "Routing", shortcut: "5", Icon: IconRoute },
  { id: "health", label: "Health", shortcut: "6", Icon: IconHeart },
  { id: "requests", label: "Requests", shortcut: "7", Icon: IconList },
  { id: "analytics", label: "Analytics", shortcut: "8", Icon: IconChart },
  { id: "debug", label: "Debug", shortcut: "9", Icon: IconBug },
  { id: "tool-profiles", label: "Tool profiles", shortcut: "0", Icon: IconWrench },
  { id: "import-export", label: "Import / Export", shortcut: "", Icon: IconDownload },
  { id: "update", label: "Update", shortcut: "", Icon: IconDownload },
  { id: "logs", label: "Logs", shortcut: "L", Icon: IconLogs },
  { id: "settings", label: "Settings", shortcut: ",", Icon: IconSettings },
];

const ALL_PAGES: Page[] = [...NAV.map((n) => n.id), "switcher"];
const SHORTCUT_PAGES: Page[] = ALL_PAGES.filter((p) =>
  ["dashboard", "providers", "models", "sessions", "routing", "health", "requests", "analytics", "debug", "tool-profiles", "settings", "logs"].includes(p),
);

const THEME_KEY = "autorouter:theme";
const PAGE_KEY = "autorouter:page";

function readStoredTheme(): Theme {
  try {
    const v = localStorage.getItem(THEME_KEY);
    if (v === "dark" || v === "light" || v === "auto") return v;
  } catch {
    /* no storage, fall through */
  }
  return "auto";
}

function readInitialPage(): Page {
  try {
    const url = new URL(window.location.href);
    const p = url.searchParams.get("page");
    if (p && ALL_PAGES.includes(p as Page)) {
      return p as Page;
    }
    const stored = localStorage.getItem(PAGE_KEY);
    if (stored && ALL_PAGES.includes(stored as Page)) {
      return stored as Page;
    }
  } catch {
    /* ignore */
  }
  return "dashboard";
}

export default function App() {
  const [page, setPageState] = useState<Page>(readInitialPage);
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [theme, setTheme] = useState<Theme>(readStoredTheme);

  const setPage = (p: Page) => {
    setPageState(p);
    try {
      const url = new URL(window.location.href);
      url.searchParams.set("page", p);
      window.history.replaceState({}, "", url);
      localStorage.setItem(PAGE_KEY, p);
    } catch { /* ignore */ }
  };

  useEffect(() => {
    (async () => {
      try {
        const s = (await invoke("get_status")) as any;
        if (s?.bind) {
          const url = `http://${s.bind}`;
          setApiBase(url);
        }
        setStatus(s);
      } catch (e) {
        try {
          const s = await api.status();
          setStatus(s);
        } catch (e2) {
          setErr(String(e2));
        }
      }
    })();
    // Only subscribe to Tauri events inside the Tauri shell
    if (!isTauri()) {
      return;
    }
    const navP = listen<string>("navigate", (event) => {
      const p = event.payload as Page;
      if (ALL_PAGES.includes(p)) setPage(p);
    });
    const gwP = listen<string>("gateway-ready", async (event) => {
      const bind = event.payload;
      if (bind) {
        setApiBase(`http://${bind}`);
      }
      try {
        const s = await api.status();
        setStatus(s);
      } catch {
        // The old port may be briefly unreachable during the rebind;
        // the next 5-second poll will pick up the new state.
      }
    });
    return () => {
      Promise.all([navP, gwP]).then(([unNav, unGw]) => {
        unNav();
        unGw();
      }).catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    const root = document.documentElement;
    root.classList.remove("theme-light", "theme-dark", "theme-auto");
    root.classList.add(`theme-${theme}`);
    try { localStorage.setItem(THEME_KEY, theme); } catch { /* ignore */ }
  }, [theme]);

  useEffect(() => {
    const id = setInterval(async () => {
      try {
        setStatus(await api.status());
      } catch {
        /* ignore transient errors */
      }
    }, 5000);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;
      const k = e.key;
      if (k >= "1" && k <= "9") {
        const idx = Number(k) - 1;
        if (idx < SHORTCUT_PAGES.length) {
          e.preventDefault();
          setPage(SHORTCUT_PAGES[idx]);
        }
      } else if (k === "0") {
        e.preventDefault();
        const tIdx = SHORTCUT_PAGES.indexOf("tool-profiles");
        if (tIdx >= 0) setPage(SHORTCUT_PAGES[tIdx]);
      } else if (k === "l" || k === "L") {
        e.preventDefault();
        setPage("logs");
      } else if (k === ",") {
        e.preventDefault();
        setPage("settings");
      } else if (k === "r" || k === "R") {
        e.preventDefault();
        api.status().then(setStatus).catch(() => undefined);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  const renderPage = () => {
    switch (page) {
      case "dashboard":
        return <Dashboard status={status} onNavigate={setPage} />;
      case "providers":
        return <Providers />;
      case "models":
        return <Models />;
      case "sessions":
        return <Sessions />;
      case "routing":
        return <Routing />;
      case "health":
        return <Health />;
      case "requests":
        return <Requests />;
      case "analytics":
        return <Analytics />;
      case "debug":
        return <Debug />;
      case "tool-profiles":
        return <ToolProfiles />;
      case "import-export":
        return <ImportExport />;
      case "update":
        return <Update />;
      case "settings":
        return <Settings />;
      case "logs":
        return <Logs />;
    }
  };

  if (page === "switcher") {
    return (
      <ToastProvider>
        <Switcher />
      </ToastProvider>
    );
  }

  return (
    <ToastProvider>
    <Switcher />
    <div className="app">
      <div className="topbar">
        <div className="brand">
          <BrandMark />
          <span className="brand-name">AutoRouter</span>
          <span className="brand-sub">Desktop</span>
        </div>
        <div className="status-chip">
          <span className={"pulse" + (status ? " ok" : "")} />
          {status ? (
            <span className="mono">v{status.version} · {status.bind}</span>
          ) : (
            <span>connecting…</span>
          )}
        </div>
        <div className="grow" />
        <div className="actions">
          <button
            className="btn ghost icon-only"
            onClick={() => setTheme(theme === "dark" ? "light" : "dark")}
            title="Toggle theme"
            aria-label="Toggle theme"
          >
            {theme === "dark" ? <IconSun /> : <IconMoon />}
          </button>
        </div>
      </div>
      <div className="sidebar">
        {NAV.map((n) => {
          const Icon = n.Icon;
          return (
            <div
              key={n.id}
              className={"nav-item" + (page === n.id ? " active" : "")}
              onClick={() => setPage(n.id)}
              role="button"
              tabIndex={0}
              onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") setPage(n.id); }}
            >
              <Icon className="nav-icon" />
              <span className="nav-label">{n.label}</span>
              {n.shortcut && <span className="nav-kbd">⌃{n.shortcut}</span>}
            </div>
          );
        })}
        <div className="spacer" />
        <div
          className="nav-item danger"
          onClick={() => invoke("quit_app")}
          role="button"
          tabIndex={0}
          onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") invoke("quit_app"); }}
        >
          <IconQuit className="nav-icon" />
          <span className="nav-label">Quit</span>
        </div>
      </div>
      <div className="main">
        <ErrorBoundary resetKey={page}>
          {err ? <Onboarding error={err} onRetry={() => location.reload()} /> : renderPage()}
        </ErrorBoundary>
      </div>
    </div>
    </ToastProvider>
  );
}
