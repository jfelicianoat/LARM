import type { ApiClient } from "./client";

export type InstanceState =
  | "CREATED" | "STARTING" | "LOADING" | "READY" | "BUSY" | "DEGRADED"
  | "FAILED" | "STOPPING" | "STOPPED" | "CRASHED";

export interface RuntimeInstance {
  id: string;
  created_at: string;
  updated_at: string;
  row_version: number;
  metadata: Record<string, unknown>;
  runtime_installation_id: string;
  variant_id: string;
  backend_id: string;
  launch_profile_id: string;
  state: InstanceState;
  pid: number | null;
  process_started_at: string | null;
  executable_hash: string | null;
  generation: number;
  execution_fingerprint: string | null;
  last_used_at: string | null;
  ownership: "LARM_MANAGED" | "EXTERNAL";
}

export interface InstanceStopReceipt {
  job_id: string;
  status_url: string;
  state: "QUEUED" | "RUNNING" | "COMPLETED" | "FAILED" | "CANCELLED";
}

export interface InstanceJobState {
  job_id: string;
  state: InstanceStopReceipt["state"];
  resource_type: string;
  resource_id: string | null;
  progress: Record<string, unknown>;
  result: { stopped?: boolean } | null;
  error: { code: string; message: string } | null;
  evidence_refs: string[];
}

export interface InstanceVariantDetail {
  canonical_model: { name: string };
  variant: { format: string | null; quantization: string | null; architecture: string | null };
}

export interface InstanceBackend {
  backend_type: string;
  status: "AVAILABLE" | "UNAVAILABLE" | "UNKNOWN";
  driver_version: string | null;
}

export function requestInstanceStop(client: ApiClient, instanceId: string, key: string): Promise<InstanceStopReceipt> {
  return client.request<InstanceStopReceipt>({
    method: "POST",
    path: `/api/v1/instances/${encodeURIComponent(instanceId)}/stop`,
    body: { reason: "operator_requested", approval_id: null },
    headers: { "Idempotency-Key": key },
  });
}

export function readInstanceStopJob(client: ApiClient, jobId: string): Promise<InstanceJobState> {
  return client.get<InstanceJobState>(`/api/v1/jobs/${encodeURIComponent(jobId)}`);
}
