import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import App from "./App";
import type { RawRequest, RawResponse, Transport } from "./api/client";

afterEach(() => { cleanup(); window.location.hash = ""; });
const id = "0192aaaa-0000-7000-8000-000000000003";
const item = {
  id, adapter_id: "fixture.http", version: "1.0.0", family: "source", package_hash: "a".repeat(64),
  publisher: "Publicador de prueba", trust: "UNTRUSTED", state: "INSTALLED", row_version: 1,
  manifest: {
    protocol_version: "2.0", signature: null,
    permissions: { read_roots: ["$models"], write_roots: [], network_origins: ["https://example.invalid"], process_allowlist: [], secret_refs: ["secret-canary-value"] },
  }, metadata: {},
};
function transport(): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const value = (async (request: RawRequest): Promise<RawResponse> => {
    calls.push(request);
    if (request.path.includes("/health/")) return { status: 200, body: { status: request.path.endsWith("live") ? "LIVE" : "READY", schema_version: "1" } };
    if (request.method === "GET" && request.path === "/api/v1/adapters?limit=200") return { status: 200, body: { items: [item], next_cursor: null, snapshot_at: "t" } };
    if (request.method === "GET" && request.path === `/api/v1/adapters/${id}`) return { status: 200, body: item };
    throw new Error(`unexpected ${request.method} ${request.path}`);
  }) as Transport & { calls: RawRequest[] };
  value.calls = calls;
  return value;
}

describe("pantalla Adaptadores", () => {
  it("advierte sobre paquetes sin firma y protocolo incompatible, muestra permisos y no revela referencias", async () => {
    const t = transport();
    window.location.hash = "#/adaptadores";
    render(<App transport={t} pollMs={60_000} />);
    fireEvent.click(await screen.findByRole("button", { name: "Ver detalles de fixture.http" }));
    const detail = await screen.findByRole("region", { name: "Detalle de fixture.http" });
    expect(within(detail).getByText(/Protocolo incompatible/)).toBeTruthy();
    expect(within(detail).getByText("Sin firma")).toBeTruthy();
    expect(within(detail).getByText("Publicador de prueba")).toBeTruthy();
    expect(within(detail).getByText("$models")).toBeTruthy();
    expect(within(detail).getByText("https://example.invalid")).toBeTruthy();
    expect(within(detail).getByText(/1 declarada/)).toBeTruthy();
    expect(screen.queryByText("secret-canary-value")).toBeNull();
    expect(t.calls.some((call) => call.path === `/api/v1/adapters/${id}`)).toBe(true);
  });

  it("permite reintentar si falla la lectura del catálogo", async () => {
    let fail = true;
    const t = (async (request: RawRequest): Promise<RawResponse> => {
      if (request.path.includes("/health/")) return { status: 200, body: { status: "LIVE", schema_version: "1" } };
      if (request.path === "/api/v1/adapters?limit=200" && fail) { fail = false; return { status: 0, body: null }; }
      return { status: 200, body: { items: [], next_cursor: null, snapshot_at: "t" } };
    }) as Transport;
    window.location.hash = "#/adaptadores";
    render(<App transport={t} pollMs={60_000} />);
    fireEvent.click(await screen.findByRole("button", { name: "Reintentar" }));
    expect(await screen.findByText("Todavía no hay adaptadores instalados.")).toBeTruthy();
  });

  it("permite reintentar el detalle sin perder el adaptador seleccionado", async () => {
    let details = 0;
    const t = (async (request: RawRequest): Promise<RawResponse> => {
      if (request.path.includes("/health/")) return { status: 200, body: { status: "LIVE", schema_version: "1" } };
      if (request.path === "/api/v1/adapters?limit=200") return { status: 200, body: { items: [item], next_cursor: null, snapshot_at: "t" } };
      if (request.path === `/api/v1/adapters/${id}`) {
        details++;
        return details === 1 ? { status: 0, body: null } : { status: 200, body: item };
      }
      throw new Error(`unexpected ${request.method} ${request.path}`);
    }) as Transport;
    window.location.hash = "#/adaptadores";
    render(<App transport={t} pollMs={60_000} />);
    fireEvent.click(await screen.findByRole("button", { name: "Ver detalles de fixture.http" }));
    await screen.findByRole("button", { name: "Reintentar detalle" });
    fireEvent.click(screen.getByRole("button", { name: "Reintentar detalle" }));
    expect(await screen.findByRole("region", { name: "Detalle de fixture.http" })).toBeTruthy();
    expect(details).toBe(2);
  });
});
