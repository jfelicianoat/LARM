import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import App from "./App";
import type { RawRequest, RawResponse, Transport } from "./api/client";
import type { ModelSource } from "./sources";

afterEach(() => { cleanup(); window.location.hash = ""; });
const LIST = "GET /api/v1/sources?limit=200";
const ADAPTERS = "GET /api/v1/adapters?limit=200";
const sourceId = "0192aaaa-0000-7000-8000-000000000001";
const bindingId = "0192aaaa-0000-7000-8000-000000000002";
const installationId = "0192aaaa-0000-7000-8000-000000000003";
const accountId = "0192aaaa-0000-7000-8000-000000000004";
const page = (items: unknown[], next_cursor: string | null = null): RawResponse => ({ status: 200, body: { items, next_cursor, snapshot_at: "t" } });
const adapters = (state = "INSTALLED", protocol = "1.0") => page([{ id: installationId, adapter_id: "fixture.http", version: "1.0.0", family: "source", state, trust: "UNTRUSTED", manifest: { protocol_version: protocol }, metadata: { source_bindings: [{ id: bindingId, source_type: "HTTP" }] } }]);
const source = (over: Partial<ModelSource> = {}): ModelSource => ({ id: sourceId, name: "Repositorio", row_version: 1, adapter_id: bindingId, base_url: "https://example.invalid/models", priority: 1, enabled: false, auth_mode: "NONE", account_id: null, policy: { vendor: { unknown: [null, true] } }, metadata: {}, created_at: "t", updated_at: "t", operational: { status: "UNKNOWN", last_sync_at: null, rate_limit: null }, ...over });
const failure = (status: number, reason: string): RawResponse => ({ status, body: { code: "CONFLICT", message: "fixture", retryable: false, correlation_id: "c", details: { reason } } });
function transport(routes: Record<string, RawResponse[]>): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const t = (async (request: RawRequest) => {
    calls.push(request);
    if (request.path.includes("/health/")) return { status: 200, body: { status: request.path.endsWith("live") ? "LIVE" : "READY", schema_version: "1" } };
    const key = `${request.method} ${request.path}`;
    const queue = routes[key];
    if (!queue?.length) throw new Error(`unexpected ${key}`);
    return queue.length > 1 ? queue.shift()! : queue[0];
  }) as Transport & { calls: RawRequest[] };
  t.calls = calls; return t;
}
function open(t: Transport) { window.location.hash = "#/fuentes"; render(<App transport={t} pollMs={60_000} />); }
async function add() {
  const button = await screen.findByRole("button", { name: "Añadir fuente" });
  await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(button);
  fireEvent.change(screen.getByLabelText("Nombre"), { target: { value: "Nueva" } });
  fireEvent.change(screen.getByLabelText("Conector"), { target: { value: bindingId } });
  fireEvent.change(screen.getByLabelText("Dirección o carpeta"), { target: { value: "https://example.invalid/models" } });
}
describe("pantalla Fuentes", () => {
  it("separa configuración y estado operativo y no inventa observaciones ausentes", async () => {
    const t = transport({ [LIST]: [page([source()])], [ADAPTERS]: [adapters()] });
    open(t);
    const card = await screen.findByRole("article", { name: "Fuente Repositorio" });
    expect(within(card).getByText("Desactivada")).toBeTruthy();
    expect(within(card).getByText("Desconocido")).toBeTruthy();
    expect(within(card).getByText("Sin observación de límite")).toBeTruthy();
    expect(within(card).getByText("Sin sincronizaciones registradas")).toBeTruthy();
  });
  it("muestra un rate limit medido y la hora de sincronización sin mezclarlo con enabled", async () => {
    const observed = source({ enabled: true, operational: {
      status: "RATE_LIMITED", last_sync_at: "2026-10-25T12:00:00.000Z",
      rate_limit: { limit: "120", remaining: "0", reset_at: "2026-10-25T12:05:00.000Z", retry_after_seconds: "60" },
    } });
    const t = transport({ [LIST]: [page([observed])], [ADAPTERS]: [adapters()] });
    open(t);
    const card = await screen.findByRole("article", { name: "Fuente Repositorio" });
    expect(within(card).getByText("Activada")).toBeTruthy();
    expect(within(card).getByText("Limitada por el proveedor")).toBeTruthy();
    expect(within(card).getByText(/Límite: 120 · Disponibles: 0/)).toBeTruthy();
    expect(within(card).getByText(/Espera: 60 s/)).toBeTruthy();
    expect(within(card).getByText(/12:00:00/)).toBeTruthy();
  });
  it("vacío sin conectores explica el siguiente paso y no permite un alta inválida", async () => {
    open(transport({ [LIST]: [page([])], [ADAPTERS]: [page([])] }));
    await screen.findByText(/Todavía no hay ninguna fuente/);
    expect(screen.getByText(/No hay conectores de fuentes compatibles y validados/)).toBeTruthy();
    expect((screen.getByRole("button", { name: "Añadir fuente" }) as HTMLButtonElement).disabled).toBe(true);
  });
  it("usa el binding real y conserva la clave de alta al reintentar una respuesta perdida", async () => {
    const t = transport({ [LIST]: [page([]), page([source()])], [ADAPTERS]: [adapters()], "POST /api/v1/sources": [{ status: 0, body: null }, { status: 201, body: source() }] });
    open(t); await add();
    expect((screen.getByLabelText("Activar fuente") as HTMLInputElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Guardar fuente" }));
    await screen.findByText(/Se perdió la conexión con LARM/);
    expect((screen.getByLabelText("Nombre") as HTMLInputElement).value).toBe("Nueva");
    fireEvent.click(screen.getByRole("button", { name: "Guardar fuente" }));
    await screen.findByRole("article", { name: "Fuente Repositorio" });
    const posts = t.calls.filter((c) => c.method === "POST");
    expect(posts).toHaveLength(2);
    expect(posts[0].body).toEqual({ name: "Nueva", adapter_id: bindingId, base_url: "https://example.invalid/models", priority: 1, enabled: false, auth_mode: "NONE", account_id: null, policy: {} });
    expect(posts[0].headers?.["Idempotency-Key"]).toBeTruthy();
    expect(posts[1].headers).toEqual(posts[0].headers);
  });
  it("un cambio de formulario tras error recibe otra clave y permite habilitar sólo un conector validado", async () => {
    const t = transport({ [LIST]: [page([])], [ADAPTERS]: [adapters("VALIDATED")], "POST /api/v1/sources": [failure(400, "invalid")] });
    open(t); await add();
    const enabled = screen.getByLabelText("Activar fuente") as HTMLInputElement;
    expect(enabled.disabled).toBe(false); fireEvent.click(enabled);
    fireEvent.click(screen.getByRole("button", { name: "Guardar fuente" })); await screen.findByRole("alert");
    fireEvent.change(screen.getByLabelText("Nombre"), { target: { value: "Otra" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar fuente" }));
    await waitFor(() => expect(t.calls.filter((c) => c.method === "POST")).toHaveLength(2));
    const posts = t.calls.filter((c) => c.method === "POST");
    expect(posts[1].headers).not.toEqual(posts[0].headers);
    expect((posts[1].body as ModelSource).enabled).toBe(true);
  });
  it("editar conserva política y cuenta, usa If-Match y mantiene el borrador ante una revisión obsoleta", async () => {
    const original = source({ account_id: installationId });
    const t = transport({ [LIST]: [page([original])], [ADAPTERS]: [adapters()], [`PATCH /api/v1/sources/${sourceId}`]: [failure(409, "stale_revision")] });
    open(t);
    fireEvent.click(within(await screen.findByRole("article", { name: "Fuente Repositorio" })).getByRole("button", { name: "Editar" }));
    fireEvent.change(screen.getByLabelText("Nombre"), { target: { value: "Mi edición" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar fuente" }));
    await screen.findByText(/Esta fuente ha cambiado/);
    expect((screen.getByLabelText("Nombre") as HTMLInputElement).value).toBe("Mi edición");
    const patch = t.calls.find((c) => c.method === "PATCH")!;
    expect(patch.headers).toEqual({ "If-Match": "1" });
    expect((patch.body as ModelSource).policy).toEqual(original.policy);
    expect((patch.body as ModelSource).account_id).toBe(installationId);
    expect(t.calls.filter((c) => c.method === "PATCH")).toHaveLength(1);
  });
  it("cancelar recarga la revisión de otro actor", async () => {
    const t = transport({ [LIST]: [page([source()]), page([source({ name: "Cambio externo", row_version: 2 })])], [ADAPTERS]: [adapters()] });
    open(t); fireEvent.click(within(await screen.findByRole("article", { name: "Fuente Repositorio" })).getByRole("button", { name: "Editar" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancelar" }));
    await screen.findByRole("article", { name: "Fuente Cambio externo" });
  });
  it("eliminar exige confirmación, envía la revisión y explica referencias sin ocultar la fuente", async () => {
    const t = transport({ [LIST]: [page([source()])], [ADAPTERS]: [adapters()], [`DELETE /api/v1/sources/${sourceId}`]: [failure(409, "source_in_use")] });
    open(t); fireEvent.click(within(await screen.findByRole("article", { name: "Fuente Repositorio" })).getByRole("button", { name: "Eliminar" }));
    expect(t.calls.some((c) => c.method === "DELETE")).toBe(false);
    expect(screen.getByText(/Los modelos y archivos descargados se conservan/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Eliminar configuración" }));
    await screen.findByText(/Esta fuente tiene modelos, cuentas o seguimientos asociados/);
    expect(screen.getByRole("article", { name: "Fuente Repositorio" })).toBeTruthy();
    expect(t.calls.find((c) => c.method === "DELETE")).toEqual({ method: "DELETE", path: `/api/v1/sources/${sourceId}`, headers: { "If-Match": "1" } });
  });
  it("lista todas las páginas de conectores y carga más fuentes con el cursor", async () => {
    const t = transport({ [LIST]: [page([source()], "source-next")], [ADAPTERS]: [page([], "adapter-next")], "GET /api/v1/adapters?limit=200&cursor=adapter-next": [adapters()], "GET /api/v1/sources?limit=200&cursor=source-next": [page([source({ id: installationId, name: "Segunda" })])] });
    open(t); const more = await screen.findByRole("button", { name: "Cargar más fuentes" });
    await waitFor(() => expect((more as HTMLButtonElement).disabled).toBe(false)); fireEvent.click(more);
    await screen.findByRole("article", { name: "Fuente Segunda" });
    expect(screen.getByRole("article", { name: "Fuente Repositorio" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Cargar más fuentes" })).toBeNull();
    expect(t.calls.some((c) => c.path.endsWith("cursor=adapter-next"))).toBe(true);
  });
  it("filtra con la API y muestra permisos y un reintento disponible", async () => {
    const t = transport({ [LIST]: [page([])], [ADAPTERS]: [adapters()], "GET /api/v1/sources?limit=200&q=nombre": [failure(403, "scope_missing")] });
    open(t); await screen.findByText(/Todavía no hay ninguna fuente/);
    fireEvent.change(screen.getByLabelText("Buscar por nombre o dirección"), { target: { value: "nombre" } });
    fireEvent.click(screen.getByRole("button", { name: "Buscar fuentes" }));
    await screen.findByText(/Tu cuenta no tiene permiso/);
    expect((screen.getByRole("button", { name: "Actualizar fuentes" }) as HTMLButtonElement).disabled).toBe(false);
  });
  it("guarda credenciales Basic por la ruta protegida y limpia ambos campos al terminar", async () => {
    const privateSource = source({ auth_mode: "BASIC", account_id: accountId });
    const account = { id: accountId, source_id: sourceId, credential_reference_id: null, display_name: "Cuenta de pruebas", accepted_license_hashes: [], row_version: 1 };
    const t = transport({ [LIST]: [page([privateSource])], [ADAPTERS]: [adapters()],
      "GET /api/v1/source-accounts?limit=200": [page([account])],
      [`POST /api/v1/sources/${sourceId}/basic-credential`]: [{ status: 201, body: { id: installationId } }] });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Gestionar acceso" }));
    await screen.findByRole("option", { name: "Cuenta de pruebas" });
    fireEvent.change(screen.getByLabelText("Usuario"), { target: { value: "private-user" } });
    fireEvent.change(screen.getByLabelText("Contraseña"), { target: { value: "private-password" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar credencial" }));
    await screen.findByRole("status", { name: "" });
    await waitFor(() => expect((screen.getByLabelText("Usuario") as HTMLInputElement).value).toBe(""));
    expect((screen.getByLabelText("Contraseña") as HTMLInputElement).value).toBe("");
    const saved = t.calls.find((call) => call.path.endsWith("/basic-credential"))!;
    expect(saved.body).toEqual({ source_id: sourceId, account_id: accountId, username: "private-user", password: "private-password" });
    expect(saved.headers).toBeUndefined();
    expect(screen.getByText("Credencial guardada en el almacén protegido.")).toBeTruthy();
    expect(screen.getByText(/Credencial de esta cuenta:/)).toBeTruthy();
    expect(screen.getByLabelText("credencial oculta").textContent).toBe("••••••••");
    expect(screen.queryByText(installationId)).toBeNull();
    expect(screen.queryByText("private-password")).toBeNull();
  });
  it("no ofrece un conector con protocolo incompatible", async () => {
    open(transport({ [LIST]: [page([])], [ADAPTERS]: [adapters("VALIDATED", "2.0")] }));
    await screen.findByText(/No hay conectores de fuentes compatibles y validados/);
    expect((screen.getByRole("button", { name: "Añadir fuente" }) as HTMLButtonElement).disabled).toBe(true);
  });
  it("crea una cuenta, la vincula con revisión y no habilita Basic hasta terminar el vínculo", async () => {
    const unlinked = source({ auth_mode: "BASIC" });
    const created = { id: accountId, source_id: sourceId, credential_reference_id: null, display_name: "Cuenta local", accepted_license_hashes: [], row_version: 1 };
    const linked = source({ auth_mode: "BASIC", account_id: accountId, row_version: 2 });
    const t = transport({ [LIST]: [page([unlinked])], [ADAPTERS]: [adapters()],
      "GET /api/v1/source-accounts?limit=200": [page([])], "POST /api/v1/source-accounts": [{ status: 201, body: created }],
      [`PATCH /api/v1/sources/${sourceId}`]: [{ status: 200, body: linked }] });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Gestionar acceso" }));
    await screen.findByText(/Aún no hay cuentas para esta fuente/);
    expect((screen.getByLabelText("Usuario") as HTMLInputElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Nombre para identificar la cuenta"), { target: { value: "Cuenta local" } });
    fireEvent.click(screen.getByRole("button", { name: "Crear y vincular cuenta" }));
    await screen.findByText("Cuenta vinculada a la fuente.");
    await waitFor(() => expect((screen.getByLabelText("Usuario") as HTMLInputElement).disabled).toBe(false));
    const patch = t.calls.find((call) => call.method === "PATCH")!;
    expect(patch.headers).toEqual({ "If-Match": "1" });
    expect((patch.body as ModelSource).account_id).toBe(accountId);
    expect(t.calls.find((call) => call.method === "POST" && call.path.endsWith("source-accounts"))?.body).toEqual({
      source_id: sourceId, credential_reference_id: null, display_name: "Cuenta local", accepted_license_hashes: [],
    });
  });
  it("desvincula la cuenta con una mutación CAS y conserva la fuente", async () => {
    const account = { id: accountId, source_id: sourceId, credential_reference_id: null, display_name: "Cuenta local", accepted_license_hashes: [], row_version: 1 };
    const current = source({ auth_mode: "BASIC", account_id: accountId });
    const unlinked = source({ auth_mode: "BASIC", account_id: null, row_version: 2 });
    const t = transport({ [LIST]: [page([current])], [ADAPTERS]: [adapters()],
      "GET /api/v1/source-accounts?limit=200": [page([account])],
      [`PATCH /api/v1/sources/${sourceId}`]: [ { status: 200, body: unlinked } ] });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Gestionar acceso" }));
    await screen.findByRole("option", { name: "Cuenta local" });
    fireEvent.click(screen.getByRole("button", { name: "Desvincular cuenta" }));
    await screen.findByText("Cuenta desvinculada de la fuente.");
    const patch = t.calls.find((call) => call.method === "PATCH")!;
    expect(patch.headers).toEqual({ "If-Match": "1" });
    expect((patch.body as ModelSource).account_id).toBeNull();
    expect(screen.getByRole("article", { name: "Fuente Repositorio" })).toBeTruthy();
  });
});
