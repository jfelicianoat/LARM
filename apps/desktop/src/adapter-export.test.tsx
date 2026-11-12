import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { AdapterExport } from "./adapter-export";
import { ApiClient, type RawRequest, type RawResponse, type Transport } from "./api/client";

afterEach(() => cleanup());

function transport(): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const value = (async (request: RawRequest): Promise<RawResponse> => {
    calls.push(request);
    if (request.method === "POST" && request.path === "/api/v1/adapters/export") {
      return { status: 201, body: { resource_id: "0192aaaa-0000-7000-8000-000000000003", row_version: 2, event_id: "0192aaaa-0000-7000-8000-000000000022" } };
    }
    throw new Error(`unexpected ${request.method} ${request.path}`);
  }) as Transport & { calls: RawRequest[] };
  value.calls = calls;
  return value;
}

describe("exportación local de adaptadores", () => {
  it("requiere confirmación explícita antes de escribir el ZIP y muestra el resultado", async () => {
    const t = transport();
    render(<AdapterExport client={new ApiClient(t)} adapterId="0192aaaa-0000-7000-8000-000000000003" adapterName="fixture.http 1.0.0" fingerprintPackage={async () => ({
      size_bytes: "42", sha256: "b".repeat(64), manifest: {
        id: "fixture.http", version: "1.0.0", family: "source", protocol_version: "1.0", publisher: "Equipo local", signature_declared: false,
        permissions: { read_roots: [], write_roots: [], network_origins: [], process_allowlist: [], secret_refs: [] },
      },
    })} />);
    fireEvent.change(screen.getByLabelText("Guardar ZIP en"), { target: { value: "C:\\Exportaciones\\fixture.zip" } });
    const button = screen.getByRole("button", { name: "Exportar ZIP" });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    expect(t.calls).toHaveLength(0);

    fireEvent.click(screen.getByRole("checkbox", { name: "Confirmo exportar fixture.http 1.0.0 a esta ruta." }));
    fireEvent.click(button);
    expect(await screen.findByText(/Paquete exportado/)).toBeTruthy();
    expect(await screen.findByText("b".repeat(64))).toBeTruthy();
    expect(screen.getByText(/42 bytes/)).toBeTruthy();
    expect(t.calls[0]).toMatchObject({
      method: "POST",
      path: "/api/v1/adapters/export",
      body: { adapter_installation_id: "0192aaaa-0000-7000-8000-000000000003", destination_path: "C:\\Exportaciones\\fixture.zip" },
    });
    expect(t.calls[0].headers?.["Idempotency-Key"]).toMatch(/^adapter-export-/);
  });
});
