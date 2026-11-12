import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ModelTransform } from "./model-transform";
import { ApiClient, type RawRequest, type RawResponse, type Transport } from "./api/client";

afterEach(() => cleanup());

function transport(handler: (request: RawRequest) => RawResponse | Promise<RawResponse>): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const value = (async (request: RawRequest) => { calls.push(request); return handler(request); }) as Transport & { calls: RawRequest[] };
  value.calls = calls;
  return value;
}

const adaptersPage = {
  items: [
    { id: "inst-1", adapter_id: "product.transformation.gguf", version: "1.1.0", family: "transformation", state: "VALIDATED" },
    { id: "inst-2", adapter_id: "reference.transformation.base64", version: "1.0.0", family: "transformation", state: "INSTALLED" },
  ],
  next_cursor: null,
};
const locationsPage = {
  items: [
    { id: "loc-1", name: "Disco modelos", state: "ONLINE" },
  ],
  next_cursor: null,
};

function planFixture(planId: string) {
  return {
    plan_id: planId,
    plan_hash: "ab".repeat(32),
    steps: [{ phase: "GGUF", adapter_id: "product.transformation.gguf" }, { phase: "register_managed_output", adapter_id: null }],
    storage_estimate: {
      confidence: "ESTIMATED", existing_bytes: "12583184", upper_bound_bytes: "17000000",
      peak_extra_bytes: "17000000", required_free_bytes: "18000000",
      per_volume: [{ volume_key: "models", peak_extra_bytes: "17000000", margin_bytes: "1000000", usable_free_bytes: "900000000000", eligible: true }],
    },
    expires_at: "2026-11-12T12:00:00Z",
    approval_required: false,
  };
}

const variantId = "0192aaaa-0000-7000-8000-0000000000aa";

describe("panel de transformación de modelo", () => {
  it("prepara el plan, lo muestra antes de confirmar y completa el job con la variante derivada", async () => {
    const jobId = "0192aaaa-0000-7000-8000-000000000021";
    const planId = "0192aaaa-0000-7000-8000-000000000022";
    let pollCount = 0;
    const t = transport((request) => {
      if (request.method === "GET" && request.path === "/api/v1/adapters?family=transformation&limit=200") return { status: 200, body: adaptersPage };
      if (request.method === "GET" && request.path === "/api/v1/storage-locations?limit=200") return { status: 200, body: locationsPage };
      if (request.method === "POST" && request.path === "/api/v1/transform-plans") {
        expect(request.body).toMatchObject({
          input_variant_id: variantId,
          adapter_id: "product.transformation.gguf",
          tool_version: "1.1.0",
          target_format: "GGUF",
          parameters: { quantization: "Q8_0" },
          storage_location_id: "loc-1",
          hard_budget_bytes: null,
        });
        return { status: 201, body: planFixture(planId) };
      }
      if (request.method === "POST" && request.path === "/api/v1/transformations") {
        expect(request.body).toMatchObject({ plan_id: planId, plan_hash: "ab".repeat(32), approval_id: null });
        return { status: 202, body: { job_id: jobId, status_url: `/api/v1/jobs/${jobId}`, state: "QUEUED" } };
      }
      if (request.method === "GET" && request.path === `/api/v1/jobs/${jobId}`) {
        pollCount += 1;
        return { status: 200, body: { job_id: jobId, state: pollCount >= 2 ? "COMPLETED" : "RUNNING", progress: { worker: { transformation_state: "RUNNING" } }, result: { variant: { id: "0192aaaa-0000-7000-8000-0000000000bb" } }, error: null } };
      }
      throw new Error(`unexpected ${request.method} ${request.path}`);
    });
    const completed = vi.fn();
    render(<ModelTransform client={new ApiClient(t)} variantId={variantId} onCompleted={completed} onClose={() => undefined} />);

    expect(await screen.findByLabelText("Adaptador de transformación")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Formato de salida"), { target: { value: "GGUF" } });
    fireEvent.change(screen.getByLabelText("Cuantización (opcional)"), { target: { value: "Q8_0" } });
    fireEvent.click(screen.getByRole("button", { name: "Preparar plan" }));

    expect(await screen.findByText(/Plan sellado/)).toBeTruthy();
    expect(screen.getByText(/GGUF → register_managed_output/)).toBeTruthy();
    // La confirmación no se envía sin el checkbox explícito.
    expect(t.calls.some((call) => call.path === "/api/v1/transformations")).toBe(false);

    fireEvent.click(screen.getByRole("checkbox", { name: "Confirmo ejecutar la transformación con este plan sellado." }));
    fireEvent.click(screen.getByRole("button", { name: "Ejecutar transformación" }));

    expect(await screen.findByText(/Transformación completada/, undefined, { timeout: 5000 })).toBeTruthy();
    expect(screen.getByText(/0192aaaa-0000-7000-8000-0000000000bb/)).toBeTruthy();
    expect(completed).toHaveBeenCalledTimes(1);
    expect(t.calls.some((call) => call.path === "/api/v1/transform-plans")).toBe(true);
    expect(t.calls.some((call) => call.path === "/api/v1/transformations")).toBe(true);
  });

  it("muestra el error del plan rechazado y no permite confirmar", async () => {
    const t = transport((request) => {
      if (request.method === "GET" && request.path === "/api/v1/adapters?family=transformation&limit=200") return { status: 200, body: adaptersPage };
      if (request.method === "GET" && request.path === "/api/v1/storage-locations?limit=200") return { status: 200, body: locationsPage };
      if (request.method === "POST" && request.path === "/api/v1/transform-plans") {
        return { status: 404, body: { code: "NOT_FOUND", message: "Adaptador desconocido", retryable: false, correlation_id: "c1", details: { reason: "transformation_adapter_not_registered" } } };
      }
      throw new Error(`unexpected ${request.method} ${request.path}`);
    });
    render(<ModelTransform client={new ApiClient(t)} variantId={variantId} onCompleted={() => undefined} onClose={() => undefined} />);

    expect(await screen.findByLabelText("Adaptador de transformación")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Formato de salida"), { target: { value: "GGUF" } });
    fireEvent.click(screen.getByRole("button", { name: "Preparar plan" }));

    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.getByText("El adaptador de transformación o la variante de entrada no están disponibles.")).toBeTruthy();
    expect(t.calls.some((call) => call.path === "/api/v1/transformations")).toBe(false);
  });

  it("solicita la cancelación del job en curso y muestra el cierre en Cancelada", async () => {
    const jobId = "0192aaaa-0000-7000-8000-000000000041";
    const planId = "0192aaaa-0000-7000-8000-000000000042";
    const t = transport((request) => {
      if (request.method === "GET" && request.path === "/api/v1/adapters?family=transformation&limit=200") return { status: 200, body: adaptersPage };
      if (request.method === "GET" && request.path === "/api/v1/storage-locations?limit=200") return { status: 200, body: locationsPage };
      if (request.method === "POST" && request.path === "/api/v1/transform-plans") return { status: 201, body: planFixture(planId) };
      if (request.method === "POST" && request.path === "/api/v1/transformations") return { status: 202, body: { job_id: jobId, status_url: `/api/v1/jobs/${jobId}`, state: "QUEUED" } };
      if (request.method === "GET" && request.path === `/api/v1/jobs/${jobId}`) return { status: 200, body: { job_id: jobId, state: "RUNNING", progress: { worker: { transformation_state: "RUNNING" } }, result: null, error: null } };
      if (request.method === "POST" && request.path === `/api/v1/jobs/${jobId}/cancel`) return { status: 202, body: { job_id: jobId, status_url: `/api/v1/jobs/${jobId}`, state: "CANCELLING" } };
      throw new Error(`unexpected ${request.method} ${request.path}`);
    });
    render(<ModelTransform client={new ApiClient(t)} variantId={variantId} onCompleted={() => undefined} onClose={() => undefined} />);

    fireEvent.change(await screen.findByLabelText("Formato de salida"), { target: { value: "GGUF" } });
    fireEvent.click(screen.getByRole("button", { name: "Preparar plan" }));
    expect(await screen.findByText(/Plan sellado/)).toBeTruthy();
    fireEvent.click(screen.getByRole("checkbox", { name: "Confirmo ejecutar la transformación con este plan sellado." }));
    fireEvent.click(screen.getByRole("button", { name: "Ejecutar transformación" }));

    fireEvent.click(await screen.findByRole("button", { name: "Solicitar cancelación" }));
    expect(await screen.findByText(/cancelación solicitada/)).toBeTruthy();
    expect(t.calls.some((call) => call.method === "POST" && call.path === `/api/v1/jobs/${jobId}/cancel`)).toBe(true);
    // El botón desaparece mientras la cancelación coopera.
    expect(screen.queryByRole("button", { name: "Solicitar cancelación" })).toBeNull();
  });

  it("muestra el error del job fallido", async () => {
    const jobId = "0192aaaa-0000-7000-8000-000000000031";
    const planId = "0192aaaa-0000-7000-8000-000000000032";
    const t = transport((request) => {
      if (request.method === "GET" && request.path === "/api/v1/adapters?family=transformation&limit=200") return { status: 200, body: adaptersPage };
      if (request.method === "GET" && request.path === "/api/v1/storage-locations?limit=200") return { status: 200, body: locationsPage };
      if (request.method === "POST" && request.path === "/api/v1/transform-plans") return { status: 201, body: planFixture(planId) };
      if (request.method === "POST" && request.path === "/api/v1/transformations") return { status: 202, body: { job_id: jobId, status_url: `/api/v1/jobs/${jobId}`, state: "QUEUED" } };
      if (request.method === "GET" && request.path === `/api/v1/jobs/${jobId}`) return { status: 200, body: { job_id: jobId, state: "FAILED", progress: {}, result: null, error: { message: "La conversión GGUF falló", code: "RESOURCE_UNAVAILABLE" } } };
      throw new Error(`unexpected ${request.method} ${request.path}`);
    });
    render(<ModelTransform client={new ApiClient(t)} variantId={variantId} onCompleted={() => undefined} onClose={() => undefined} />);

    fireEvent.change(await screen.findByLabelText("Formato de salida"), { target: { value: "GGUF" } });
    fireEvent.click(screen.getByRole("button", { name: "Preparar plan" }));
    expect(await screen.findByText(/Plan sellado/)).toBeTruthy();
    fireEvent.click(screen.getByRole("checkbox", { name: "Confirmo ejecutar la transformación con este plan sellado." }));
    fireEvent.click(screen.getByRole("button", { name: "Ejecutar transformación" }));

    expect(await screen.findByText(/La conversión GGUF falló \(RESOURCE_UNAVAILABLE\)/, undefined, { timeout: 5000 })).toBeTruthy();
    expect(screen.getByText(/Transformación fallida/)).toBeTruthy();
  });
});
