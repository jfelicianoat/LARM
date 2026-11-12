import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ApiClient, type RawRequest, type RawResponse, type Transport } from "./api/client";
import { DownloadsPage } from "./downloads";

afterEach(cleanup);

const downloadId = "0192aaaa-0000-7000-8000-000000000101";
const variantId = "0192aaaa-0000-7000-8000-000000000102";
const locationId = "0192aaaa-0000-7000-8000-000000000103";
const transformId = "0192aaaa-0000-7000-8000-000000000104";
const inputId = "0192aaaa-0000-7000-8000-000000000105";
const adapterId = "0192aaaa-0000-7000-8000-000000000106";

function transport(downloadResponse: RawResponse): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const fake = (async (request: RawRequest) => {
    calls.push(request);
    if (request.method === "PATCH" && request.path.endsWith("/priority")) {
      return {
        status: 200,
        headers: { etag: '"2"' },
        body: { ...download({ phase: "RUNNING" }), ...(request.body as object), row_version: 2 },
      };
    }
    if (request.path.startsWith("/api/v1/downloads")) return downloadResponse;
    if (request.path.startsWith("/api/v1/transformations")) return {
      status: 200,
      body: { items: [{
        id: transformId, created_at: "2026-10-24T10:00:00.000Z", updated_at: "2026-10-24T10:00:00.000Z", row_version: 1,
        metadata: { plan_snapshot: { storage_estimate: { temporary_peak_bytes: "2147483648" } } },
        input_variant_id: inputId, output_variant_id: null, adapter_id: adapterId, tool_version: "1.2.3",
        parameters: { quantization: "Q4_K_M" }, plan_hash: "a".repeat(64), state: "FAILED_RECOVERABLE", attempt: 1,
        error: { code: "PROCESS_INTERRUPTED", message: "El proceso se interrumpió." },
      }], next_cursor: null, snapshot_at: "2026-10-24T10:00:00.000Z" },
    };
    throw new Error(`Ruta inesperada: ${request.path}`);
  }) as Transport & { calls: RawRequest[] };
  fake.calls = calls;
  return fake;
}

function download(progress: Record<string, unknown>) {
  return {
    id: downloadId, created_at: "2026-10-24T10:00:00.000Z", updated_at: "2026-10-24T10:00:00.000Z", row_version: 1,
    metadata: {}, variant_id: variantId, storage_location_id: locationId, plan_hash: "b".repeat(64), state: "RUNNING",
    attempt: 1, progress, error: null, priority: "NORMAL",
  };
}

describe("cola de descargas y transformaciones", () => {
  it("muestra los bytes completados persistidos sin inventar el tamaño del origen", async () => {
    // Forma observada en el SQLite de la aceptación nativa del 2026-10-30.
    const job = { ...download({
      phase: "COMPLETED", completed_artifacts: 1, completed_bytes: "2097152",
      artifacts: { [variantId]: { durable_offset: "2097152", resume_validator: null, source_id: null, revision_key: null } },
      bytes_done: "0",
    }), state: "COMPLETED" };
    const fake = transport({ status: 200, body: { items: [job], next_cursor: null } });
    render(<DownloadsPage client={new ApiClient(fake)} pollMs={60_000} />);

    expect(await screen.findByText("2,0 MiB · total desconocido")).toBeTruthy();
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.queryByText("Tiempo restante")).toBeNull();
    expect(screen.queryByText("Velocidad")).toBeNull();
  });

  it("suma checkpoints durables distintos sin sumar otra vez los bytes completados", async () => {
    const job = { ...download({
      completed_bytes: "9999",
      artifacts: { a: { durable_offset: "1024" }, b: { durable_offset: "2048" } },
    }), state: "PAUSED" };
    const fake = transport({ status: 200, body: { items: [job], next_cursor: null } });
    render(<DownloadsPage client={new ApiClient(fake)} pollMs={60_000} />);

    expect(await screen.findByText("3,0 KiB · total desconocido")).toBeTruthy();
    expect(screen.queryByText("Tiempo restante")).toBeNull();
  });

  it("conserva un checkpoint de cero bytes", async () => {
    const fake = transport({ status: 200, body: { items: [download({ artifacts: { a: { durable_offset: "0" } } })], next_cursor: null } });
    render(<DownloadsPage client={new ApiClient(fake)} pollMs={60_000} />);
    expect(await screen.findByText("0 bytes · total desconocido")).toBeTruthy();
  });

  it.each([{}, { a: null }, { a: { durable_offset: 512 } }, { a: { durable_offset: "-1" } }, { a: { durable_offset: "1e3" } }, { a: { durable_offset: "512" }, b: {} }])(
    "no presenta un agregado parcial o inválido como transferido: %j", async (artifacts) => {
      const fake = transport({ status: 200, body: { items: [download({ artifacts })], next_cursor: null } });
      render(<DownloadsPage client={new ApiClient(fake)} pollMs={60_000} />);
      expect(await screen.findByText("No informado · total desconocido")).toBeTruthy();
      expect(screen.queryByRole("progressbar")).toBeNull();
    },
  );

  it("no inventa total ni tiempo restante cuando faltan datos", async () => {
    const fake = transport({
      status: 200,
      body: { items: [download({ phase: "RUNNING", bytes_done: "512" })], next_cursor: null, snapshot_at: "2026-10-24T10:00:00.000Z" },
    });
    render(<DownloadsPage client={new ApiClient(fake)} pollMs={60_000} />);

    expect(await screen.findByText(/total desconocido/)).toBeTruthy();
    expect(screen.getByText("Progreso porcentual no calculable con los datos disponibles.")).toBeTruthy();
    expect(screen.queryByText("Tiempo restante")).toBeNull();
  });

  it("muestra el preflight de transformación y conserva el diagnóstico recuperable", async () => {
    const fake = transport({
      status: 200,
      body: { items: [download({ phase: "RUNNING" })], next_cursor: null, snapshot_at: "2026-10-24T10:00:00.000Z" },
    });
    render(<DownloadsPage client={new ApiClient(fake)} pollMs={60_000} />);
    fireEvent.click(screen.getByRole("tab", { name: "Transformaciones" }));

    expect(await screen.findByText(/Interrumpida; se puede recuperar/)).toBeTruthy();
    expect(screen.getByText("El proceso se interrumpió.")).toBeTruthy();
    expect(screen.getByText(/Q4_K_M/)).toBeTruthy();
    expect(screen.getByText("2,0 GiB")).toBeTruthy();
    expect(screen.getByText(/calcular otro preflight/)).toBeTruthy();
  });

  it("explica que el daemon todavía no expone la cola de descargas", async () => {
    const fake = transport({
      status: 404,
      body: { code: "NOT_FOUND", message: "No existe", retryable: false, correlation_id: "c1", details: { reason: "route_missing" } },
    });
    render(<DownloadsPage client={new ApiClient(fake)} pollMs={60_000} />);

    expect((await screen.findByRole("alert")).textContent).toContain("consulta de esta cola aún no está disponible");
  });

  it("normaliza un fallo de verificación SHA-256", async () => {
    const failed = { ...download({ phase: "FAILED" }), state: "FAILED", error: { code: "CHECKSUM_MISMATCH" } };
    const fake = transport({
      status: 200,
      body: { items: [failed], next_cursor: null, snapshot_at: "2026-10-24T10:00:00.000Z" },
    });
    render(<DownloadsPage client={new ApiClient(fake)} pollMs={60_000} />);

    expect(await screen.findByText("La verificación SHA-256 no coincide.")).toBeTruthy();
  });

  it("guarda la prioridad solo tras una acción explícita y usa If-Match", async () => {
    const fake = transport({
      status: 200,
      body: { items: [download({ phase: "RUNNING" })], next_cursor: null, snapshot_at: "2026-10-24T10:00:00.000Z" },
    });
    render(<DownloadsPage client={new ApiClient(fake)} pollMs={60_000} />);

    const selector = await screen.findByLabelText(`Nueva prioridad para modelo ${variantId.slice(0, 8)}`);
    fireEvent.change(selector, { target: { value: "BACKGROUND" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar" }));

    await waitFor(() => expect(fake.calls.some((call) => call.method === "PATCH")).toBe(true));
    const request = fake.calls.find((call) => call.method === "PATCH");
    expect(request?.path).toBe(`/api/v1/downloads/${downloadId}/priority`);
    expect(request?.headers?.["If-Match"]).toBe('"1"');
    expect(request?.body).toEqual({ priority: "BACKGROUND" });
    const saveButton = screen.getByRole("button", { name: "Guardar" }) as HTMLButtonElement;
    await waitFor(() => expect(saveButton.disabled).toBe(true));
  });
});
