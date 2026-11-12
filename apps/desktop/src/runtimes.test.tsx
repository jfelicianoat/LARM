import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import App from "./App";
import type { RawRequest, RawResponse, Transport } from "./api/client";
import { CHANNELS, type Runtime, type RuntimeInstallation, type RuntimeProfileLock, type RuntimeVersion } from "./api/runtimes";

afterEach(() => { cleanup(); window.location.hash = ""; });
const RID = "0192aaaa-0000-7000-8000-000000000001", BID = "0192aaaa-0000-7000-8000-000000000002";
const PROFILE_HASH = "a".repeat(64);
const LIST = "/api/v1/runtimes?limit=200", VERSIONS = `/api/v1/runtimes/${RID}/versions?limit=200`;
const record = { created_at: "2026-10-20T13:00:00.000Z", updated_at: "2026-10-20T13:00:00.000Z", metadata: {}, row_version: 1 };
const runtime = (over: Partial<Runtime> = {}): Runtime => ({ ...record, id: RID, name: "Motor de prueba", adapter_id: BID, capabilities: ["vendor.future"], default_version_id: null, channel_policy: { allowed_channels: ["stable"] }, retention_policy: { keep_last_validated: 3 }, ...over });
const versions = (): RuntimeVersion[] => CHANNELS.map((channel, i) => ({ ...record, id: `version-${i}`, runtime_id: RID, version_string: `b7142-${channel}`, channel, commit: null, build: null, released_at: null, artifact_hash: null, promotion: "NONE", lifecycle: "CANDIDATE", metadata: channel === "custom" ? { upstream_channel: { value: "experimental.vendor/42", reason: "UNKNOWN" } } : {} }));
const installation = (over: Partial<RuntimeInstallation> = {}): RuntimeInstallation => ({
  ...record, id: "0192aaaa-0000-7000-8000-000000000040", runtime_version_id: "version-0",
  storage_location_id: null, relative_path: null, external_host_id: null, strategy: "EXTERNAL_ADOPTION",
  state: "ACTIVE", stage: "VALIDATED", registration_mode: "EXTERNAL", ownership: "EXTERNAL", lock_hash: null,
  ...over,
});
const profileLock = (): RuntimeProfileLock => ({
  schema_version: "larm.runtime-profile-lock/1",
  key: { runtime: "llama.cpp", source_url: "https://example.org/llama.cpp/1.0", source_revision: "0123456789abcdef0123456789abcdef01234567", source_sha256: "b".repeat(64), upstream_version: "1.0.0", os: "windows", os_build: "10.0.22631", backend: "CPU", protocol: "openai-compatible" },
  fields: { upstream_version: "1.0.0", installation_strategies: ["PREBUILT_BINARY"], install_strategy: null },
  qualification: "UNQUALIFIED",
  lock_sha256: PROFILE_HASH,
});
const page = (items: unknown[], next_cursor: string | null = null): RawResponse => ({ status: 200, body: { items, next_cursor, snapshot_at: "2026-10-20T13:00:00.000Z" } });
const error = (status: number, reason: string): RawResponse => ({ status, body: { code: status === 409 ? "STALE_REVISION" : "FORBIDDEN", message: "fixture", retryable: false, correlation_id: "c", details: { reason } } });
type Handler = (request: RawRequest) => RawResponse | Promise<RawResponse>;
function transport(handler?: Handler): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const t = (async (request: RawRequest) => {
    calls.push(request);
    if (request.path.includes("/health/")) return { status: 200, body: { status: request.path.endsWith("live") ? "LIVE" : "READY", schema_version: "1" } };
    if (handler) return handler(request);
    if (request.path.startsWith("/api/v1/adapters")) return page([{ id: "installation", family: "runtime", adapter_id: "fixture.engine", version: "1.0.0", metadata: { runtime_bindings: [{ id: BID, runtime_family: "Engine", capabilities: ["vendor.future"] }] } }]);
    if (request.path === LIST) return page([runtime()]);
    if (request.path === VERSIONS) return page(versions());
    if (request.path === `/api/v1/runtimes/${RID}`) return { status: 200, body: runtime() };
    if (request.path === "/api/v1/runtimes/other") return { status: 200, body: runtime({ id: "other", name: "Segundo" }) };
    return page([]);
  }) as Transport & { calls: RawRequest[] };
  t.calls = calls; return t;
}
function open(t: Transport) { window.location.hash = "#/runtimes"; render(<App transport={t} pollMs={60_000} />); }
async function detail() { fireEvent.click(await screen.findByRole("button", { name: "Ver versiones de Motor de prueba" })); await screen.findByRole("list", { name: "Versiones registradas" }); }

describe("inventario de runtimes", () => {
  it("presenta los cinco canales y unknown original sin ocultarlos por la política stable", async () => {
    open(transport()); await detail();
    const list = screen.getByRole("list", { name: "Versiones registradas" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(5);
    for (const channel of CHANNELS) expect(within(list).getByText(`(${channel})`)).toBeTruthy();
    expect(screen.getByText(/Canal original sin clasificar: experimental.vendor/)).toBeTruthy();
    expect(document.activeElement).toBe(within(screen.getByRole("region", { name: "Detalle de Motor de prueba" })).getByRole("heading", { name: "Motor de prueba" }));
  });
  it("mantiene visible una clasificación unknown guardada en el formato anterior", async () => {
    const rows = versions();
    rows[4] = { ...rows[4], metadata: { channel_reason: "UNKNOWN" } };
    const base = transport();
    open(transport((request) => request.path === VERSIONS ? page(rows) : base(request)));
    await detail();
    expect(screen.getByText("Canal original sin clasificar: Sin valor")).toBeTruthy();
  });
  it("vacío sin conectores ofrece un siguiente paso y bloquea un alta sin binding", async () => {
    open(transport(() => page([])));
    await screen.findByText("Todavía no hay ningún runtime registrado.");
    expect(screen.getByText(/No hay conectores de runtime registrados/)).toBeTruthy();
    expect((screen.getByRole("button", { name: "Registrar runtime" }) as HTMLButtonElement).disabled).toBe(true);
  });
  it("guarda política con If-Match y evita enviar la selección vacía", async () => {
    const base = transport();
    const t = transport((req) => req.method === "PATCH" ? { status: 200, body: runtime({ row_version: 2, channel_policy: { allowed_channels: ["nightly", "commit"] } }) } : base(req));
    open(t); await detail(); fireEvent.click(screen.getByRole("button", { name: "Editar política" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Estable (stable)" }));
    fireEvent.click(screen.getByRole("button", { name: "Guardar política" }));
    await screen.findByText("Selecciona al menos un canal."); expect(t.calls.some((r) => r.method === "PATCH")).toBe(false);
    fireEvent.click(screen.getByRole("checkbox", { name: "Nocturno (nightly)" })); fireEvent.click(screen.getByRole("checkbox", { name: "Commit (commit)" }));
    fireEvent.click(screen.getByRole("button", { name: "Guardar política" }));
    await screen.findByRole("button", { name: "Editar política" });
    expect(t.calls.find((r) => r.method === "PATCH")).toEqual({ method: "PATCH", path: `/api/v1/runtimes/${RID}/channel-policy`, body: { allowed_channels: ["nightly", "commit"] }, headers: { "If-Match": "1" } });
    expect(within(screen.getByRole("list", { name: "Versiones registradas" })).getAllByRole("listitem")).toHaveLength(5);
  });
  it("conserva el borrador al recibir stale y recarga sólo por acción explícita", async () => {
    const base = transport();
    const current = runtime({ row_version: 4, channel_policy: { allowed_channels: ["prerelease"] } });
    const t = transport((req) => req.method === "PATCH" ? error(409, "stale_revision") : req.path === `/api/v1/runtimes/${RID}` ? { status: 200, body: current } : base(req));
    open(t); await detail(); fireEvent.click(screen.getByRole("button", { name: "Editar política" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Commit (commit)" })); fireEvent.click(screen.getByRole("button", { name: "Guardar política" }));
    await screen.findByText(/Este runtime ha cambiado/);
    expect((screen.getByRole("checkbox", { name: "Commit (commit)" }) as HTMLInputElement).checked).toBe(true);
    expect(t.calls.filter((r) => r.path === `/api/v1/runtimes/${RID}`)).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Descartar y recargar" })); await screen.findByRole("button", { name: "Editar política" });
    fireEvent.click(screen.getByRole("button", { name: "Editar política" }));
    expect((screen.getByRole("checkbox", { name: "Prelanzamiento (prerelease)" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "Commit (commit)" }) as HTMLInputElement).checked).toBe(false);
  });
  it("un 403 conserva selección y explica permiso sin cerrar el editor", async () => {
    const base = transport(); const t = transport((req) => req.method === "PATCH" ? error(403, "scope_missing") : base(req));
    open(t); await detail(); fireEvent.click(screen.getByRole("button", { name: "Editar política" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Nocturno (nightly)" })); fireEvent.click(screen.getByRole("button", { name: "Guardar política" }));
    await screen.findByText(/Tu cuenta no tiene permiso/); expect((screen.getByRole("checkbox", { name: "Nocturno (nightly)" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("button", { name: "Guardar política" }) as HTMLButtonElement).disabled).toBe(false);
  });
  it("reintenta el alta con la misma key y deriva capacidades del binding publicado", async () => {
    const base = transport(); let count = 0;
    const t = transport((req) => req.method === "POST" ? (++count === 1 ? error(500, "database_error") : { status: 201, body: runtime({ name: "Nuevo motor" }) }) : base(req));
    open(t); const button = await screen.findByRole("button", { name: "Registrar runtime" });
    await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false)); fireEvent.click(button);
    fireEvent.change(screen.getByLabelText("Nombre del runtime"), { target: { value: "Nuevo motor" } });
    fireEvent.change(screen.getByLabelText("Conector de runtime"), { target: { value: BID } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar runtime" })); await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Guardar runtime" })); await screen.findByText("Runtime registrado.");
    const posts = t.calls.filter((r) => r.method === "POST"); expect(posts).toHaveLength(2);
    expect(posts[0].headers?.["Idempotency-Key"]).toBe(posts[1].headers?.["Idempotency-Key"]);
    expect(posts[0].body).toEqual({ name: "Nuevo motor", adapter_id: BID, capabilities: ["vendor.future"], default_version_id: null, channel_policy: { allowed_channels: ["stable"] }, retention_policy: { keep_last_validated: 3 } });
  });
  it("carga páginas de conectores, runtimes y versiones sin perder elementos", async () => {
    const base = transport();
    const t = transport((req) => {
      if (req.path === "/api/v1/adapters?limit=200") return page([], "binding-next");
      if (req.path === "/api/v1/adapters?limit=200&cursor=binding-next") return base({ ...req, path: "/api/v1/adapters?limit=200" });
      if (req.path === LIST) return page([runtime()], "runtime-next");
      if (req.path === LIST + "&cursor=runtime-next") return page([runtime({ id: "other", name: "Segundo" })]);
      if (req.path === VERSIONS) return page(versions().slice(0, 2), "version-next");
      if (req.path === VERSIONS + "&cursor=version-next") return page(versions().slice(2));
      return base(req);
    });
    open(t); const more = await screen.findByRole("button", { name: "Cargar más runtimes" });
    await waitFor(() => expect((more as HTMLButtonElement).disabled).toBe(false)); fireEvent.click(more); await screen.findByRole("button", { name: "Ver versiones de Segundo" });
    await detail(); expect(within(screen.getByRole("list", { name: "Versiones registradas" })).getAllByRole("listitem")).toHaveLength(5);
    expect((screen.getByRole("button", { name: "Registrar runtime" }) as HTMLButtonElement).disabled).toBe(false);
  });
  it("una respuesta tardía de un runtime anterior no sustituye el detalle nuevo", async () => {
    const base = transport(); let resolve: (r: RawResponse) => void = () => {};
    const t = transport((req) => req.path === LIST ? page([runtime(), runtime({ id: "other", name: "Segundo" })])
      : req.path === VERSIONS ? new Promise((done) => { resolve = done; }) : base(req));
    open(t); fireEvent.click(await screen.findByRole("button", { name: "Ver versiones de Motor de prueba" }));
    await waitFor(() => expect(t.calls.some((r) => r.path === VERSIONS)).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "Ver versiones de Segundo" })); await screen.findByText("No hay versiones registradas para este runtime.");
    await act(async () => { resolve(page(versions())); });
    expect(screen.queryByText("b7142-stable")).toBeNull(); expect(screen.getByRole("region", { name: "Detalle de Segundo" })).toBeTruthy();
  });
  it("actualizar y volver a elegir el mismo runtime recargan política y versiones", async () => {
    const base = transport(); let current = runtime(), inventory = versions();
    const t = transport((req) => req.path === LIST ? page([current])
      : req.path === `/api/v1/runtimes/${RID}` ? { status: 200, body: current }
        : req.path === VERSIONS ? page(inventory) : base(req));
    open(t); await detail();
    current = runtime({ row_version: 2, channel_policy: { allowed_channels: ["nightly"] } });
    inventory = [{ ...versions()[0], version_string: "compilación nueva" }];
    fireEvent.click(screen.getByRole("button", { name: "Actualizar runtimes" }));
    await screen.findByText("compilación nueva");
    expect(within(screen.getByRole("region", { name: "Detalle de Motor de prueba" })).getByText("Nocturno")).toBeTruthy();
    expect(screen.queryByText("b7142-stable")).toBeNull();
    inventory = [{ ...versions()[0], version_string: "otra compilación" }];
    fireEvent.click(await screen.findByRole("button", { name: "Ver versiones de Motor de prueba" }));
    await screen.findByText("otra compilación");
  });
  it("actualizar conserva borrador y CAS de su revisión original", async () => {
    const base = transport(); let current = runtime();
    const t = transport((req) => req.method === "PATCH" ? error(409, "stale_revision")
      : req.path === LIST ? page([current]) : req.path === `/api/v1/runtimes/${RID}` ? { status: 200, body: current } : base(req));
    open(t); await detail(); fireEvent.click(screen.getByRole("button", { name: "Editar política" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Commit (commit)" }));
    current = runtime({ row_version: 2, channel_policy: { allowed_channels: ["nightly"] } });
    fireEvent.click(screen.getByRole("button", { name: "Actualizar runtimes" }));
    await screen.findByText(/Los datos registrados han cambiado/);
    expect((screen.getByRole("checkbox", { name: "Commit (commit)" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "Nocturno (nightly)" }) as HTMLInputElement).checked).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Guardar política" })); await screen.findByText(/Este runtime ha cambiado/);
    expect(t.calls.find((r) => r.method === "PATCH")?.headers?.["If-Match"]).toBe("1");
  });
  it("mantiene visible un candidato BROKEN sin ofrecer promoción", async () => {
    const base = transport();
    const broken = installation({ state: "BROKEN", stage: "FAILED", ownership: "LARM_MANAGED", registration_mode: "MANAGED" });
    const t = transport((req) => req.path.startsWith(`/api/v1/runtime-installations?runtime_id=${RID}`) ? page([broken]) : base(req));
    open(t); await detail();
    expect(screen.getByText(/No operativa \/ Fallida/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /promover/i })).toBeNull();
  });
  it("muestra el ownership externo y no ofrece eliminar ni adoptar de nuevo", async () => {
    const base = transport();
    const external = installation();
    const t = transport((req) => req.path.startsWith(`/api/v1/runtime-installations?runtime_id=${RID}`) ? page([external]) : base(req));
    open(t); await detail();
    const panel = screen.getByRole("region", { name: "Detalle de Motor de prueba" });
    expect(within(panel).getByText(/Externo \/ Externo/)).toBeTruthy();
    expect(within(panel).queryByRole("button", { name: /eliminar|adoptar de nuevo/i })).toBeNull();
  });
  it("una lectura anterior al guardado no revierte la política recién confirmada", async () => {
    const base = transport(); let pending = false, resolve: (r: RawResponse) => void = () => {};
    const t = transport((req) => req.path === VERSIONS && pending ? new Promise((done) => { resolve = done; })
      : req.method === "PATCH" ? { status: 200, body: runtime({ row_version: 2, channel_policy: { allowed_channels: ["stable", "commit"] } }) } : base(req));
    open(t); await detail(); fireEvent.click(screen.getByRole("button", { name: "Editar política" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Commit (commit)" }));
    pending = true; fireEvent.click(screen.getByRole("button", { name: "Actualizar runtimes" }));
    await screen.findByText("Cargando versiones, instalaciones y ubicaciones…");
    fireEvent.click(screen.getByRole("button", { name: "Guardar política" })); await screen.findByRole("button", { name: "Editar política" });
    await act(async () => { resolve(page(versions())); });
    expect(within(screen.getByRole("region", { name: "Detalle de Motor de prueba" })).getByText("Estable · Commit")).toBeTruthy();
  });
  it("adquiere un lock P099 y lo consulta por hash mostrando que no está cualificado", async () => {
    const base = transport(); const lock = profileLock();
    const t = transport((req) => req.method === "PUT" || req.path === `/api/v1/runtime-profile-locks/${PROFILE_HASH}`
      ? { status: 200, body: lock } : base(req));
    open(t);
    fireEvent.change(await screen.findByLabelText("Documento del lock (JSON)"), { target: { value: JSON.stringify(lock) } });
    fireEvent.click(screen.getByRole("button", { name: "Adquirir lock" }));
    expect(await screen.findByText(/Sin cualificación \(UNQUALIFIED\)/)).toBeTruthy();
    expect(screen.getByText(/no acredita compatibilidad ni permite instalar/)).toBeTruthy();
    expect(t.calls.find((request) => request.method === "PUT")).toEqual({
      method: "PUT", path: `/api/v1/runtime-profile-locks/${PROFILE_HASH}`, body: lock,
    });

    fireEvent.click(screen.getByRole("button", { name: "Consultar lock" }));
    await waitFor(() => expect(t.calls.some((request) => request.method === "GET" && request.path === `/api/v1/runtime-profile-locks/${PROFILE_HASH}`)).toBe(true));
    expect(screen.getByText("1.0.0")).toBeTruthy();
  });
  it("no intenta adquirir un documento promocionado ni un JSON inválido", async () => {
    const t = transport(); open(t);
    const input = await screen.findByLabelText("Documento del lock (JSON)");
    fireEvent.change(input, { target: { value: JSON.stringify({ ...profileLock(), qualification: "QUALIFIED" }) } });
    fireEvent.click(screen.getByRole("button", { name: "Adquirir lock" }));
    expect((await screen.findByRole("alert")).textContent).toContain("conservar el estado UNQUALIFIED");
    expect(t.calls.some((request) => request.method === "PUT")).toBe(false);

    fireEvent.change(input, { target: { value: "not-json" } });
    fireEvent.click(screen.getByRole("button", { name: "Adquirir lock" }));
    expect((await screen.findByRole("alert")).textContent).toContain("no es JSON válido");
    expect(t.calls.some((request) => request.method === "PUT")).toBe(false);
  });
  it("guarda retención con CAS e idempotencia sin presentar la política como borrado ya ejecutado", async () => {
    const base = transport();
    let saves = 0;
    const t = transport((req) => req.method === "PATCH" && req.path.endsWith("/retention-policy")
      ? (++saves === 1 ? error(500, "database_error") : { status: 200, body: runtime({ row_version: 2, retention_policy: { keep_last_validated: 5 } }) }) : base(req));
    open(t); await detail();
    expect(screen.getByText(/La limpieza automática aún requiere la integración del worker/)).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Versiones validadas que conservar"), { target: { value: "5" } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar retención" }));
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Guardar retención" }));
    await screen.findByText("Configuración actual: conservar 5 versiones validadas.");
    const patches = t.calls.filter((request) => request.method === "PATCH" && request.path.endsWith("/retention-policy"));
    expect(patches).toHaveLength(2);
    expect(patches[0].headers?.["Idempotency-Key"]).toBe(patches[1].headers?.["Idempotency-Key"]);
    expect(patches[0]).toMatchObject({
      body: { keep_last_validated: 5 }, headers: { "If-Match": "1", "Idempotency-Key": expect.any(String) },
    });
  });
  it("prepara rollback y exige confirmación antes de cambiar la versión predeterminada", async () => {
    const base = transport();
    const rows = [
      { ...versions()[0], version_string: "1.0.0", lifecycle: "DEFAULT" },
      { ...versions()[1], version_string: "0.9.0", lifecycle: "RETAINED" },
    ];
    const installed = installation({ runtime_version_id: rows[1].id, state: "VALIDATED", stage: "VALIDATED" });
    const t = transport((req) => req.path === VERSIONS ? page(rows)
      : req.path.startsWith(`/api/v1/runtime-installations?runtime_id=${RID}`) ? page([installed])
        : req.method === "POST" && req.path.endsWith("/rollback") ? { status: 201, body: { resource_id: RID, row_version: 2, event_id: "0192aaaa-0000-7000-8000-000000000050" } }
          : base(req));
    open(t); await detail();
    fireEvent.click(screen.getByRole("button", { name: "Preparar rollback a esta versión" }));
    expect(screen.getByRole("region", { name: "Confirmar rollback" })).toBeTruthy();
    expect(t.calls.some((request) => request.method === "POST" && request.path.endsWith("/rollback"))).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Confirmar rollback" }));
    await waitFor(() => expect(t.calls.some((request) => request.method === "POST" && request.path.endsWith("/rollback"))).toBe(true));
    expect(t.calls.find((request) => request.method === "POST" && request.path.endsWith("/rollback"))).toMatchObject({
      path: `/api/v1/runtime-versions/${rows[1].id}/rollback`, body: { expected_default_version_id: null },
      headers: { "Idempotency-Key": expect.any(String) },
    });
  });
  it("fija y retira una versión para la pareja modelo y perfil", async () => {
    const base = transport();
    const rows = versions();
    rows[0] = { ...rows[0], lifecycle: "VALIDATED" };
    const variantId = "0192aaaa-0000-7000-8000-000000000060";
    const profileId = "0192aaaa-0000-7000-8000-000000000061";
    const profile = { ...record, id: profileId, name: "BALANCED", parameters: {}, backend_id: null, profile_hash: "profile-hash" };
    let pins: Array<Record<string, unknown>> = [];
    const t = transport((request) => {
      if (request.path === "/api/v1/variants?limit=200") return page([{ id: variantId, status: "AVAILABLE", lifecycle: "AVAILABLE" }]);
      if (request.path === "/api/v1/launch-profiles?limit=200") return page([profile]);
      if (request.path === "/api/v1/runtime-pins?limit=200") return page(pins);
      if (request.method === "POST" && request.path.endsWith("/pin")) {
        const body = request.body as { variant_id: string; launch_profile_id: string };
        pins = [{ ...record, id: "pin-id", ...body, runtime_version_id: rows[0].id }];
        return { status: 201, body: { resource_id: "pin-id", row_version: 1, event_id: "event-id" } };
      }
      if (request.method === "DELETE" && request.path.startsWith("/api/v1/runtime-pins/")) {
        pins = [];
        return { status: 204, body: null };
      }
      return request.path === VERSIONS ? page(rows) : base(request);
    });
    open(t); await detail();
    await screen.findByRole("combobox", { name: "Modelo" });
    fireEvent.change(screen.getByLabelText("Modelo"), { target: { value: variantId } });
    fireEvent.change(screen.getByLabelText("Perfil de lanzamiento"), { target: { value: profileId } });
    fireEvent.change(screen.getByLabelText("Versión que fijar"), { target: { value: rows[0].id } });
    fireEvent.click(screen.getByRole("button", { name: "Fijar esta versión" }));
    await screen.findByText("Pin actual: b7142-stable.");
    expect(t.calls.find((request) => request.method === "POST" && request.path.endsWith("/pin"))).toMatchObject({
      path: `/api/v1/runtime-versions/${rows[0].id}/pin`,
      body: { variant_id: variantId, launch_profile_id: profileId },
      headers: { "Idempotency-Key": expect.any(String) },
    });
    fireEvent.click(screen.getByRole("button", { name: "Retirar pin para nuevas leases" }));
    await screen.findByText("Esta combinación usa la versión predeterminada del runtime.");
    expect(t.calls.find((request) => request.method === "DELETE" && request.path.startsWith("/api/v1/runtime-pins/"))).toMatchObject({
      path: `/api/v1/runtime-pins/${variantId}/${profileId}`,
      headers: { "Idempotency-Key": expect.any(String) },
    });
  });
  it("actualiza una versión candidata con consentimiento y la ruta P032", async () => {
    const base = transport();
    const rows = [
      { ...versions()[0], version_string: "1.0.0", lifecycle: "DEFAULT" },
      { ...versions()[1], version_string: "2.0.0", lifecycle: "CANDIDATE" },
    ];
    const location = { id: "0192aaaa-0000-7000-8000-000000000030", name: "Modelos", drive_letter: "D:", canonical_path: "D:/LARM", state: "ONLINE" };
    const receipt = { job_id: "0192aaaa-0000-7000-8000-000000000031", status_url: "/api/v1/jobs/0192aaaa-0000-7000-8000-000000000031", state: "QUEUED" } as const;
    const matchingProfile = { ...profileLock(), key: { ...profileLock().key, upstream_version: "2.0.0" } };
    const t = transport((req) => req.path === VERSIONS ? page(rows)
      : req.path === "/api/v1/storage-locations?limit=200" ? page([location])
        : req.path === `/api/v1/runtime-profile-locks/${PROFILE_HASH}` ? { status: 200, body: matchingProfile }
          : req.method === "POST" && req.path.endsWith("/update") ? { status: 202, body: receipt }
            : base(req));
    open(t); await detail();
    fireEvent.click(await screen.findByRole("button", { name: "Actualizar a esta versión" }));
    fireEvent.change(screen.getByLabelText("Ubicación de almacenamiento"), { target: { value: location.id } });
    fireEvent.change(screen.getByLabelText("SHA-256 del perfil P099"), { target: { value: PROFILE_HASH } });
    fireEvent.click(screen.getByRole("button", { name: "Validar perfil" }));
    await screen.findByText("llama.cpp · 2.0.0");
    const confirm = screen.getByRole("button", { name: "Confirmar y solicitar actualización" });
    expect((confirm as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByLabelText("Confirmo solicitar la actualización a 2.0.0."));
    fireEvent.click(confirm);
    await screen.findByText("En cola");
    expect(t.calls.find((request) => request.method === "POST" && request.path.endsWith("/update"))).toMatchObject({
      path: `/api/v1/runtime-versions/${rows[1].id}/update`,
      body: { strategy: "PREBUILT_BINARY", storage_location_id: location.id, profile_lock_hash: PROFILE_HASH, external_host_id: null },
      headers: { "Idempotency-Key": expect.any(String) },
    });
  });
  it("solicita una instalación ligada a versión, lock, estrategia y ubicación y permite consultar el job", async () => {
    const base = transport();
    const version = { ...versions()[0], version_string: "1.0.0" };
    const location = { id: "0192aaaa-0000-7000-8000-000000000030", name: "Modelos", drive_letter: "D:", canonical_path: "D:/LARM", state: "ONLINE" };
    const receipt = { job_id: "0192aaaa-0000-7000-8000-000000000031", status_url: "/api/v1/jobs/0192aaaa-0000-7000-8000-000000000031", state: "QUEUED" } as const;
    let runtimeReads = 0;
    let releaseDetailRefresh: (() => void) | undefined;
    let signalDetailRefresh: (() => void) | undefined;
    const detailRefreshGate = new Promise<void>((resolve) => { releaseDetailRefresh = resolve; });
    const detailRefreshStarted = new Promise<void>((resolve) => { signalDetailRefresh = resolve; });
    const t = transport(async (req) => {
      if (req.path === `/api/v1/runtimes/${RID}`) {
        runtimeReads += 1;
        if (runtimeReads > 1) { signalDetailRefresh?.(); await detailRefreshGate; }
        return { status: 200, body: runtime() };
      }
      if (req.path === VERSIONS) return page([version]);
      if (req.path === "/api/v1/storage-locations?limit=200") return page([location]);
      if (req.path === `/api/v1/runtime-profile-locks/${PROFILE_HASH}`) return { status: 200, body: profileLock() };
      if (req.method === "POST" && req.path === "/api/v1/runtime-installations") return { status: 202, body: receipt };
      if (req.path === receipt.status_url) return { status: 200, body: { job_id: receipt.job_id, state: "COMPLETED", error: null } };
      return base(req);
    });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Ver versiones de Motor de prueba" }));
    await screen.findByText("1.0.0");
    fireEvent.click(screen.getByRole("button", { name: "Solicitar una instalación" }));
    fireEvent.change(screen.getByLabelText("Versión registrada"), { target: { value: version.id } });
    fireEvent.change(screen.getByLabelText("Ubicación de almacenamiento"), { target: { value: location.id } });
    fireEvent.change(screen.getByLabelText("SHA-256 del perfil P099"), { target: { value: PROFILE_HASH } });
    fireEvent.click(screen.getByRole("button", { name: "Validar perfil" }));
    await screen.findByText("llama.cpp · 1.0.0");
    fireEvent.click(screen.getByRole("button", { name: "Solicitar instalación" }));
    await screen.findByText("En cola");
    const post = t.calls.find((request) => request.method === "POST" && request.path === "/api/v1/runtime-installations");
    expect(post?.body).toEqual({ runtime_id: RID, upstream_version: "1.0.0", strategy: "PREBUILT_BINARY", storage_location_id: location.id, profile_lock_hash: PROFILE_HASH, external_host_id: null });
    expect(post?.headers?.["Idempotency-Key"]).toMatch(/^[0-9a-f-]{36}$/);
    await screen.findByText("En cola");
    await detailRefreshStarted;
    expect(screen.getByText("En cola")).toBeTruthy();
    releaseDetailRefresh?.();
    fireEvent.click(screen.getByRole("button", { name: "Actualizar estado de la instalación" }));
    await screen.findByText("Instalación completada");
  });
  it("cancela una instalación sólo mientras el job sigue en cola", async () => {
    const base = transport();
    const version = { ...versions()[0], version_string: "1.0.0" };
    const location = { id: "0192aaaa-0000-7000-8000-000000000030", name: "Modelos", drive_letter: "D:", canonical_path: "D:/LARM", state: "ONLINE" };
    const receipt = { job_id: "0192aaaa-0000-7000-8000-000000000031", status_url: "/api/v1/jobs/0192aaaa-0000-7000-8000-000000000031", state: "QUEUED" } as const;
    const t = transport((req) => req.path === VERSIONS ? page([version])
      : req.path === "/api/v1/storage-locations?limit=200" ? page([location])
        : req.path === `/api/v1/runtime-profile-locks/${PROFILE_HASH}` ? { status: 200, body: profileLock() }
          : req.method === "POST" && req.path === "/api/v1/runtime-installations" ? { status: 202, body: receipt }
            : req.method === "POST" && req.path === `/api/v1/jobs/${receipt.job_id}/cancel` ? { status: 200, body: { ...receipt, state: "CANCELLED" } }
              : req.path === receipt.status_url ? { status: 200, body: { job_id: receipt.job_id, state: "QUEUED", error: null } }
                : base(req));
    open(t); fireEvent.click(await screen.findByRole("button", { name: "Ver versiones de Motor de prueba" }));
    await screen.findByText("1.0.0"); fireEvent.click(screen.getByRole("button", { name: "Solicitar una instalación" }));
    fireEvent.change(screen.getByLabelText("Versión registrada"), { target: { value: version.id } });
    fireEvent.change(screen.getByLabelText("Ubicación de almacenamiento"), { target: { value: location.id } });
    fireEvent.change(screen.getByLabelText("SHA-256 del perfil P099"), { target: { value: PROFILE_HASH } });
    fireEvent.click(screen.getByRole("button", { name: "Validar perfil" })); await screen.findByText("llama.cpp · 1.0.0");
    fireEvent.click(screen.getByRole("button", { name: "Solicitar instalación" })); await screen.findByText("En cola");
    fireEvent.click(screen.getByRole("button", { name: "Cancelar instalación en cola" }));
    await screen.findByText("Instalación cancelada");
    expect(t.calls.find((request) => request.path === `/api/v1/jobs/${receipt.job_id}/cancel`)).toMatchObject({ method: "POST" });
    expect(screen.queryByRole("button", { name: "Cancelar instalación en cola" })).toBeNull();
    expect((screen.getByRole("button", { name: "Actualizar estado de la instalación" }) as HTMLButtonElement).disabled).toBe(true);
  });
  it("mantiene RUNNING hasta que el worker confirma y se consulta el drenaje de una cancelación", async () => {
    const base = transport();
    const version = { ...versions()[0], version_string: "1.0.0" };
    const location = { id: "0192aaaa-0000-7000-8000-000000000030", name: "Modelos", drive_letter: "D:", canonical_path: "D:/LARM", state: "ONLINE" };
    const receipt = { job_id: "0192aaaa-0000-7000-8000-000000000031", status_url: "/api/v1/jobs/0192aaaa-0000-7000-8000-000000000031", state: "RUNNING" } as const;
    let cancelRequested = false, installationReads = 0;
    const t = transport((req) => req.path === VERSIONS ? page([version])
      : req.path === "/api/v1/storage-locations?limit=200" ? page([location])
        : req.path === `/api/v1/runtime-profile-locks/${PROFILE_HASH}` ? { status: 200, body: profileLock() }
          : req.method === "POST" && req.path === "/api/v1/runtime-installations" ? { status: 202, body: receipt }
            : req.method === "POST" && req.path === `/api/v1/jobs/${receipt.job_id}/cancel` ? (cancelRequested = true, { status: 200, body: receipt })
              : req.path.startsWith(`/api/v1/runtime-installations?runtime_id=${RID}`) ? (installationReads += 1, page([]))
                : req.path === receipt.status_url ? { status: 200, body: { job_id: receipt.job_id, state: cancelRequested ? "CANCELLED" : "RUNNING", error: null } }
                  : base(req));
    open(t); fireEvent.click(await screen.findByRole("button", { name: "Ver versiones de Motor de prueba" }));
    await screen.findByText("1.0.0"); fireEvent.click(screen.getByRole("button", { name: "Solicitar una instalación" }));
    fireEvent.change(screen.getByLabelText("Versión registrada"), { target: { value: version.id } });
    fireEvent.change(screen.getByLabelText("Ubicación de almacenamiento"), { target: { value: location.id } });
    fireEvent.change(screen.getByLabelText("SHA-256 del perfil P099"), { target: { value: PROFILE_HASH } });
    fireEvent.click(screen.getByRole("button", { name: "Validar perfil" })); await screen.findByText("llama.cpp · 1.0.0");
    fireEvent.click(screen.getByRole("button", { name: "Solicitar instalación" })); await screen.findByText("Instalando");
    await waitFor(() => expect(installationReads).toBeGreaterThan(1));
    fireEvent.click(screen.getByRole("button", { name: "Cancelar instalación" }));
    await screen.findByText("Cancelación solicitada; esperando a que el worker detenga y drene la instalación.");
    expect((screen.getByRole("button", { name: "Actualizar estado de la instalación" }) as HTMLButtonElement).disabled).toBe(false);
    const readsBeforeTerminalRefresh = installationReads;
    fireEvent.click(screen.getByRole("button", { name: "Actualizar estado de la instalación" }));
    await screen.findByText("Instalación cancelada");
    await waitFor(() => expect(installationReads).toBeGreaterThan(readsBeforeTerminalRefresh));
    expect(t.calls.find((request) => request.path === `/api/v1/jobs/${receipt.job_id}/cancel`)).toMatchObject({ method: "POST" });
  });
  it("explica que una estrategia sin cualificación no se encola", async () => {
    const base = transport();
    const version = { ...versions()[0], version_string: "1.0.0" };
    const location = { id: "0192aaaa-0000-7000-8000-000000000030", name: "Modelos", drive_letter: "D:", canonical_path: "D:/LARM", state: "ONLINE" };
    const t = transport((req) => req.path === VERSIONS ? page([version])
      : req.path === "/api/v1/storage-locations?limit=200" ? page([location])
        : req.path === `/api/v1/runtime-profile-locks/${PROFILE_HASH}` ? { status: 200, body: profileLock() }
          : req.method === "POST" && req.path === "/api/v1/runtime-installations" ? error(409, "runtime_profile_strategy_unqualified") : base(req));
    open(t); fireEvent.click(await screen.findByRole("button", { name: "Ver versiones de Motor de prueba" }));
    await screen.findByText("1.0.0"); fireEvent.click(screen.getByRole("button", { name: "Solicitar una instalación" }));
    fireEvent.change(screen.getByLabelText("Versión registrada"), { target: { value: version.id } });
    fireEvent.change(screen.getByLabelText("Ubicación de almacenamiento"), { target: { value: location.id } });
    fireEvent.change(screen.getByLabelText("SHA-256 del perfil P099"), { target: { value: PROFILE_HASH } });
    fireEvent.click(screen.getByRole("button", { name: "Validar perfil" })); await screen.findByText("llama.cpp · 1.0.0");
    fireEvent.click(screen.getByRole("button", { name: "Solicitar instalación" }));
    expect((await screen.findByRole("alert")).textContent).toContain("no tiene evidencia válida para la estrategia elegida");
    expect(t.calls.filter((request) => request.method === "POST" && request.path === "/api/v1/runtime-installations")).toHaveLength(1);
  });
  it("crea, copia, compara y edita perfiles reproducibles con If-Match", async () => {
    const base = transport();
    const saved: Array<Record<string, unknown>> = [];
    let nextId = 40;
    const parameters = {
      threads: 7, context: 4096, batch: 512, concurrency: 1, offload_layers: null, backend_id: BID,
      tokenizer_hash: null, chat_template_hash: null,
      generation: { temperature: null, top_p: null, seed: null, max_output_tokens: null },
      memory_limit_bytes: null, runtime_arguments: [], environment: { allowlist: [], variables: [] }, adapter_options: {},
    };
    const t = transport((request) => {
      if (request.path.startsWith("/api/v1/launch-profiles?")) return page(saved);
      if (request.method === "POST" && request.path === "/api/v1/launch-profiles") {
        const body = request.body as Record<string, unknown>;
        const profile = { ...record, id: `0192aaaa-0000-7000-8000-${String(nextId++).padStart(12, "0")}`, ...body, profile_hash: "a".repeat(64) };
        saved.push(profile); return { status: 201, body: profile };
      }
      if (request.method === "PATCH" && request.path.startsWith("/api/v1/launch-profiles/")) {
        const index = saved.findIndex((profile) => profile.id === request.path.split("/").at(-1));
        const profile = { ...saved[index], ...(request.body as Record<string, unknown>), row_version: 2, profile_hash: "b".repeat(64) };
        saved[index] = profile; return { status: 200, body: profile };
      }
      return base(request);
    });
    open(t);
    await screen.findByRole("heading", { name: "Perfiles de lanzamiento" });
    fireEvent.change(screen.getByLabelText("Parámetros efectivos (JSON)"), { target: { value: JSON.stringify(parameters, null, 2) } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar perfil" }));
    await screen.findByRole("button", { name: "Guardar revisión" });
    expect(t.calls.find((request) => request.method === "POST" && request.path === "/api/v1/launch-profiles")).toMatchObject({
      headers: { "Idempotency-Key": expect.any(String) },
      body: { name: "BALANCED", backend_id: null, parameters },
    });

    fireEvent.click(screen.getByRole("button", { name: "Copiar perfil" }));
    fireEvent.change(screen.getByLabelText("Parámetros efectivos (JSON)"), { target: { value: JSON.stringify({ ...parameters, batch: 256 }, null, 2) } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar perfil" }));
    await screen.findByText(/Cambios efectivos: .*\/parameters\/batch/);
    expect(t.calls.filter((request) => request.method === "POST" && request.path === "/api/v1/launch-profiles")).toHaveLength(2);

    fireEvent.change(screen.getByLabelText("Parámetros efectivos (JSON)"), { target: { value: JSON.stringify({ ...parameters, batch: 768 }, null, 2) } });
    fireEvent.click(screen.getByRole("button", { name: "Guardar revisión" }));
    await screen.findByText("b".repeat(64));
    expect(t.calls.find((request) => request.method === "PATCH" && request.path === `/api/v1/launch-profiles/${saved[1].id}`)).toMatchObject({
      headers: { "If-Match": "1" }, body: { name: "BALANCED", backend_id: null, parameters: { ...parameters, batch: 768 } },
    });
  });
});
