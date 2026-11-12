import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import App from "./App";
import type { RawRequest, RawResponse, Transport } from "./api/client";
import type { RuntimeInstance } from "./api/instances";

afterEach(() => { cleanup(); window.location.hash = ""; });

const INSTANCE_ID = "0192aaaa-0000-7000-8000-000000000001";
const VARIANT_ID = "0192aaaa-0000-7000-8000-000000000002";
const BACKEND_ID = "0192aaaa-0000-7000-8000-000000000003";
const JOB_ID = "0192aaaa-0000-7000-8000-000000000004";
const STAMP = "2026-10-20T13:00:00.000Z";

const instance = (overrides: Partial<RuntimeInstance> = {}): RuntimeInstance => ({
  id: INSTANCE_ID,
  created_at: STAMP,
  updated_at: STAMP,
  row_version: 1,
  metadata: {
    larm_runtime_launch_snapshot_v1: {
      launch_plan: {
        memory_estimate: { total_bytes: "1073741824" },
        effective_memory_limit_bytes: "2147483648",
      },
    },
  },
  runtime_installation_id: "0192aaaa-0000-7000-8000-000000000005",
  variant_id: VARIANT_ID,
  backend_id: BACKEND_ID,
  launch_profile_id: "0192aaaa-0000-7000-8000-000000000006",
  state: "READY",
  pid: 3210,
  process_started_at: STAMP,
  executable_hash: "a".repeat(64),
  generation: 1,
  execution_fingerprint: "b".repeat(64),
  last_used_at: STAMP,
  ownership: "LARM_MANAGED",
  ...overrides,
});

const page = (items: unknown[], next_cursor: string | null = null): RawResponse => ({
  status: 200,
  body: { items, next_cursor, snapshot_at: STAMP },
});

type Handler = (request: RawRequest) => RawResponse | Promise<RawResponse>;
function transport(handler: Handler): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const result = (async (request: RawRequest) => {
    calls.push(request);
    if (request.path === "/api/v1/health/live") return { status: 200, body: { status: "LIVE", schema_version: "1" } };
    if (request.path === "/api/v1/health/ready") return { status: 200, body: { status: "READY", schema_version: "1" } };
    return handler(request);
  }) as Transport & { calls: RawRequest[] };
  result.calls = calls;
  return result;
}

function open(t: Transport) {
  window.location.hash = "#/instancias";
  render(<App transport={t} pollMs={60_000} />);
}

function baseHandler(current = instance()): Handler {
  return (request) => {
    if (request.path === "/api/v1/instances?limit=200") return page([current]);
    if (request.path === `/api/v1/instances/${INSTANCE_ID}`) return { status: 200, body: current };
    if (request.path === `/api/v1/variants/${VARIANT_ID}/detail`) return {
      status: 200,
      body: { canonical_model: { name: "Modelo local" }, variant: { format: "GGUF", quantization: "Q4_K_M", architecture: "llama" } },
    };
    if (request.path === `/api/v1/runtime-backends/${BACKEND_ID}`) return {
      status: 200,
      body: { backend_type: "CUDA", status: "AVAILABLE", driver_version: "12.4" },
    };
    return page([]);
  };
}

describe("pantalla de instancias", () => {
  it("muestra modelo, backend, proceso, tiempo activo y memoria estimada sin inventar telemetría", async () => {
    open(transport(baseHandler()));
    await screen.findByText("Modelo local");
    expect(screen.getByText(/CUDA · disponible/)).toBeTruthy();
    expect(screen.getByText("3210")).toBeTruthy();
    expect(screen.getByText("1,0 GiB")).toBeTruthy();
    expect(screen.getByText("2,0 GiB")).toBeTruthy();
    expect(screen.getByText(/todavía no publica un endpoint, memoria medida ni ubicación de logs/)).toBeTruthy();
  });

  it("requiere confirmación y envía una parada idempotente; consulta y refleja el job terminal", async () => {
    let current = instance();
    const t = transport((request) => {
      if (request.path === "/api/v1/instances?limit=200") return page([current]);
      if (request.path === `/api/v1/instances/${INSTANCE_ID}`) return { status: 200, body: current };
      if (request.path === `/api/v1/variants/${VARIANT_ID}/detail`) return { status: 200, body: { canonical_model: { name: "Modelo local" }, variant: { format: "GGUF", quantization: null, architecture: null } } };
      if (request.path === `/api/v1/runtime-backends/${BACKEND_ID}`) return { status: 200, body: { backend_type: "CPU", status: "UNKNOWN", driver_version: null } };
      if (request.method === "POST" && request.path === `/api/v1/instances/${INSTANCE_ID}/stop`) {
        current = instance({ state: "STOPPING", row_version: 2 });
        return { status: 202, body: { job_id: JOB_ID, status_url: `/api/v1/jobs/${JOB_ID}`, state: "QUEUED" } };
      }
      if (request.path === `/api/v1/jobs/${JOB_ID}`) return {
        status: 200,
        body: { job_id: JOB_ID, state: "COMPLETED", resource_type: "runtime_instance", resource_id: INSTANCE_ID, progress: {}, result: { stopped: true }, error: null, evidence_refs: [] },
      };
      return page([]);
    });

    open(t);
    await screen.findByText("Modelo local");
    expect(t.calls.some((request) => request.method === "POST" && request.path.endsWith("/stop"))).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Detener instancia" }));
    expect(screen.getByRole("heading", { name: "¿Detener esta instancia?" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Sí, detener instancia" }));
    await screen.findByText(JOB_ID);
    const stop = t.calls.find((request) => request.method === "POST" && request.path.endsWith("/stop"));
    expect(stop).toEqual({
      method: "POST",
      path: `/api/v1/instances/${INSTANCE_ID}/stop`,
      body: { reason: "operator_requested", approval_id: null },
      headers: { "Idempotency-Key": expect.any(String) },
    });
    fireEvent.click(screen.getByRole("button", { name: "Consultar estado de parada" }));
    await screen.findByRole("heading", { name: "Instancia detenida" });
    await waitFor(() => expect(t.calls.some((request) => request.path === `/api/v1/jobs/${JOB_ID}`)).toBe(true));
  });

  it("deja una instancia externa en solo lectura y permite reintentar si la lista falla", async () => {
    const external = instance({ ownership: "EXTERNAL", state: "READY" });
    let attempts = 0;
    const t = transport((request) => {
      if (request.path === "/api/v1/instances?limit=200" && attempts++ === 0) return { status: 503, body: { code: "RESOURCE_UNAVAILABLE", message: "offline fixture", retryable: true } };
      return baseHandler(external)(request);
    });
    open(t);
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Reintentar" }));
    await screen.findByText("Modelo local");
    expect(screen.queryByRole("button", { name: "Detener instancia" })).toBeNull();
    expect(screen.getByText(/LARM no puede detenerla/)).toBeTruthy();
    expect(t.calls.some((request) => request.method === "POST" && request.path.endsWith("/stop"))).toBe(false);
  });
});
