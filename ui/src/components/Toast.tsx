// Single global toast for "Copied", "Saved", "Failed" feedback.
//
// The Tauri WebView2 on Windows sometimes denies `navigator.clipboard` for
// non-secure contexts, so callers wrap `writeText` in `try/catch`. The toast
// mirrors that pattern: callers fire-and-forget, the provider swaps any
// in-flight toast for the new one (no toast pile-up) and auto-dismisses after
// 2 seconds.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { IconCheck, IconAlertTriangle } from "./Icons";

export type ToastKind = "ok" | "err";

interface ToastState {
  id: number;
  msg: string;
  kind: ToastKind;
}

export interface ToastApi {
  /** Stable ref — safe to depend on in useEffect dep lists. */
  show: (msg: string, kind?: ToastKind) => void;
}

const ToastContext = createContext<ToastApi | null>(null);

export function useToast(): ToastApi {
  const ctx = useContext(ToastContext);
  if (!ctx) {
    // Defensive: never crash a tree that forgot the provider. A no-op toast
    // is better than a white screen on the dashboard.
    return { show: () => undefined };
  }
  return ctx;
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toast, setToast] = useState<ToastState | null>(null);
  // Incrementing id lets a new toast preempt a slow `setTimeout` cleanup
  // without leaving the old timer to clobber the new toast.
  const idRef = useRef(0);
  const timerRef = useRef<number | null>(null);

  const show = useCallback((msg: string, kind: ToastKind = "ok") => {
    if (timerRef.current !== null) {
      window.clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    idRef.current += 1;
    setToast({ id: idRef.current, msg, kind });
    timerRef.current = window.setTimeout(() => {
      timerRef.current = null;
      // Use the latest id at fire time so an in-flight timeout from a
      // toast that's already been replaced can't clear the replacement.
      setToast((prev) => (prev && prev.id === idRef.current ? null : prev));
    }, 2000);
  }, []);

  useEffect(() => {
    return () => {
      if (timerRef.current !== null) {
        window.clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
  }, []);

  return (
    <ToastContext.Provider value={{ show }}>
      {children}
      {toast ? (
        <div
          className={`toast ${toast.kind === "err" ? "err" : "ok"}`}
          role={toast.kind === "err" ? "alert" : "status"}
          aria-live={toast.kind === "err" ? "assertive" : "polite"}
          key={toast.id}
        >
          <span className="toast-icon" aria-hidden>
            {toast.kind === "err" ? <IconAlertTriangle /> : <IconCheck />}
          </span>
          <span className="toast-msg">{toast.msg}</span>
        </div>
      ) : null}
    </ToastContext.Provider>
  );
}
