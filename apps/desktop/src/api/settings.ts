import { ApiError, type ApiClient } from "./client";

export interface Settings {
  storage: {
    download_bandwidth_limit_bytes_per_second: string | null;
    low_disk_thresholds:
      | { mode: "PERCENT"; warning_basis_points: number; critical_basis_points: number }
      | { mode: "BYTES"; warning_bytes: string; critical_bytes: string };
    [key: string]: unknown;
  };
  [key: string]: unknown;
}

export interface VersionedSettings {
  value: Settings;
  revision: number;
}

const PATH = "/api/v1/settings";

function revisionFrom(headers: Record<string, string> | undefined): number {
  const raw = headers?.etag;
  const match = raw && /^"?(0|[1-9][0-9]*)"?$/.exec(raw);
  const revision = match ? Number(match[1]) : Number.NaN;
  if (!Number.isSafeInteger(revision) || revision < 0) {
    throw new ApiError("PROTOCOL_ERROR", "La respuesta de configuración no incluye una revisión válida.", 502, false, null, "settings_etag_invalid");
  }
  return revision;
}

export async function readSettings(client: ApiClient): Promise<VersionedSettings> {
  const response = await client.requestRaw<Settings>({ method: "GET", path: PATH });
  return { value: response.body, revision: revisionFrom(response.headers) };
}

export async function writeSettings(client: ApiClient, revision: number, value: Settings): Promise<VersionedSettings> {
  const response = await client.requestRaw<Settings>({
    method: "PATCH",
    path: PATH,
    headers: { "If-Match": String(revision) },
    body: value,
  });
  return { value: response.body, revision: revisionFrom(response.headers) };
}
