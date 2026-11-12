import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { ApiClient, ApiError, toApiError, validatePath, type RawRequest, type RawResponse, type Transport } from "./api/client";
import { probe } from "./api/status";
import { SCREENS } from "./screens";
import { runSmoke } from "./smoke";

afterEach(() => {
  cleanup();
  window.location.hash = "";
});

/** API falsa solo para pruebas: responde según el mapa o lanza (offline). */
function fakeTransport(routes: Record<string, RawResponse | "offline">): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const t = (async (req: RawRequest) => {
    calls.push(req);
    const r = routes[`${req.method} ${req.path}`];
    if (r === "offline" || r === undefined) throw new Error("no se pudo abrir el pipe de bootstrap");
    return r;
  }) as Transport & { calls: RawRequest[] };
  t.calls = calls;
  return t;
}

const LIVE = { status: 200, body: { status: "LIVE", schema_version: "1" } };
const READY = { status: 200, body: { status: "READY", schema_version: "1" } };
const online = () => fakeTransport({ "GET /api/v1/health/live": LIVE, "GET /api/v1/health/ready": READY });

describe("cliente", () => {
  it("convierte el envelope del contrato en ApiError sin inventar campos", () => {
    const e = toApiError({
      status: 403,
      body: { code: "FORBIDDEN", message: "Falta el scope", retryable: false, correlation_id: "c1", details: { reason: "scope_missing" } },
    });
    expect([e.code, e.status, e.retryable, e.correlationId, e.reason]).toEqual(["FORBIDDEN", 403, false, "c1", "scope_missing"]);
    const odd = toApiError({ status: 502, body: "<html>" });
    expect(odd.code).toBe("UNEXPECTED_RESPONSE");
  });

  it("rechaza rutas fuera de /api/v1 antes de salir del frontend", async () => {
    const t = online();
    const client = new ApiClient(t);
    for (const bad of ["/admin", "http://evil/api/v1/x", "/api/v1/../../etc", "api/v1/x"]) {
      expect(() => validatePath(bad)).toThrow(ApiError);
      await expect(client.get(bad)).rejects.toThrow(ApiError);
    }
    expect(t.calls).toHaveLength(0);
  });

  it("un fallo de transporte es DAEMON_UNREACHABLE reintentable", async () => {
    const client = new ApiClient(fakeTransport({}));
    const err = await client.get("/api/v1/health/live").catch((e: ApiError) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect([(err as ApiError).isOffline, (err as ApiError).retryable]).toEqual([true, true]);
  });
});

describe("estado del daemon", () => {
  it("ONLINE, DEGRADED y OFFLINE según live/ready", async () => {
    expect((await probe(new ApiClient(online()))).status).toBe("ONLINE");
    const degraded = fakeTransport({ "GET /api/v1/health/live": LIVE, "GET /api/v1/health/ready": { status: 200, body: { status: "DEGRADED", schema_version: "1" } } });
    expect((await probe(new ApiClient(degraded))).status).toBe("DEGRADED");
    const noRead = fakeTransport({
      "GET /api/v1/health/live": LIVE,
      "GET /api/v1/health/ready": { status: 503, body: { code: "AUTH_BACKEND_UNAVAILABLE", message: "x", retryable: true, correlation_id: "c" } },
    });
    const s = await probe(new ApiClient(noRead));
    expect([s.status, s.error?.code]).toEqual(["DEGRADED", "AUTH_BACKEND_UNAVAILABLE"]);
    expect((await probe(new ApiClient(fakeTransport({})))).status).toBe("OFFLINE");
  });
});

describe("shell", () => {
  it("muestra las 14 pantallas y el daemon en línea", async () => {
    render(<App transport={online()} />);
    const nav = screen.getByRole("navigation", { name: "Secciones" });
    const links = within(nav).getAllByRole("link");
    expect(links.map((l) => l.textContent)).toEqual(SCREENS.map((s) => s.title));
    await waitFor(() => expect(screen.getByTestId("daemon-status").dataset.status).toBe("ONLINE"));
    expect(screen.getByRole("heading", { level: 1, name: "Inicio" })).toBeTruthy();
    // La pantalla de inicio ya tiene contenido propio (dashboard de sesión).
    expect(screen.getByRole("heading", { name: "Estado del daemon" })).toBeTruthy();
  });

  it("sin daemon: estado OFFLINE, pantalla offline y diagnóstico con el error", async () => {
    render(<App transport={fakeTransport({})} />);
    await waitFor(() => expect(screen.getByTestId("daemon-status").dataset.status).toBe("OFFLINE"));
    expect(screen.getByRole("heading", { name: "No hay conexión con el daemon de LARM" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Ver diagnóstico" }));
    await waitFor(() => expect(screen.getByRole("heading", { level: 1, name: "Diagnóstico de conexión" })).toBeTruthy());
    expect(screen.getByTestId("diag-error").textContent).toContain("DAEMON_UNREACHABLE");
  });

  it("pasa de offline a online y avisa sin bloquear", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const routes: Record<string, RawResponse | "offline"> = {};
    const t = fakeTransport(routes);
    render(<App transport={t} pollMs={1000} />);
    await waitFor(() => expect(screen.getByTestId("daemon-status").dataset.status).toBe("OFFLINE"));
    routes["GET /api/v1/health/live"] = LIVE;
    routes["GET /api/v1/health/ready"] = READY;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1100);
    });
    await waitFor(() => expect(screen.getByTestId("daemon-status").dataset.status).toBe("ONLINE"));
    expect(screen.getByRole("status").textContent).toContain("restablecida");
    vi.useRealTimers();
  });

  it("navegar mueve el foco al título de la pantalla", async () => {
    render(<App transport={online()} />);
    act(() => {
      window.location.hash = "#/hardware";
      window.dispatchEvent(new HashChangeEvent("hashchange"));
    });
    const h1 = await screen.findByRole("heading", { level: 1, name: "Hardware" });
    await waitFor(() => expect(document.activeElement).toBe(h1));
    expect(screen.getByRole("link", { name: "Hardware" }).getAttribute("aria-current")).toBe("page");
  });

  it("abre la cola P125 desde la navegación y consulta ambos listados", async () => {
    const emptyPage = { status: 200, body: { items: [], next_cursor: null, snapshot_at: "2026-10-24T10:00:00.000Z" } };
    const t = fakeTransport({
      "GET /api/v1/health/live": LIVE,
      "GET /api/v1/health/ready": READY,
      "GET /api/v1/downloads?limit=200": emptyPage,
      "GET /api/v1/transformations?limit=200": emptyPage,
    });
    render(<App transport={t} />);
    fireEvent.click(screen.getByRole("link", { name: "Descargas" }));

    expect(await screen.findByRole("heading", { level: 2, name: "Cola de descargas" })).toBeTruthy();
    expect(screen.getByRole("status").textContent).toContain("No hay descargas");
    expect(t.calls.some((request) => request.path === "/api/v1/downloads?limit=200")).toBe(true);
    expect(t.calls.some((request) => request.path === "/api/v1/transformations?limit=200")).toBe(true);
  });
});

describe("prueba de humo", () => {
  it("solo actúa con smoke activado y entrega ping + salud por el backend", async () => {
    const reports: string[] = [];
    const invoke = (async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "smoke_enabled") return true;
      if (cmd === "ping") return "pong";
      if (cmd === "smoke_report") reports.push(String(args?.result));
      return undefined;
    }) as <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
    expect(await runSmoke(invoke, online())).toBe(true);
    expect(JSON.parse(reports[0])).toEqual({ ping: "pong", live: LIVE.body, ready: READY.body });
    const off = (async (cmd: string) => (cmd === "smoke_enabled" ? false : undefined)) as typeof invoke;
    expect(await runSmoke(off, online())).toBe(false);
  });
});
