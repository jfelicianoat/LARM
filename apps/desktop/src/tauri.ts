/** Verdadero solo dentro del shell Tauri: exige el puente IPC real, no un objeto residual. */
export function isTauri(): boolean {
  if (typeof window === "undefined") return false;
  const internals = (window as unknown as { __TAURI_INTERNALS__?: { invoke?: unknown } }).__TAURI_INTERNALS__;
  return typeof internals?.invoke === "function";
}
