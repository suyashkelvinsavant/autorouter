// Thin shim that proxies Tauri command invocations. When the app
// is running outside the Tauri shell (e.g. during `vite dev` with no
// desktop host), the calls are no-ops so the UI still renders.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";

export function isTauri(): boolean {
  return typeof (window as any).__TAURI_INTERNALS__ !== "undefined";
}

export async function reveal_data_dir(): Promise<string | null> {
  if (!isTauri()) return null;
  try {
    return (await tauriInvoke("reveal_data_dir")) as string;
  } catch {
    return null;
  }
}

export async function quit_app(): Promise<void> {
  if (!isTauri()) return;
  try {
    await tauriInvoke("quit_app");
  } catch {
    /* ignore */
  }
}
