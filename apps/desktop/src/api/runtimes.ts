import { ApiClient } from "./client";

export const CHANNELS = ["stable", "prerelease", "nightly", "commit", "custom"] as const;
export type RuntimeChannel = typeof CHANNELS[number];
export const CHANNEL_LABEL: Record<RuntimeChannel, string> = {
  stable: "Estable", prerelease: "Prelanzamiento", nightly: "Nocturno", commit: "Commit", custom: "Personalizado",
};
export const INSTALLATION_STRATEGIES = [
  "PREBUILT_BINARY", "GITHUB_RELEASE", "SOURCE_BUILD", "WINGET", "PIP", "UV", "CONDA", "MAMBA",
  "NUGET", "WSL", "CONTAINER", "CUSTOM", "MSI_MANAGED", "PYTHON_VENV", "EXTERNAL_ADOPTION",
] as const;
export type InstallationStrategy = typeof INSTALLATION_STRATEGIES[number];
export interface RuntimeInstallRequest {
  runtime_id: string;
  upstream_version: string;
  strategy: InstallationStrategy;
  storage_location_id: string;
  profile_lock_hash: string;
  external_host_id: string | null;
}
export interface JobReceipt { job_id: string; status_url: string; state: "QUEUED" | "RUNNING" | "COMPLETED" | "FAILED" | "CANCELLED" }
export interface RuntimeInstallJobState {
  job_id: string;
  state: JobReceipt["state"];
  error: { code: string; message: string } | null;
}
export interface ChannelPolicy { allowed_channels: RuntimeChannel[] }
export interface RuntimeRetentionPolicy { keep_last_validated: number }
export type RuntimeVersionLifecycle = "CANDIDATE" | "INSTALLING" | "VALIDATING" | "VALIDATED" | "DEFAULT" | "RETAINED" | "BROKEN" | "REJECTED";
interface RecordBase { id: string; created_at: string; updated_at: string; row_version: number; metadata: Record<string, unknown> }
export interface RuntimeCreate {
  name: string; adapter_id: string; capabilities: string[]; default_version_id: string | null; channel_policy: ChannelPolicy; retention_policy: RuntimeRetentionPolicy;
}
export interface Runtime extends RecordBase, RuntimeCreate {}
export interface RuntimeVersion extends RecordBase {
  runtime_id: string; version_string: string; commit: string | null; build: string | null;
  channel: RuntimeChannel; released_at: string | null; artifact_hash: string | null;
  promotion: "NONE" | "CANDIDATE" | "DEFAULT" | "PINNED" | "REJECTED";
  lifecycle: RuntimeVersionLifecycle;
}
export interface RuntimeVersionPin extends RecordBase {
  variant_id: string;
  launch_profile_id: string;
  runtime_version_id: string;
}
export interface RuntimeInstallation extends RecordBase {
  runtime_version_id: string; storage_location_id: string | null; relative_path: string | null;
  external_host_id: string | null; strategy: string; state: string; stage: string;
  registration_mode: string; ownership: string; lock_hash: string | null;
}
export interface RuntimeBackend extends RecordBase {
  runtime_installation_id: string; backend_type: string; device_id: string | null; driver_version: string | null;
  status: "AVAILABLE" | "UNAVAILABLE" | "UNKNOWN"; evidence_refs: string[];
  limits: { max_instances: number; multi_model_support: boolean; parallel_requests: number; gpu_sharing: boolean };
}
export interface ExternalHost extends RecordBase {
  name: string; base_url: string; host_type: string; credential_reference_id: string | null;
  observed_version: string | null; state: "ONLINE" | "OFFLINE" | "UNKNOWN";
  registration_mode: string; ownership: "EXTERNAL";
}
export interface ExternalHostCreate {
  name: string; base_url: string; host_type: string; credential_reference_id: string | null;
  registration_mode: "ADOPTED" | "EXTERNAL";
}
export interface AdapterInstallation {
  id: string; adapter_id: string; version: string; family: string; metadata: Record<string, unknown>;
}
export interface Connector { id: string; label: string; capabilities: string[] }
export interface Page<T> { items: T[]; next_cursor: string | null; snapshot_at: string }
export interface RuntimeProfileLock {
  schema_version: "larm.runtime-profile-lock/1";
  key: {
    runtime: string; source_url: string; source_revision: string; source_sha256: string;
    upstream_version: string; os: string; os_build: string; backend: string; protocol: string;
  };
  fields: Record<string, unknown>;
  qualification: "UNQUALIFIED";
  lock_sha256: string;
}

/** Cancela una instalación en cola o solicita la cancelación al worker activo. */
export function cancelRuntimeInstallJob(client: ApiClient, jobId: string): Promise<JobReceipt> {
  return client.request<JobReceipt>({ method: "POST", path: `/api/v1/jobs/${encodeURIComponent(jobId)}/cancel` });
}

export function runtimeConnections(adapters: AdapterInstallation[]): Connector[] {
  return adapters.flatMap((adapter) => adapter.family !== "runtime" || !Array.isArray(adapter.metadata.runtime_bindings) ? []
    : adapter.metadata.runtime_bindings.flatMap((binding: unknown) => {
      if (!binding || typeof binding !== "object" || !("id" in binding) || typeof binding.id !== "string"
        || !("runtime_family" in binding) || typeof binding.runtime_family !== "string"
        || !("capabilities" in binding) || !Array.isArray(binding.capabilities)
        || !binding.capabilities.every((value: unknown) => typeof value === "string")) return [];
      return [{ id: binding.id, label: `${binding.runtime_family} · ${adapter.adapter_id} ${adapter.version}`, capabilities: binding.capabilities as string[] }];
    }));
}

/** Sólo lecturas; un cambio de selección invalida la generación en el consumidor. */
export async function allPages<T>(client: ApiClient, path: string, current: () => boolean): Promise<T[]> {
  const items: T[] = [], seen = new Set<string>();
  let cursor: string | null = null;
  do {
    const url = new URL(path, "http://local.invalid");
    url.searchParams.set("limit", "200");
    if (cursor) url.searchParams.set("cursor", cursor);
    const page = await client.get<Page<T>>(url.pathname + url.search);
    if (!current()) return [];
    items.push(...page.items); cursor = page.next_cursor;
    if (cursor && seen.has(cursor)) throw new Error("El catálogo repitió un cursor");
    if (cursor) seen.add(cursor);
  } while (cursor);
  return items;
}
