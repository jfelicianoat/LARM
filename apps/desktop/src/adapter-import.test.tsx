import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdapterImport, type PackageFingerprinter } from "./adapter-import";
import { ApiClient, type RawRequest, type RawResponse, type Transport } from "./api/client";

afterEach(() => cleanup());

function transport(handler: (request: RawRequest) => RawResponse | Promise<RawResponse>): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const value = (async (request: RawRequest) => { calls.push(request); return handler(request); }) as Transport & { calls: RawRequest[] };
  value.calls = calls;
  return value;
}

const fingerprint: PackageFingerprinter = async () => ({
  size_bytes: "1234", sha256: "a".repeat(64),
  manifest: {
    id: "fixture.local", version: "1.2.3", family: "source", protocol_version: "1.0", publisher: "Equipo local", signature_declared: false,
    permissions: { read_roots: ["$models"], write_roots: [], network_origins: ["https://example.invalid"], process_allowlist: [], secret_refs: ["reference"] },
  },
});

describe("importación local de adaptadores", () => {
  it("muestra tamaño exacto y SHA-256 y espera confirmación antes de crear el job", async () => {
    const jobId = "0192aaaa-0000-7000-8000-000000000021";
    const t = transport((request) => {
      if (request.method === "POST" && request.path === "/api/v1/adapters/import") return { status: 202, body: { job_id: jobId, status_url: `/api/v1/jobs/${jobId}`, state: "QUEUED" } };
      if (request.method === "GET" && request.path === `/api/v1/jobs/${jobId}`) return { status: 200, body: { job_id: jobId, status_url: `/api/v1/jobs/${jobId}`, state: "COMPLETED", progress: {}, result: {}, error: null } };
      throw new Error(`unexpected ${request.method} ${request.path}`);
    });
    const completed = vi.fn();
    render(<AdapterImport client={new ApiClient(t)} onCompleted={completed} fingerprintPackage={fingerprint} />);

    fireEvent.change(screen.getByLabelText("Ruta del paquete ZIP"), { target: { value: "C:\\Paquetes\\adaptador.zip" } });
    fireEvent.click(screen.getByRole("button", { name: "Revisar paquete" }));
    expect(await screen.findByText(/Exacto/)).toBeTruthy();
    expect(screen.getByText(/1234 bytes/)).toBeTruthy();
    expect(screen.getByText("a".repeat(64))).toBeTruthy();
    expect(screen.getByText("fixture.local")).toBeTruthy();
    expect(screen.getByText("Equipo local")).toBeTruthy();
    expect(screen.getByText("https://example.invalid")).toBeTruthy();
    expect(screen.getByText(/1 referencia declarada/)).toBeTruthy();
    expect(t.calls.some((call) => call.path === "/api/v1/adapters/import")).toBe(false);

    const confirm = screen.getByRole("checkbox", { name: "Confirmo iniciar la importación de este paquete." });
    fireEvent.click(confirm);
    fireEvent.click(screen.getByRole("button", { name: "Importar adaptador" }));
    expect(await screen.findByText("Completada")).toBeTruthy();
    expect(t.calls.find((call) => call.path === "/api/v1/adapters/import")).toMatchObject({
      method: "POST",
      body: { package_path: "C:\\Paquetes\\adaptador.zip", expected_hash: "a".repeat(64), approval_id: null },
    });
    expect(t.calls.find((call) => call.path === "/api/v1/adapters/import")?.headers?.["Idempotency-Key"]).toMatch(/^adapter-import-/);
    expect(completed).toHaveBeenCalledOnce();
  });

  it("descarta la huella y la confirmación cuando cambia la ruta", async () => {
    const t = transport(() => ({ status: 500, body: null }));
    render(<AdapterImport client={new ApiClient(t)} onCompleted={() => undefined} fingerprintPackage={fingerprint} />);
    const path = screen.getByLabelText("Ruta del paquete ZIP");
    fireEvent.change(path, { target: { value: "C:\\Paquetes\\uno.zip" } });
    fireEvent.click(screen.getByRole("button", { name: "Revisar paquete" }));
    await screen.findByText(/Exacto/);
    fireEvent.click(screen.getByRole("checkbox", { name: "Confirmo iniciar la importación de este paquete." }));
    fireEvent.change(path, { target: { value: "C:\\Paquetes\\dos.zip" } });
    expect(screen.queryByText("a".repeat(64))).toBeNull();
    expect(screen.queryByRole("checkbox")).toBeNull();
    expect(t.calls.some((call) => call.path === "/api/v1/adapters/import")).toBe(false);
  });

  it("mantiene el formulario bloqueado si la huella no se puede leer", async () => {
    render(<AdapterImport client={new ApiClient(transport(() => ({ status: 200, body: {} })))} onCompleted={() => undefined} fingerprintPackage={async () => { throw new Error("denied"); }} />);
    fireEvent.change(screen.getByLabelText("Ruta del paquete ZIP"), { target: { value: "C:\\privado\\adaptador.zip" } });
    fireEvent.click(screen.getByRole("button", { name: "Revisar paquete" }));
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Importar adaptador" })).toBeNull();
  });
});
