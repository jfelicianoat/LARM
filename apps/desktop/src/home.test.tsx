import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { HomePage } from "./home";
import { ApiClient, type RawRequest, type RawResponse, type Transport } from "./api/client";

afterEach(() => cleanup());

function transport(handler: (request: RawRequest) => RawResponse): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const value = (async (request: RawRequest) => { calls.push(request); return handler(request); }) as Transport & { calls: RawRequest[] };
  value.calls = calls;
  return value;
}

function page<T>(items: T[]): RawResponse {
  return { status: 200, body: { items, next_cursor: null } };
}

const snapshot = { status: "ONLINE" as const, error: null, checkedAt: new Date("2026-11-12T10:00:00Z") };

const variantRows = [
  { id: "v1", quantization: "Q8_0", promotion: "DEFAULT" },
  { id: "v2", quantization: "Q4_0", promotion: "CANDIDATE" },
  { id: "v3", quantization: null, promotion: "CANDIDATE" },
];
const adapterRows = [
  { id: "a1", family: "transformation", state: "VALIDATED" },
  { id: "a2", family: "transformation", state: "INSTALLED" },
  { id: "a3", family: "source", state: "VALIDATED" },
];
const locationRows = [
  { id: "l1", name: "Disco modelos", state: "ONLINE", metadata: { volume_budget: { usable_bytes: "1073741824" } } },
  { id: "l2", name: "Otro disco", state: "OFFLINE", metadata: {} },
];

describe("pantalla de inicio", () => {
  it("compone el resumen de la sesión con contadores y trabajos en curso", async () => {
    const t = transport((request) => {
      if (request.path === "/api/v1/variants?limit=200") return page(variantRows);
      if (request.path === "/api/v1/adapters?limit=200") return page(adapterRows);
      if (request.path === "/api/v1/storage-locations?limit=200") return page(locationRows);
      if (request.path === "/api/v1/transformations?limit=200") return page([{ id: "tj", state: "RUNNING", adapter_id: "product.transformation.gguf" }]);
      if (request.path === "/api/v1/downloads?limit=200") return page([{ id: "dj", state: "QUEUED", adapter_id: null }]);
      throw new Error(`unexpected ${request.path}`);
    });
    render(<HomePage client={new ApiClient(t)} snapshot={snapshot} />);

    expect(await screen.findByText(/^3 variante(s)? registrada(s)?\./)).toBeTruthy();
    expect(await screen.findByText(/Candidata: 2 · Predeterminada: 1/)).toBeTruthy();
    expect(screen.getByText("Con cuantización registrada: 2")).toBeTruthy();
    expect(await screen.findByText("Cualificados (VALIDATED): 2")).toBeTruthy();
    expect(screen.getByText(/Presupuesto usable/)).toBeTruthy();
    expect(screen.getByText(/Transformación \(RUNNING\) · product\.transformation\.gguf/)).toBeTruthy();
    expect(screen.getByText(/Descarga \(QUEUED\)/)).toBeTruthy();
    // Enlaces a las secciones (diagnóstico usa su propio texto).
    for (const href of ["#/mis-modelos", "#/adaptadores", "#/almacenamiento", "#/descargas", "#/diagnostico"]) {
      expect(document.querySelector(`a[href="${href}"]`)).toBeTruthy();
    }
  });

  it("degrada sólo la tarjeta cuyo recurso falla", async () => {
    const t = transport((request) => {
      if (request.path === "/api/v1/variants?limit=200") return { status: 500, body: { code: "INTERNAL", message: "boom", retryable: false, correlation_id: "c", details: {} } };
      if (request.path === "/api/v1/adapters?limit=200") return page(adapterRows);
      if (request.path === "/api/v1/storage-locations?limit=200") return page([]);
      if (request.path === "/api/v1/transformations?limit=200") return page([]);
      if (request.path === "/api/v1/downloads?limit=200") return page([]);
      throw new Error(`unexpected ${request.path}`);
    });
    render(<HomePage client={new ApiClient(t)} snapshot={snapshot} />);

    expect(await screen.findByText("No se pudieron cargar las variantes.")).toBeTruthy();
    expect(await screen.findByText(/3 instalados/)).toBeTruthy();
    expect(screen.getByText("Sin descargas ni transformaciones en curso.")).toBeTruthy();
  });

  it("muestra vacíos cuando no hay datos", async () => {
    const t = transport((request) => {
      if (request.path === "/api/v1/variants?limit=200") return page([]);
      if (request.path === "/api/v1/adapters?limit=200") return page([]);
      if (request.path === "/api/v1/storage-locations?limit=200") return page([]);
      if (request.path === "/api/v1/transformations?limit=200") return page([]);
      if (request.path === "/api/v1/downloads?limit=200") return page([]);
      throw new Error(`unexpected ${request.path}`);
    });
    render(<HomePage client={new ApiClient(t)} snapshot={snapshot} />);

    expect(await screen.findByText("Sin descargas ni transformaciones en curso.")).toBeTruthy();
    expect(await screen.findByText("Cualificados (VALIDATED): 0")).toBeTruthy();
  });
});
