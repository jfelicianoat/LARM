import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DiscoverPage, buildSearchRequest } from "./discover";
import { ApiClient, type RawRequest, type RawResponse, type Transport } from "./api/client";

afterEach(() => cleanup());

function transport(handler: (request: RawRequest) => RawResponse | Promise<RawResponse>): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const value = (async (request: RawRequest) => { calls.push(request); return handler(request); }) as Transport & { calls: RawRequest[] };
  value.calls = calls;
  return value;
}

function candidate(id: string, overrides: Record<string, unknown> = {}) {
  return {
    model: { id: `model-${id}`, name: `Modelo ${id}`, capabilities: [] },
    source_id: "huggingface",
    source_priority: 1,
    variant_id: id,
    author: "Autor",
    capabilities: [],
    format: "GGUF",
    quantization: "Q8_0",
    architecture: "llama",
    parameter_count: "7000000000",
    size_bytes: "4000000000",
    size_confidence: "ESTIMATED",
    licenses: ["MIT"],
    technical_fit: 78,
    trust_score: 90,
    ...overrides,
  };
}

describe("pantalla descubrir", () => {
  it("envía el DTO Search completo con los filtros y renderiza los resultados", async () => {
    const t = transport((request) => {
      if (request.method === "POST" && request.path === "/api/v1/models/search") {
        expect(request.body).toMatchObject({
          q: "llama",
          capabilities: [],
          source_ids: [],
          compatibility: "ALL",
          architectures: [],
          formats: ["GGUF"],
          quantizations: ["Q8_0"],
          licenses: [],
          runtime_ids: [],
          parameter_count: { minimum: null, maximum: null },
          size_bytes: { minimum: null, maximum: null },
          trust_score: { minimum: null, maximum: null },
          certification_statuses: [],
          only_executable: false,
          only_recommended: false,
          show_incompatible: true,
          cursor: null,
          limit: 50,
        });
        return {
          status: 201,
          body: {
            items: [candidate("a1")],
            next_cursor: null,
            metadata: { search: { warnings: [{ source_id: "huggingface", reason: "límite de tasa", limitations: ["30 s"] }] } },
          },
        };
      }
      throw new Error(`unexpected ${request.method} ${request.path}`);
    });
    const navigate = vi.fn();
    render(<DiscoverPage client={new ApiClient(t)} navigate={navigate} />);

    fireEvent.change(screen.getByLabelText("Texto"), { target: { value: "llama" } });
    fireEvent.change(screen.getByLabelText("Formato"), { target: { value: "GGUF" } });
    fireEvent.change(screen.getByLabelText("Cuantización"), { target: { value: "Q8_0" } });
    fireEvent.click(screen.getByRole("button", { name: "Buscar" }));

    expect(await screen.findByText("Modelo a1")).toBeTruthy();
    expect(screen.getByText(/huggingface \(prioridad 1\)/)).toBeTruthy();
    expect(screen.getByText(/GGUF · Q8_0 · llama/)).toBeTruthy();
    expect(screen.getByText(/ajuste técnico 78%/)).toBeTruthy();
    expect(screen.getByText(/Advertencia de la fuente huggingface: límite de tasa/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Ver en Mis modelos" }));
    expect(navigate).toHaveBeenCalledWith("mis-modelos", "a1");
  });

  it("pagina con el cursor acumulando resultados", async () => {
    let page = 0;
    const t = transport((request) => {
      if (request.method === "POST" && request.path === "/api/v1/models/search") {
        page += 1;
        expect(request.body).toMatchObject({ cursor: page === 1 ? null : "cursor-1" });
        return {
          status: 201,
          body: page === 1
            ? { items: [candidate("a1")], next_cursor: "cursor-1" }
            : { items: [candidate("a2")], next_cursor: null },
        };
      }
      throw new Error(`unexpected ${request.method} ${request.path}`);
    });
    render(<DiscoverPage client={new ApiClient(t)} navigate={() => undefined} />);

    fireEvent.click(screen.getByRole("button", { name: "Buscar" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cargar más resultados" }));

    expect(await screen.findByText("Modelo a2")).toBeTruthy();
    expect(screen.getByText("Modelo a1")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Cargar más resultados" })).toBeNull();
  });

  it("muestra el vacío y el error de contrato", async () => {
    const t = transport((request) => {
      if (request.method === "POST" && request.path === "/api/v1/models/search") {
        return { status: 422, body: { code: "VALIDATION_FAILED", message: "Search.limit: fuera de rango", retryable: false, correlation_id: "c1", details: { reason: "search_invalid" } } };
      }
      throw new Error(`unexpected ${request.method} ${request.path}`);
    });
    render(<DiscoverPage client={new ApiClient(t)} navigate={() => undefined} />);

    fireEvent.click(screen.getByRole("button", { name: "Buscar" }));
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.getByText(/La búsqueda no se pudo completar/)).toBeTruthy();
  });
});

describe("buildSearchRequest", () => {
  it("normaliza el texto y los filtros según el contrato", () => {
    const request = buildSearchRequest({ q: "  gpt  ", formats: ["GGUF"], quantizations: [], compatibility: "COMPATIBLE", limit: 25, cursor: "c" });
    expect(request).toMatchObject({ q: "gpt", formats: ["GGUF"], quantizations: [], compatibility: "COMPATIBLE", limit: 25, cursor: "c" });
  });
});
