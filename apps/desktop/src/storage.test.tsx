import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import App from "./App";
import type { RawRequest, RawResponse, Transport } from "./api/client";
import { bytesToGib, gibToBytes, type StorageLocation } from "./storage";

afterEach(() => {
  cleanup();
  window.location.hash = "";
});

const LIVE = { status: 200, body: { status: "LIVE", schema_version: "1" } };
const READY = { status: 200, body: { status: "READY", schema_version: "1" } };
const LIST = "GET /api/v1/storage-locations?limit=200";
const GUID = "fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3";

/** API falsa solo para pruebas: cola de respuestas por ruta. */
function transport(routes: Record<string, RawResponse[]>): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const t = (async (req: RawRequest) => {
    calls.push(req);
    const key = `${req.method} ${req.path}`;
    if (key === "GET /api/v1/health/live") return LIVE;
    if (key === "GET /api/v1/health/ready") return READY;
    const queue = routes[key];
    if (!queue || queue.length === 0) throw new Error(`ruta no prevista: ${key}`);
    return queue.length > 1 ? queue.shift()! : queue[0];
  }) as Transport & { calls: RawRequest[] };
  t.calls = calls;
  return t;
}

function loc(id: string, name: string, over: Partial<StorageLocation> = {}): StorageLocation {
  return {
    id,
    row_version: 1,
    metadata: { volume_budget: { usable_bytes: "172387418112", shared_with: [] }, performance_class_source: "observed" },
    name,
    root_path: `C:\\IA\\${name}`,
    canonical_path: `C:\\IA\\${name}`,
    volume_guid: GUID,
    drive_letter: "C:",
    unc_path: null,
    filesystem: "NTFS",
    storage_type: "LOCAL",
    capacity_bytes: "1998992699392",
    free_bytes: "176682385408",
    reserved_bytes: "4294967296",
    priority: 1,
    auto_placement: true,
    state: "ONLINE",
    last_seen_at: "2026-10-11T10:00:00.000Z",
    volume_key: GUID,
    performance_class: "NVME",
    ...over,
  };
}

const page = (items: StorageLocation[]): RawResponse => ({ status: 200, body: { items, next_cursor: null, snapshot_at: "t" } });
const error = (status: number, code: string, reason: string): RawResponse => ({
  status,
  body: { code, message: code, retryable: false, correlation_id: "c", details: { reason } },
});

function open(t: Transport) {
  window.location.hash = "#/almacenamiento";
  render(<App transport={t} pollMs={60_000} />);
}

describe("reserva en GiB", () => {
  it("convierte a bytes exactos y vuelve", () => {
    expect(gibToBytes("4")).toBe("4294967296");
    expect(gibToBytes("1,5")).toBe("1610612736");
    expect(gibToBytes("0.001")).toBe("1073741");
    expect(gibToBytes("-1")).toBeNull();
    expect(gibToBytes("abc")).toBeNull();
    expect(bytesToGib("4294967296")).toBe("4");
    expect(bytesToGib("1610612736")).toBe("1,5");
  });
});

describe("pantalla Almacenamiento", () => {
  it("cancelar una edición recarga los cambios de otro actor y permite actualizar después", async () => {
    const a = loc("0192aaaa-0000-7000-8000-000000000001", "Antes");
    const b = { ...a, name: "Actualizada por API", row_version: 2 };
    const t = transport({ [LIST]: [page([a]), page([b]), page([])] });
    open(t);
    fireEvent.click(within(await screen.findByRole("article", { name: "Ubicación Antes" })).getByRole("button", { name: "Editar" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancelar" }));
    await screen.findByRole("article", { name: "Ubicación Actualizada por API" });
    expect(screen.queryByRole("article", { name: "Ubicación Antes" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Actualizar ubicaciones" }));
    await screen.findByText(/Todavía no hay ninguna ubicación/);
    expect(t.calls.filter((c) => `${c.method} ${c.path}` === LIST)).toHaveLength(3);
  });

  it("una reserva imposible conserva el formulario y explica cómo corregirla", async () => {
    open(transport({ [LIST]: [page([])], "POST /api/v1/storage-locations": [error(400, "VALIDATION_FAILED", "reservation_exceeds_capacity")] }));
    fireEvent.click(await screen.findByRole("button", { name: "Añadir ubicación" }));
    fireEvent.change(screen.getByLabelText("Nombre"), { target: { value: "Modelos" } });
    fireEvent.change(screen.getByLabelText("Carpeta"), { target: { value: "C:\\Modelos" } });
    fireEvent.change(screen.getByLabelText(/Espacio que no se debe usar/), { target: { value: "99999" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar" }));
    expect((await screen.findByRole("alert")).textContent).toContain("La reserva supera la capacidad del disco");
    expect((screen.getByLabelText("Nombre") as HTMLInputElement).value).toBe("Modelos");
  });

  it("vacía: lo dice y ofrece añadir", async () => {
    open(transport({ [LIST]: [page([])] }));
    expect(await screen.findByText(/Todavía no hay ninguna ubicación/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Añadir ubicación" })).toBeTruthy();
  });

  it("muestra estado en texto, espacio utilizable y presupuesto compartido", async () => {
    const a = loc("0192aaaa-0000-7000-8000-000000000001", "Modelos", {
      metadata: { volume_budget: { usable_bytes: "172387418112", shared_with: ["0192aaaa-0000-7000-8000-000000000002"] } },
    });
    const b = loc("0192aaaa-0000-7000-8000-000000000002", "Cache", { state: "MISSING" });
    open(transport({ [LIST]: [page([a, b])] }));
    const modelos = await screen.findByRole("article", { name: "Ubicación Modelos" });
    expect(within(modelos).getByText("Disponible")).toBeTruthy();
    expect(within(modelos).getByTestId("usable").textContent).toContain("160,5 GiB de 164,5 GiB libres");
    expect(within(modelos).getByText(/Comparte el disco con Cache/)).toBeTruthy();
    expect(within(screen.getByRole("article", { name: "Ubicación Cache" })).getByText("Falta la carpeta")).toBeTruthy();
  });

  it("añadir: bytes exactos, Idempotency-Key, error del servidor y éxito", async () => {
    const created = loc("0192aaaa-0000-7000-8000-000000000003", "Nueva");
    const t = transport({
      [LIST]: [page([]), page([created])],
      "POST /api/v1/storage-locations": [error(400, "VALIDATION_FAILED", "root_not_found"), { status: 201, body: created }],
    });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Añadir ubicación" }));
    fireEvent.change(screen.getByLabelText("Nombre"), { target: { value: "Nueva" } });
    fireEvent.change(screen.getByLabelText("Carpeta"), { target: { value: "D:\\no-existe" } });
    fireEvent.change(screen.getByLabelText(/Espacio que no se debe usar/), { target: { value: "1,5" } });
    expect(screen.getByTestId("reserved-exact").textContent).toBe("1.610.612.736 bytes");
    fireEvent.click(screen.getByRole("button", { name: "Guardar" }));
    expect((await screen.findByRole("alert")).textContent).toContain("La carpeta no existe");
    fireEvent.change(screen.getByLabelText("Carpeta"), { target: { value: "C:\\IA\\Nueva" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar" }));
    await screen.findByRole("article", { name: "Ubicación Nueva" });
    const posts = t.calls.filter((c) => c.method === "POST");
    expect(posts[1].body).toEqual({
      name: "Nueva", root_path: "C:\\IA\\Nueva", reserved_bytes: "1610612736", priority: 0, auto_placement: false, performance_class: "UNKNOWN",
    });
    expect(posts[1].headers?.["idempotency-key"]).toMatch(/^loc-/);
    expect(screen.getByText("Ubicación «Nueva» añadida.")).toBeTruthy();
  });

  it("editar con If-Match y versión vieja explicada", async () => {
    const a = loc("0192aaaa-0000-7000-8000-000000000001", "Modelos");
    const t = transport({
      [LIST]: [page([a])],
      [`PATCH /api/v1/storage-locations/${a.id}`]: [error(409, "STALE_REVISION", "stale_revision")],
    });
    open(t);
    fireEvent.click(within(await screen.findByRole("article", { name: "Ubicación Modelos" })).getByRole("button", { name: "Editar" }));
    expect((screen.getByLabelText(/Espacio que no se debe usar/) as HTMLInputElement).value).toBe("4");
    fireEvent.click(screen.getByRole("button", { name: "Guardar" }));
    expect((await screen.findByRole("alert")).textContent).toContain("Alguien cambió esta ubicación");
    expect(t.calls.find((c) => c.method === "PATCH")?.headers).toEqual({ "if-match": "1" });
  });

  it("quitar pide confirmación y explica por qué no se puede", async () => {
    const a = loc("0192aaaa-0000-7000-8000-000000000001", "Modelos");
    const t = transport({
      [LIST]: [page([a]), page([])],
      [`DELETE /api/v1/storage-locations/${a.id}`]: [error(409, "CONFLICT", "location_has_artifacts"), { status: 204, body: null }],
    });
    open(t);
    fireEvent.click(within(await screen.findByRole("article", { name: "Ubicación Modelos" })).getByRole("button", { name: "Quitar" }));
    const dialog = screen.getByRole("alertdialog");
    expect(within(dialog).getByText(/No se borra ningún fichero/)).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Quitar" }));
    expect((await within(dialog).findByRole("alert")).textContent).toContain("tiene modelos");
    fireEvent.click(within(dialog).getByRole("button", { name: "Quitar" }));
    await screen.findByText(/Todavía no hay ninguna ubicación/);
    expect(t.calls.filter((c) => c.method === "DELETE")[0].headers).toEqual({ "if-match": "1" });
  });

  it("analizar lanza el job y recarga al terminar", async () => {
    const a = loc("0192aaaa-0000-7000-8000-000000000001", "Modelos");
    const id = "0192aaaa-0000-7000-8000-0000000000aa";
    const t = transport({
      [LIST]: [page([a]), page([{ ...a, state: "MISSING" }])],
      [`POST /api/v1/storage-locations/${a.id}/probe`]: [{ status: 202, body: { job_id: id, status_url: `/api/v1/jobs/${id}`, state: "QUEUED" } }],
      [`GET /api/v1/jobs/${id}`]: [{ status: 200, body: { job_id: id, state: "COMPLETED" } }],
    });
    open(t);
    fireEvent.click(within(await screen.findByRole("article", { name: "Ubicación Modelos" })).getByRole("button", { name: "Analizar" }));
    await waitFor(() => expect(screen.getByText("Falta la carpeta")).toBeTruthy(), { timeout: 5000 });
    expect(screen.getByText("«Modelos» analizada.")).toBeTruthy();
  }, 10_000);
});
