import { ApiClient, ApiError, tauriTransport, type Transport } from "./api/client";

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/**
 * Prueba de humo del shell (P001/P006). Solo actúa con LARM_DESKTOP_SMOKE=1:
 * hace `ping` y recorre la cadena real WebView → backend Tauri → bootstrap →
 * API (`health/live` y `health/ready`), y entrega el resultado al backend,
 * que lo imprime y cierra la ventana.
 */
export async function runSmoke(invoke: Invoke, transport: Transport = tauriTransport): Promise<boolean> {
  if (!(await invoke<boolean>("smoke_enabled"))) return false;
  const report: Record<string, unknown> = { ping: await invoke<string>("ping") };
  const client = new ApiClient(transport);
  for (const [name, path] of [["live", "/api/v1/health/live"], ["ready", "/api/v1/health/ready"]] as const) {
    try {
      report[name] = await client.get<{ status: string }>(path);
    } catch (e) {
      report[name] = e instanceof ApiError ? { error: e.code, reason: e.reason } : { error: String(e) };
    }
  }
  await invoke("smoke_report", { result: JSON.stringify(report) });
  return true;
}
