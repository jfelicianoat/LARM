import { ApiClient, ApiError } from "./client";

/** Estado del daemon que muestra la interfaz (P006). */
export type DaemonStatus = "CONNECTING" | "ONLINE" | "DEGRADED" | "OFFLINE";

export interface StatusSnapshot {
  status: DaemonStatus;
  /** Último error, para la página de diagnóstico. */
  error: ApiError | null;
  checkedAt: Date | null;
}

interface Health {
  status: "LIVE" | "READY" | "DEGRADED";
  schema_version: string;
}

/**
 * Estado a partir de `health/live` (público) y `health/ready` (scope read).
 * - live inalcanzable → OFFLINE
 * - ready READY → ONLINE
 * - ready DEGRADED, o el daemon vive pero ready falla → DEGRADED
 */
export async function probe(client: ApiClient): Promise<StatusSnapshot> {
  const checkedAt = new Date();
  try {
    await client.get<Health>("/api/v1/health/live");
  } catch (e) {
    const error = e instanceof ApiError ? e : new ApiError("DAEMON_UNREACHABLE", String(e), 0, true, null, null);
    return { status: "OFFLINE", error, checkedAt };
  }
  try {
    const ready = await client.get<Health>("/api/v1/health/ready");
    return { status: ready.status === "READY" ? "ONLINE" : "DEGRADED", error: null, checkedAt };
  } catch (e) {
    const error = e instanceof ApiError ? e : null;
    return { status: error?.isOffline ? "OFFLINE" : "DEGRADED", error, checkedAt };
  }
}

/** Texto en español para cada estado. */
export const STATUS_LABEL: Record<DaemonStatus, string> = {
  CONNECTING: "Conectando…",
  ONLINE: "En línea",
  DEGRADED: "Degradado",
  OFFLINE: "Sin conexión",
};
