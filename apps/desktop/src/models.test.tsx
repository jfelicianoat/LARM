import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ApiClient, type RawRequest, type RawResponse, type Transport } from "./api/client";
import { ModelsPage, type ModelDetail } from "./models";

afterEach(cleanup);

const localId = "0192aaaa-0000-7000-8000-000000000001";
const candidateId = "0192aaaa-0000-7000-8000-000000000002";
const locationId = "0192aaaa-0000-7000-8000-000000000003";
const targetLocationId = "0192aaaa-0000-7000-8000-000000000015";
const sourceId = "0192aaaa-0000-7000-8000-000000000004";
const requiredId = "0192aaaa-0000-7000-8000-000000000005";
const optionalId = "0192aaaa-0000-7000-8000-000000000006";
const sourceBindingId = "0192aaaa-0000-7000-8000-000000000007";
const downloadJobId = "0192aaaa-0000-7000-8000-000000000022";
const downloadReceipt = { status: 202, body: { job_id: downloadJobId, status_url: `/api/v1/jobs/${downloadJobId}`, state: "QUEUED" } };

function page(items: unknown[], next_cursor: string | null = null): RawResponse {
  return { status: 200, body: { items, next_cursor, snapshot_at: "2026-10-23T12:00:00.000Z" } };
}
function json(body: unknown): RawResponse { return { status: 200, body }; }

function executionLimits(): ModelDetail["variant"]["model_limits"] {
  return { parameter_count: null, layers: null, kv_heads: null, head_dim: null, context_limit: null, kv_bits: null, model_max_memory_bytes: null, architecture_metadata_hash: null };
}

function variant(id: string, lifecycle: string, availability: string, ownership = "LARM_MANAGED") {
  return {
    id, row_version: 1, revision_id: `revision-${id}`, format: "GGUF", quantization: "Q4_K_M", architecture: "llama",
    bundle_hash: "a".repeat(64), lifecycle, availability, ownership, promotion: "DEFAULT", capabilities: ["text.generate"],
    model_limits: executionLimits(),
    update_available: false, status: lifecycle,
  };
}

function size(confidence: "EXACT" | "HIGH" | "ESTIMATED" | "UNKNOWN", bytes: string | null, lower: string | null, upper: string | null) {
  return { confidence, bytes, lower_bound_bytes: lower, upper_bound_bytes: upper, source: confidence === "EXACT" ? "MANIFEST" : confidence === "UNKNOWN" ? "UNKNOWN" : "SOURCE_METADATA", evidence_refs: ["fixture:size"], reason: confidence === "UNKNOWN" ? "metadata_missing" : null };
}

function detail(id: string, name: string, installed: boolean, availability: string) {
  const model = variant(id, installed ? "AVAILABLE" : "DISCOVERED", availability);
  return {
    variant: model,
    revision: { id: model.revision_id, revision_key: `main-${id}`, state: "CURRENT", upstream_commit: null },
    distribution: { id: `distribution-${id}`, source_id: sourceId, upstream_namespace: "fixture", upstream_model_id: name.toLowerCase(), license_id: "apache-2.0" },
    canonical_model: { id: `model-${id}`, name, capabilities: ["text.generate"] },
    bundle: { id: `bundle-${id}`, state: "VERIFIED", required_artifact_ids: [requiredId], optional_artifact_ids: [optionalId] },
    artifacts: [
      { id: requiredId, filename: "model.gguf", required: true, actual_size: installed ? "3221225472" : null, expected_size: "3221225472", size_estimate: size("EXACT", "3221225472", "3221225472", "3221225472"), sha256: "b".repeat(64) },
      { id: optionalId, filename: "config.json", required: false, actual_size: null, expected_size: null, size_estimate: size("UNKNOWN", null, null, null), sha256: null },
    ],
    locations: installed ? [{ id: `artifact-location-${id}`, artifact_id: requiredId, storage_location_id: locationId, relative_path: "models/model.gguf", file_size: "3221225472", sha256: "b".repeat(64), state: availability === "OFFLINE" ? "OFFLINE" : "ONLINE", ownership: "LARM_MANAGED" }] : [],
    compatibility: [{ status: "QUALIFIED", technical_fit: 82, limitations: [] }],
    active_lease_ids: [],
  };
}

function remoteDetail(id: string, name: string) {
  const remote = detail(id, name, false, "UNKNOWN");
  return { ...remote, variant: { ...remote.variant, bundle_hash: null }, bundle: null };
}

const storageLocation = {
  id: locationId, row_version: 1,
  metadata: { volume_budget: { usable_bytes: "64424509440", shared_with: [] } },
  name: "Modelos SSD", root_path: "D:\\LARM\\Models", canonical_path: "D:\\LARM\\Models", volume_guid: "volume-guid", drive_letter: "D:", unc_path: null,
  filesystem: "NTFS", storage_type: "LOCAL", capacity_bytes: "100000000000", free_bytes: "70000000000", reserved_bytes: "5000000000", priority: 1, auto_placement: true,
  state: "ONLINE", last_seen_at: "2026-10-23T12:00:00.000Z", volume_key: "volume-guid", performance_class: "SSD",
};

const storageEstimate = {
  download_bytes: "3221225472", final_artifact_bytes: "3221225472", installed_bytes: "3221225472", derived_cache_bytes: "0", temporary_peak_bytes: "0", existing_bytes: "0",
  peak_extra_bytes: "3221225472", required_free_bytes: "3758096384", upper_bound_bytes: "3758096384", confidence: "HIGH", reason: "margin_applied", evidence_refs: ["fixture:space"],
  per_volume: [{ volume_key: "volume-guid", usable_free_bytes: "64424509440", required_free_bytes: "3758096384", eligible: true, reason: "sufficient_space" }],
};

const targetStorageLocation = {
  ...storageLocation, id: targetLocationId, name: "Archivo HDD", root_path: "E:\\LARM\\Models", canonical_path: "E:\\LARM\\Models",
  drive_letter: "E:", volume_guid: "target-volume", volume_key: "target-volume", free_bytes: "90000000000", reserved_bytes: "1000000000",
  metadata: { volume_budget: { usable_bytes: "80000000000", shared_with: [] } },
};

function sourceAccessRoutes(authMode = "TOKEN", accountId: string | null = null, localImport = false) {
  return {
    [`GET /api/v1/sources/${sourceId}`]: [json({
      id: sourceId, name: "Fuente de modelos", adapter_id: sourceBindingId, enabled: true,
      auth_mode: authMode, account_id: accountId, policy: localImport ? { local_import: true } : {},
    })],
    "GET /api/v1/adapters?limit=200": [page([{
      id: "0192aaaa-0000-7000-8000-000000000008", adapter_id: localImport ? "larm.builtin.local-folder" : "fixture.http",
      state: "VALIDATED", trust: localImport ? "VERIFIED" : "UNTRUSTED",
      metadata: { source_bindings: localImport ? [] : [{ id: sourceBindingId, source_type: "HTTP" }] },
    }])],
  };
}

function movePlan() {
  return {
    plan_id: "0192aaaa-0000-7000-8000-000000000016", plan_hash: "d".repeat(64), operation: "model.move", resource_ids: [localId],
    storage_estimate: {
      download_bytes: "0", final_artifact_bytes: "3221225472", installed_bytes: "3221225472", derived_cache_bytes: "0", temporary_peak_bytes: "3221225472",
      existing_bytes: "3221225472", peak_extra_bytes: "3221225472", required_free_bytes: "3758096384", upper_bound_bytes: "3758096384", confidence: "EXACT",
      reason: "registered_file_sizes", evidence_refs: ["fixture:move"], per_volume: [{ volume_key: "target-volume", peak_extra_bytes: "3221225472", margin_bytes: "536870912", other_reservations_bytes: "1000000000", usable_free_bytes: "80000000000", eligible: true, reason: "sufficient_space" }],
    },
    steps: [{ phase: "COPYING", adapter_id: null, request: {} }, { phase: "VERIFIED", adapter_id: null, request: {} }, { phase: "SWITCHED", adapter_id: null, request: {} }],
    expires_at: "2030-01-01T00:00:00.000Z", approval_required: false,
  };
}

function downloadPreview(optionalSelected = false, eligible = true, destinationSelected = false, requestedVariantId = candidateId) {
  const sourceCandidates = optionalSelected ? [] : [{
    source_id: "0192aaaa-0000-7000-8000-000000000018",
    variant_id: "0192aaaa-0000-7000-8000-000000000019",
    revision_id: "0192aaaa-0000-7000-8000-000000000020",
    revision_key: "commit:mirror",
    manifest_hash: "f".repeat(64),
    source_priority: 10,
    confidence: "EXACT",
    authenticated: false,
    https: true,
    artifacts: [{ target_artifact_id: requiredId, source_artifact_id: "0192aaaa-0000-7000-8000-000000000021", relative_path: "model.gguf", sha256: "b".repeat(64), size_bytes: "3221225472" }],
  }];
  return {
    preview_id: "0192aaaa-0000-7000-8000-000000000010", preview_hash: "c".repeat(64), variant_id: requestedVariantId,
    optional_artifact_ids: optionalSelected ? [optionalId] : [],
    artifact_estimates: [
      { artifact_id: requiredId, relative_path: "model.gguf", required: true, selected: true, size_estimate: size("EXACT", "3221225472", "3221225472", "3221225472") },
      { artifact_id: optionalId, relative_path: "config.json", required: false, selected: optionalSelected, size_estimate: size("UNKNOWN", null, null, null) },
    ],
    destination_options: [{ storage_location_id: locationId, selected: destinationSelected, eligible, reason: eligible ? "fits" : "insufficient_space: requiere 3758096384 y hay 0 utilizables", storage_estimate: eligible ? storageEstimate : null }],
    selected_storage_location_id: destinationSelected ? locationId : null, plan_id: destinationSelected && eligible ? "0192aaaa-0000-7000-8000-000000000017" : null, plan_hash: destinationSelected && eligible ? "e".repeat(64) : null, storage_estimate: destinationSelected && eligible ? storageEstimate : null, source_id: sourceId, source_candidates: sourceCandidates, hard_budget_bytes: null, priority: "NORMAL",
    expires_at: "2030-01-01T00:00:00.000Z", operator_confirmation_required: true,
  };
}

function transport(routes: Record<string, RawResponse[]>): Transport & { calls: RawRequest[] } {
  const calls: RawRequest[] = [];
  const fn = (async (request: RawRequest) => {
    calls.push(request);
    const key = `${request.method} ${request.path}`;
    const queue = routes[key];
    if (!queue?.length) throw new Error(`unexpected ${key}`);
    return queue.length > 1 ? queue.shift()! : queue[0];
  }) as Transport & { calls: RawRequest[] };
  fn.calls = calls;
  return fn;
}

function open(t: Transport) { render(<ModelsPage client={new ApiClient(t)} />); }

function confirmationRoutes(outcomes: RawResponse[], prepared = downloadPreview(false, true, true)): Record<string, RawResponse[]> {
  return {
    "GET /api/v1/variants?limit=50": [page([variant(candidateId, "DISCOVERED", "UNKNOWN")])],
    [`GET /api/v1/variants/${candidateId}/detail`]: [json(detail(candidateId, "Qwen candidato", false, "UNKNOWN"))],
    "POST /api/v1/download-plans": [json(downloadPreview()), json(prepared)],
    "GET /api/v1/storage-locations?limit=200": [page([storageLocation])],
    "POST /api/v1/downloads": outcomes,
    ...sourceAccessRoutes("NONE"),
  };
}

async function prepareConfirmation(t: Transport) {
  open(t);
  fireEvent.click(await screen.findByRole("button", { name: "Comparar y preparar descarga" }));
  fireEvent.click(await screen.findByRole("button", { name: /Qwen candidato/ }));
  fireEvent.click(screen.getByRole("button", { name: "Comparar destinos" }));
  fireEvent.click(await screen.findByRole("button", { name: "Preparar plan para este destino" }));
  await screen.findByRole("button", { name: "Preparar plan para este destino" });
}

describe("pantalla Mis modelos", () => {
  it("compara el payload real con límites de ejecución y tamaño de origen desconocido", async () => {
    const first = remoteDetail(localId, "Reference model 0");
    const second = remoteDetail(candidateId, "Reference model 1");
    for (const model of [first, second]) {
      model.artifacts = model.artifacts.slice(0, 1);
      model.artifacts[0].expected_size = null;
      model.artifacts[0].size_estimate = size("UNKNOWN", null, null, null);
    }
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([first.variant, second.variant])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(first)],
      [`GET /api/v1/variants/${candidateId}/detail`]: [json(second)],
    });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Comparar y preparar descarga" }));
    await screen.findByRole("button", { name: /Reference model 0/ });
    for (const check of screen.getAllByRole("checkbox", { name: "Comparar" })) fireEvent.click(check);
    const comparison = within(await screen.findByRole("region", { name: "Comparación de variantes" }));
    expect(within(comparison.getByRole("row", { name: /^Tamaño de descarga/ })).getAllByRole("cell").map((cell) => cell.textContent)).toEqual(["Desconocido", "Desconocido"]);
    expect(t.calls.some((call) => call.method === "POST")).toBe(false);
  });

  it("suma sólo los archivos necesarios, conserva cero exacto y separa memoria de descarga", async () => {
    const first = remoteDetail(localId, "Archivos exactos");
    const second = remoteDetail(candidateId, "Archivo vacío");
    first.variant.model_limits.model_max_memory_bytes = "68719476736";
    first.artifacts[0].size_estimate = size("EXACT", "1024", "1024", "1024");
    first.artifacts.push({ ...first.artifacts[0], id: "second-required", filename: "tokenizer.bin", size_estimate: size("EXACT", "2048", "2048", "2048") });
    second.artifacts = [{ ...second.artifacts[0], size_estimate: size("EXACT", "0", "0", "0") }];
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([first.variant, second.variant])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(first)],
      [`GET /api/v1/variants/${candidateId}/detail`]: [json(second)],
    });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Comparar y preparar descarga" }));
    await screen.findByRole("button", { name: /Archivos exactos/ });
    for (const check of screen.getAllByRole("checkbox", { name: "Comparar" })) fireEvent.click(check);
    const comparison = within(await screen.findByRole("region", { name: "Comparación de variantes" }));
    const row = within(comparison.getByRole("row", { name: /^Tamaño de descarga/ }));
    expect(row.getByText("Exacto · 3,0 KiB")).toBeTruthy();
    expect(row.getByText("Exacto · 0 bytes")).toBeTruthy();
  });

  it("conserva intervalos de origen y no convierte un archivo desconocido en tamaño cero", async () => {
    const first = remoteDetail(localId, "Intervalo");
    const second = remoteDetail(candidateId, "Parcial desconocido");
    first.artifacts = [{ ...first.artifacts[0], size_estimate: size("HIGH", null, "1024", "4096") },
      { ...first.artifacts[0], id: "required-extra", size_estimate: size("EXACT", "1024", "1024", "1024") }];
    second.artifacts[1].required = true;
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([first.variant, second.variant])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(first)],
      [`GET /api/v1/variants/${candidateId}/detail`]: [json(second)],
    });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Comparar y preparar descarga" }));
    await screen.findByRole("button", { name: /Intervalo/ });
    for (const check of screen.getAllByRole("checkbox", { name: "Comparar" })) fireEvent.click(check);
    const comparison = within(await screen.findByRole("region", { name: "Comparación de variantes" }));
    const row = within(comparison.getByRole("row", { name: /^Tamaño de descarga/ }));
    expect(row.getByText("Estimación alta · 2,0 KiB–5,0 KiB")).toBeTruthy();
    expect(row.getByText("Desconocido")).toBeTruthy();
  });

  it.each([
    ["ONLINE", "2097152", "2097152", "Observado localmente · 2,0 MiB"],
    ["OFFLINE", "2097152", "2097152", "Último tamaño local registrado · 2,0 MiB"],
    ["MISSING", "2097152", "2097152", "Último tamaño local registrado · 2,0 MiB"],
    ["COPYING", null, "1048576", "Tamaño local desconocido"],
    ["ONLINE", "2097152", "1048576", "Tamaños locales en conflicto"],
    ["ONLINE", "0", "0", "Observado localmente · 0 bytes"],
  ])("distingue tamaño local y fuente desconocida: %s / %s / %s", async (state, actual, observed, label) => {
    const local = detail(localId, "Modelo observado", true, "UNKNOWN");
    local.artifacts = local.artifacts.slice(0, 1);
    local.artifacts[0].actual_size = actual;
    local.artifacts[0].size_estimate = size("UNKNOWN", null, null, null);
    local.locations = [{ ...local.locations[0], file_size: observed!, state }];
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([local.variant])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(local)],
    });
    open(t);
    const panel = within(await screen.findByRole("region", { name: "Modelo observado" }));
    const row = within(panel.getByRole("row", { name: /model.gguf Necesario/ }));
    expect(row.getByText(label)).toBeTruthy();
    expect(row.getByText(/Desconocido · metadata_missing/)).toBeTruthy();
    expect(row.getByText("Fuente:")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Modelo observado/ }).textContent).toContain(label);
    expect(t.calls.some((call) => call.method === "POST")).toBe(false);
  });

  it("compara un solo tamaño por archivo y señala copias contradictorias o inventario parcial", async () => {
    const first = detail(localId, "Gemma", true, "AVAILABLE");
    const second = detail(candidateId, "Mistral", true, "AVAILABLE");
    first.locations.push({ ...first.locations[0], id: "second-copy" });
    second.locations.push({ ...second.locations[0], id: "different-copy", file_size: "6442450944" });
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([first.variant, second.variant])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(first)],
      [`GET /api/v1/variants/${candidateId}/detail`]: [json(second)],
    });
    open(t);
    await screen.findByRole("button", { name: /Gemma/ });
    for (const check of screen.getAllByRole("checkbox", { name: "Comparar" })) fireEvent.click(check);
    const comparison = within(await screen.findByRole("region", { name: "Comparación de variantes" }));
    expect(comparison.getByText("Observado localmente · 3,0 GiB · 1 de 2 archivos")).toBeTruthy();
    expect(comparison.getByText("Tamaños locales en conflicto")).toBeTruthy();
    expect(comparison.queryByText(/6,0 GiB|9,0 GiB/)).toBeNull();
  });

  it("permite preparar opcionales declarados de un modelo remoto sin bundle físico", async () => {
    const remote = remoteDetail(candidateId, "Qwen candidato");
    const routes = confirmationRoutes([downloadReceipt], downloadPreview(true, true, true));
    routes[`GET /api/v1/variants/${candidateId}/detail`] = [json(remote)];
    routes["POST /api/v1/download-plans"] = [json(downloadPreview(true)), json(downloadPreview(true, true, true))];
    const t = transport(routes);
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Comparar y preparar descarga" }));
    fireEvent.click(await screen.findByRole("button", { name: /Qwen candidato/ }));
    const optional = screen.getByRole("checkbox", { name: "config.json" }) as HTMLInputElement;
    expect(optional.checked).toBe(false);
    fireEvent.click(optional);
    fireEvent.change(screen.getByLabelText(/Presupuesto máximo/), { target: { value: "8,5" } });
    fireEvent.click(screen.getByRole("button", { name: "Comparar destinos" }));
    fireEvent.click(await screen.findByRole("button", { name: "Preparar plan para este destino" }));
    expect(await screen.findByText(/Plan preparado para esta ubicación/)).toBeTruthy();
    const previews = t.calls.filter((call) => call.path === "/api/v1/download-plans");
    expect(previews).toHaveLength(2);
    for (const request of previews) {
      expect(request.body).toMatchObject({ optional_artifact_ids: [optionalId], hard_budget_bytes: "9126805504" });
    }
    expect(screen.getAllByText(/Desconocido · metadata_missing/).length).toBeGreaterThan(0);
    expect(t.calls.some((call) => call.path === "/api/v1/downloads")).toBe(false);
    expect((screen.getByRole("button", { name: "Confirmar y crear descarga" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("descarta el plan y los opcionales anteriores al cambiar de variante", async () => {
    const first = remoteDetail(candidateId, "Qwen candidato");
    const second = remoteDetail(localId, "Gemma candidata");
    const routes = confirmationRoutes([downloadReceipt]);
    routes["GET /api/v1/variants?limit=50"] = [page([first.variant, second.variant])];
    routes[`GET /api/v1/variants/${candidateId}/detail`] = [json(first)];
    routes[`GET /api/v1/variants/${localId}/detail`] = [json(second)];
    routes["POST /api/v1/download-plans"] = [json(downloadPreview(true)), json(downloadPreview(true, true, true)), json(downloadPreview(false, true, false, localId))];
    const t = transport(routes);
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Comparar y preparar descarga" }));
    fireEvent.click(await screen.findByRole("button", { name: /Qwen candidato/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: "config.json" }));
    fireEvent.change(screen.getByLabelText(/Presupuesto máximo/), { target: { value: "8,5" } });
    fireEvent.click(screen.getByRole("button", { name: "Comparar destinos" }));
    fireEvent.click(await screen.findByRole("button", { name: "Preparar plan para este destino" }));
    await screen.findByText(/Plan preparado para esta ubicación/);
    fireEvent.click(screen.getByRole("button", { name: /Gemma candidata/ }));
    expect(screen.queryByText(/Plan preparado para esta ubicación/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Confirmar y crear descarga" })).toBeNull();
    expect((screen.getByLabelText(/Presupuesto máximo/) as HTMLInputElement).value).toBe("");
    expect((screen.getByRole("checkbox", { name: "config.json" }) as HTMLInputElement).checked).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Comparar destinos" }));
    await screen.findByRole("button", { name: "Preparar plan para este destino" });
    const requests = t.calls.filter((call) => call.path === "/api/v1/download-plans");
    expect(requests).toHaveLength(3);
    expect(requests[2].body).toMatchObject({ variant_id: localId, optional_artifact_ids: [], hard_budget_bytes: null });
    expect(t.calls.some((call) => call.path === "/api/v1/downloads")).toBe(false);
  });

  it("separa biblioteca local y comparación, distingue tamaños y nunca inicia una descarga desde el preflight", async () => {
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([variant(localId, "AVAILABLE", "AVAILABLE"), variant(candidateId, "DISCOVERED", "UNKNOWN")])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(detail(localId, "Gemma local", true, "AVAILABLE"))],
      [`GET /api/v1/variants/${candidateId}/detail`]: [json(detail(candidateId, "Qwen candidato", false, "UNKNOWN"))],
      "POST /api/v1/download-plans": [json(downloadPreview(true)), json(downloadPreview(true, true, true))],
      "POST /api/v1/downloads": [downloadReceipt],
      "GET /api/v1/storage-locations?limit=200": [page([storageLocation])],
      ...sourceAccessRoutes(),
    });
    open(t);
    expect(await screen.findByRole("region", { name: "Mis modelos" })).toBeTruthy();
    await screen.findByRole("button", { name: /Gemma local/ });
    expect(screen.queryByRole("button", { name: /Qwen candidato/ })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Comparar y preparar descarga" }));
    fireEvent.click(await screen.findByRole("button", { name: /Qwen candidato/ }));
    expect(screen.getByText("Exacto · 3,0 GiB")).toBeTruthy();
    expect(screen.getByText(/Desconocido · metadata_missing/)).toBeTruthy();

    fireEvent.click(screen.getByLabelText("config.json"));
    fireEvent.change(screen.getByLabelText(/Presupuesto máximo/), { target: { value: "8,5" } });
    fireEvent.click(screen.getByRole("button", { name: "Comparar destinos" }));
    await screen.findByText("Modelos SSD · D:");
    const accessNotice = within(screen.getByRole("region", { name: "Licencia y acceso" }));
    expect(accessNotice.getByText("apache-2.0")).toBeTruthy();
    expect(accessNotice.getByText(/Autenticación prevista: Token/)).toBeTruthy();
    expect(accessNotice.getByText("Confianza del conector: No verificado.")).toBeTruthy();
    expect(accessNotice.getByText(/La ejecución requerirá autorización vigente/)).toBeTruthy();
    const initialRequest = t.calls.find((call) => call.path === "/api/v1/download-plans")!;
    expect(initialRequest.method).toBe("POST");
    expect(initialRequest.body).toMatchObject({ variant_id: candidateId, storage_location_id: null, hard_budget_bytes: "9126805504", optional_artifact_ids: [optionalId] });
    expect(screen.getByText(/Libre 65,2 GiB · reservado 4,7 GiB/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Preparar plan para este destino" }));
    expect(await screen.findByText(/Plan preparado para esta ubicación/)).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Confirmación de descarga" }).parentElement?.textContent).toContain("LARM vuelve a comprobar la fuente, su autorización y el espacio disponible");
    const planRequest = t.calls.filter((call) => call.path === "/api/v1/download-plans")[1];
    expect(planRequest.body).toMatchObject({ variant_id: candidateId, storage_location_id: locationId, hard_budget_bytes: "9126805504", optional_artifact_ids: [optionalId] });

    const confirm = screen.getByRole("button", { name: "Confirmar y crear descarga" }) as HTMLButtonElement;
    expect(confirm.disabled).toBe(false);
    expect(t.calls.some((call) => call.path === "/api/v1/downloads")).toBe(false);
    fireEvent.click(confirm);
    fireEvent.click(confirm);
    expect(await screen.findByRole("link", { name: "Ver descargas" })).toBeTruthy();
    const requests = t.calls.filter((call) => call.path === "/api/v1/downloads");
    expect(requests).toHaveLength(1);
    expect(requests[0].body).toEqual({ plan_id: "0192aaaa-0000-7000-8000-000000000017", plan_hash: "e".repeat(64), operator_confirmed: true });
    expect(requests[0].headers?.["Idempotency-Key"]).toBe(`download-confirm-0192aaaa-0000-7000-8000-000000000017-${"e".repeat(64)}`);
    expect(confirm.disabled).toBe(true);
  });

  it("recupera una respuesta perdida conservando cuerpo y clave, sin permitir cambiar el plan pendiente", async () => {
    const t = transport(confirmationRoutes([{ status: 0, body: null }, downloadReceipt]));
    await prepareConfirmation(t);
    fireEvent.click(screen.getByRole("button", { name: "Confirmar y crear descarga" }));
    expect(await screen.findByText(/No se pudo comprobar si LARM creó la descarga/)).toBeTruthy();
    expect((screen.getByLabelText(/Presupuesto máximo/) as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Preparar plan para este destino" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Comprobar solicitud" }));
    await screen.findByRole("link", { name: "Ver descargas" });
    const requests = t.calls.filter((call) => call.path === "/api/v1/downloads");
    expect(requests).toHaveLength(2);
    expect(requests[1]).toEqual(requests[0]);
  });

  it("impide confirmar un plan caducado", async () => {
    const t = transport(confirmationRoutes([downloadReceipt], { ...downloadPreview(false, true, true), expires_at: "2000-01-01T00:00:00.000Z" }));
    await prepareConfirmation(t);
    await screen.findByText("El plan caducó. Compara de nuevo los destinos antes de confirmar.");
    const button = screen.getByRole("button", { name: "Confirmar y crear descarga" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    fireEvent.click(button);
    expect(t.calls.some((call) => call.path === "/api/v1/downloads")).toBe(false);
  });

  it("conserva el rechazo de autenticación del daemon sin anunciar una descarga creada", async () => {
    const t = transport(confirmationRoutes([{ status: 401, body: { code: "AUTH_REQUIRED", message: "auth", retryable: false, correlation_id: "c", details: { reason: "download_source_auth_unavailable" } } }]));
    await prepareConfirmation(t);
    fireEvent.click(screen.getByRole("button", { name: "Confirmar y crear descarga" }));
    await screen.findByText(/La fuente requiere autenticación/);
    expect(screen.queryByRole("link", { name: "Ver descargas" })).toBeNull();
  });

  it("filtra estados derivados y permite comparar hasta tres modelos locales", async () => {
    const offlineId = "0192aaaa-0000-7000-8000-000000000011";
    const corruptId = "0192aaaa-0000-7000-8000-000000000012";
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([variant(localId, "AVAILABLE", "AVAILABLE"), variant(offlineId, "VERIFIED", "OFFLINE"), variant(corruptId, "CORRUPT", "CORRUPT")])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(detail(localId, "Gemma", true, "AVAILABLE"))],
      [`GET /api/v1/variants/${offlineId}/detail`]: [json(detail(offlineId, "Mistral offline", true, "OFFLINE"))],
      [`GET /api/v1/variants/${corruptId}/detail`]: [json(detail(corruptId, "Llama corrupto", true, "CORRUPT"))],
    });
    open(t);
    await screen.findByRole("button", { name: /Gemma/ });
    fireEvent.click(screen.getByRole("button", { name: "Sin conexión" }));
    expect(await screen.findByRole("button", { name: /Mistral offline/ })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Gemma/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Dañados" }));
    expect(await screen.findByRole("button", { name: /Llama corrupto/ })).toBeTruthy();
  });

  it("compara detalles y estimaciones de dos variantes seleccionadas", async () => {
    const otherId = "0192aaaa-0000-7000-8000-000000000014";
    const first = detail(localId, "Gemma", true, "AVAILABLE");
    const second = detail(otherId, "Mistral", true, "AVAILABLE");
    first.artifacts[0].size_estimate = size("HIGH", "3221225472", "3221225472", "3758096384");
    second.artifacts[0].size_estimate = size("ESTIMATED", "3221225472", "3221225472", "3758096384");
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([variant(localId, "AVAILABLE", "AVAILABLE"), variant(otherId, "VERIFIED", "AVAILABLE")])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(first)],
      [`GET /api/v1/variants/${otherId}/detail`]: [json(second)],
    });
    open(t);
    await screen.findByRole("button", { name: /Gemma/ });
    const checks = screen.getAllByRole("checkbox");
    fireEvent.click(checks[0]); fireEvent.click(checks[1]);
    expect(await screen.findByRole("heading", { name: "Comparación de variantes" })).toBeTruthy();
    expect(screen.getByText("Tamaño de descarga")).toBeTruthy();
    expect(screen.getAllByText("Estimado · 3,0 GiB–3,5 GiB").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Estimación alta · 3,0 GiB–3,5 GiB").length).toBeGreaterThan(0);
  });

  it("carga de forma progresiva las variantes de páginas siguientes", async () => {
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([variant(localId, "AVAILABLE", "AVAILABLE")], "variant-next")],
      "GET /api/v1/variants?limit=50&cursor=variant-next": [page([variant(candidateId, "DISCOVERED", "UNKNOWN")])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(detail(localId, "Gemma local", true, "AVAILABLE"))],
      [`GET /api/v1/variants/${candidateId}/detail`]: [json(detail(candidateId, "Qwen candidato", false, "UNKNOWN"))],
    });
    open(t);
    await screen.findByRole("button", { name: "Cargar más modelos" });
    fireEvent.click(screen.getByRole("button", { name: "Cargar más modelos" }));
    fireEvent.click(screen.getByRole("button", { name: "Comparar y preparar descarga" }));
    expect(await screen.findByRole("button", { name: /Qwen candidato/ })).toBeTruthy();
    expect(t.calls.some((call) => call.path === "/api/v1/variants?limit=50&cursor=variant-next")).toBe(true);
  });

  it("bloquea todos los destinos sin espacio suficiente y no habilita la confirmación", async () => {
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([variant(candidateId, "DISCOVERED", "UNKNOWN")])],
      [`GET /api/v1/variants/${candidateId}/detail`]: [json(detail(candidateId, "Qwen", false, "UNKNOWN"))],
      "POST /api/v1/download-plans": [json(downloadPreview(false, false))],
      "GET /api/v1/storage-locations?limit=200": [page([storageLocation])],
      ...sourceAccessRoutes("NONE"),
    });
    open(t);
    await screen.findByRole("button", { name: "Comparar y preparar descarga" });
    fireEvent.click(screen.getByRole("button", { name: "Comparar y preparar descarga" }));
    fireEvent.click(await screen.findByRole("button", { name: /Qwen/ }));
    fireEvent.click(screen.getByRole("button", { name: "Comparar destinos" }));
    await screen.findByText(/Espacio insuficiente/);
    expect(screen.getByText(/Fuente alternativa 1/)).toBeTruthy();
    expect((screen.getByRole("button", { name: "Confirmar y crear descarga" }) as HTMLButtonElement).disabled).toBe(true);
    expect(t.calls.some((call) => call.path === "/api/v1/downloads")).toBe(false);
  });

  it("lee confianza y validación de la instalación de la fuente local interna", async () => {
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([variant(localId, "AVAILABLE", "AVAILABLE")])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(detail(localId, "Gemma local", true, "AVAILABLE"))],
      "POST /api/v1/download-plans": [json(downloadPreview(false, true, false, localId))],
      "GET /api/v1/storage-locations?limit=200": [page([storageLocation])],
      ...sourceAccessRoutes("NONE", null, true),
    });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: /Gemma local/ }));
    fireEvent.click(screen.getByRole("button", { name: "Comparar destinos" }));
    const notice = within(await screen.findByRole("region", { name: "Licencia y acceso" }));
    expect(notice.getByText("Confianza del conector: Verificado.")).toBeTruthy();
    expect(notice.queryByText(/No se pudo comprobar la confianza/)).toBeNull();
  });

  it("explica la falta de autorización al solicitar el preflight", async () => {
    const forbidden: RawResponse = { status: 403, body: { code: "AUTHORIZATION_FAILED", message: "forbidden", retryable: false, correlation_id: "c", details: { reason: "scope_missing" } } };
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([variant(candidateId, "DISCOVERED", "UNKNOWN")])],
      [`GET /api/v1/variants/${candidateId}/detail`]: [json(detail(candidateId, "Qwen", false, "UNKNOWN"))],
      "POST /api/v1/download-plans": [forbidden],
    });
    open(t);
    await screen.findByRole("button", { name: "Comparar y preparar descarga" });
    fireEvent.click(screen.getByRole("button", { name: "Comparar y preparar descarga" }));
    fireEvent.click(await screen.findByRole("button", { name: /Qwen/ }));
    fireEvent.click(screen.getByRole("button", { name: "Comparar destinos" }));
    expect(await screen.findByText(/Tu cuenta no tiene permiso/)).toBeTruthy();
    expect(t.calls.some((call) => call.path === "/api/v1/downloads")).toBe(false);
  });

  it("encola verificación solo para ubicaciones locales disponibles", async () => {
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([variant(localId, "AVAILABLE", "AVAILABLE")]), page([variant(localId, "AVAILABLE", "AVAILABLE")])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(detail(localId, "Gemma local", true, "AVAILABLE")), json(detail(localId, "Gemma local", true, "AVAILABLE"))],
      [`POST /api/v1/models/${localId}/verify`]: [json({ job_id: "0192aaaa-0000-7000-8000-000000000013", status_url: `/api/v1/jobs/0192aaaa-0000-7000-8000-000000000013`, state: "QUEUED" })],
    });
    open(t);
    await screen.findByRole("button", { name: "Verificar archivos locales" });
    fireEvent.click(screen.getByRole("button", { name: "Verificar archivos locales" }));
    expect(await screen.findByRole("status")).toBeTruthy();
    const verification = t.calls.find((call) => call.method === "POST")!;
    expect(verification.body).toEqual({ resource_id: localId, profile_id: null });
    expect(verification.headers?.["Idempotency-Key"]).toBeTruthy();
  });

  it("prepara el movimiento sin ejecutarlo y exige confirmación explícita antes de consultar el progreso", async () => {
    const plan = movePlan();
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([variant(localId, "AVAILABLE", "AVAILABLE")]), page([variant(localId, "AVAILABLE", "AVAILABLE")])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(detail(localId, "Gemma local", true, "AVAILABLE")), json(detail(localId, "Gemma local", true, "AVAILABLE"))],
      "GET /api/v1/storage-locations?limit=200": [page([storageLocation, targetStorageLocation])],
      [`POST /api/v1/models/${localId}/move`]: [json(plan), json(plan)],
      [`GET /api/v1/jobs/${plan.plan_id}`]: [json({ job_id: plan.plan_id, state: "COMPLETED", resource_type: "model_variant", resource_id: localId, progress: { phase: "COMPLETED", bytes_total: "3221225472", bytes_copied: "3221225472" }, result: {}, error: null, evidence_refs: [] })],
    });
    open(t);
    await screen.findByRole("button", { name: "Verificar archivos locales" });
    fireEvent.click(screen.getByRole("button", { name: "Mover a otro disco" }));
    fireEvent.click(await screen.findByRole("button", { name: "Preparar movimiento" }));
    await screen.findByRole("heading", { name: "Plan de movimiento" });
    const moveCalls = () => t.calls.filter((call) => call.path === `/api/v1/models/${localId}/move`);
    expect(moveCalls()).toHaveLength(1);
    expect(moveCalls()[0].body).toEqual({ target_storage_location_id: targetLocationId, plan_hash: null, dry_run: true, approval_id: null });
    expect((screen.getByRole("button", { name: "Confirmar y mover" }) as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(screen.getByLabelText(/Confirmo mover estos archivos/));
    fireEvent.click(screen.getByRole("button", { name: "Confirmar y mover" }));
    expect(await screen.findByText("El movimiento terminó correctamente. El inventario local se ha actualizado.")).toBeTruthy();
    expect(moveCalls()).toHaveLength(2);
    expect(moveCalls()[1].body).toEqual({ target_storage_location_id: targetLocationId, plan_hash: "d".repeat(64), dry_run: false, approval_id: null });
    expect(t.calls.some((call) => call.path === `/api/v1/jobs/${plan.plan_id}`)).toBe(true);
  });

  it("bloquea preparar un movimiento mientras el modelo tiene sesiones activas", async () => {
    const activeDetail = { ...detail(localId, "Gemma local", true, "AVAILABLE"), active_lease_ids: ["0192aaaa-0000-7000-8000-000000000017"] };
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([variant(localId, "AVAILABLE", "AVAILABLE")])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(activeDetail)],
      "GET /api/v1/storage-locations?limit=200": [page([storageLocation, targetStorageLocation])],
    });
    open(t);
    await screen.findByRole("button", { name: "Mover a otro disco" });
    fireEvent.click(screen.getByRole("button", { name: "Mover a otro disco" }));
    expect(await screen.findByText(/Hay 1 sesión\(es\) activas/)).toBeTruthy();
    expect((await screen.findByRole("button", { name: "Preparar movimiento" }) as HTMLButtonElement).disabled).toBe(true);
    expect(t.calls.some((call) => call.method === "POST" && call.path.endsWith("/move"))).toBe(false);
  });

  it("analiza el uso por categorías y solo prepara una vista previa de limpieza", async () => {
    const analysis = {
      storage_location_id: locationId, volume_key: "volume-guid", free_bytes: "70000000000", reserved_bytes: "5000000000",
      categories: [
        { kind: "MODELS", used_bytes: "3221225472", reclaimable_bytes: "0", resource_ids: [localId], blocker_codes: [] },
        { kind: "CACHES", used_bytes: "2147483648", reclaimable_bytes: "2147483648", resource_ids: [], blocker_codes: [] },
      ], snapshot_at: "2026-10-23T12:00:00.000Z",
    };
    const plan = {
      ...movePlan(), operation: "storage.cleanup", approval_required: true, storage_estimate: { ...movePlan().storage_estimate, reason: "libera 2147483648 bytes" },
      steps: [{ phase: "DELETE", adapter_id: null, request: { category: "CACHES", relative_path: "cache/derived/old.bin", size_bytes: "2147483648", reason: "regenerable_cache" } }],
    };
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([])],
      "GET /api/v1/storage-locations?limit=200": [page([storageLocation])],
      [`GET /api/v1/storage-locations/${locationId}/analysis`]: [json(analysis)],
      [`POST /api/v1/storage-locations/${locationId}/cleanup-plan`]: [json(plan)],
    });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Analizar discos" }));
    expect(await screen.findByRole("table", { name: "Uso observado por categoría" })).toBeTruthy();
    expect(screen.getByText("Cachés")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Preparar vista previa de limpieza" }));
    expect(await screen.findByText(/cache\/derived\/old.bin/)).toBeTruthy();
    expect(screen.getByText(/aquí solo se muestra; no se borra ningún archivo/)).toBeTruthy();
    expect(t.calls.some((call) => call.path.endsWith("/cleanup"))).toBe(false);
  });

  it("revisa la operación de importación y no crea el trabajo hasta la confirmación", async () => {
    const importJobId = "0192aaaa-0000-7000-8000-000000000018";
    const preview = {
      files_seen: 2, recognized_files: 1, bytes_seen: "16", recognized_bytes: "11",
      variants: [{
        dir: "", name: "fixture", format: "GGUF", quantization: "Q4_K_M", architecture: null,
        size_bytes: "11", files: [{ relative_path: "fixture.gguf", kind: "gguf", role: "weights", required: true, size_bytes: "11", facts: {} }],
      }],
      ignored: [{ relative_path: "notes.txt", reason: "unknown_format" }], skipped: [],
    };
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([]), page([])],
      "GET /api/v1/storage-locations?limit=200": [page([storageLocation])],
      "POST /api/v1/models/import/preview": [json(preview)],
      "POST /api/v1/models/import": [json({ job_id: importJobId, status_url: `/api/v1/jobs/${importJobId}`, state: "QUEUED" })],
      [`GET /api/v1/jobs/${importJobId}`]: [json({ job_id: importJobId, status_url: `/api/v1/jobs/${importJobId}`, state: "COMPLETED", progress: {}, result: { variants: [{}], hashed: 3, ignored: [], skipped: [] }, error: null })],
    });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Importar carpeta" }));
    fireEvent.change(await screen.findByLabelText("Ruta de la carpeta local"), { target: { value: "C:\\Modelos\\fixture" } });
    fireEvent.change(screen.getByLabelText("Cómo importar"), { target: { value: "COPY" } });
    fireEvent.click(screen.getByRole("button", { name: "Previsualizar carpeta" }));
    expect(await screen.findByRole("heading", { name: "Vista previa de solo lectura" })).toBeTruthy();
    expect(screen.getByText(/creará una fuente local para esta carpeta o reutilizará la existente/)).toBeTruthy();
    expect(screen.getByText("fixture.gguf")).toBeTruthy();
    expect(screen.getByText(/11 bytes únicos/)).toBeTruthy();
    expect(screen.getByText("Copiar y registrar como gestionado por LARM")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Iniciar importación" }) as HTMLButtonElement).disabled).toBe(true);
    expect(t.calls.some((call) => call.path === "/api/v1/models/import")).toBe(false);
    const previewRequest = t.calls.find((call) => call.path === "/api/v1/models/import/preview")!;
    expect(previewRequest.body).toEqual({ source_path: "C:\\Modelos\\fixture" });
    expect(previewRequest.headers?.["Idempotency-Key"]).toBeUndefined();
    fireEvent.click(screen.getByLabelText("Confirmo iniciar la importación de 1 variante en el destino elegido."));
    fireEvent.click(screen.getByRole("button", { name: "Iniciar importación" }));
    expect(await screen.findByText("1 variante detectada · 3 archivos verificados · 0 ignorados · 0 omitidos")).toBeTruthy();
    const request = t.calls.find((call) => call.path === "/api/v1/models/import")!;
    expect(request.body).toEqual({ source_path: "C:\\Modelos\\fixture", storage_location_id: locationId, mode: "COPY", format: null, license_id: null });
    expect(request.headers?.["Idempotency-Key"]).toBeTruthy();
  });

  it("filtra formatos detectados y confirma solo las variantes seleccionadas", async () => {
    const preview = {
      files_seen: 2, recognized_files: 2, bytes_seen: "24", recognized_bytes: "24",
      variants: [
        { dir: "", name: "fixture-gguf", format: "GGUF", quantization: null, architecture: null, size_bytes: "11", files: [] },
        { dir: "", name: "fixture-safe", format: "SafeTensors", quantization: null, architecture: null, size_bytes: "13", files: [] },
      ],
      ignored: [], skipped: [],
    };
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([]), page([])],
      "GET /api/v1/storage-locations?limit=200": [page([storageLocation])],
      "POST /api/v1/models/import/preview": [json(preview)],
      "POST /api/v1/models/import": [json({ job_id: "0192aaaa-0000-7000-8000-000000000020", status_url: "/api/v1/jobs/0192aaaa-0000-7000-8000-000000000020", state: "QUEUED" })],
    });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Importar carpeta" }));
    fireEvent.change(await screen.findByLabelText("Ruta de la carpeta local"), { target: { value: "C:\\Modelos\\mixto" } });
    fireEvent.click(screen.getByRole("button", { name: "Previsualizar carpeta" }));
    await screen.findByRole("heading", { name: "Vista previa de solo lectura" });
    expect(screen.getByLabelText("Formato a importar (opcional)")).toBeTruthy();
    expect(screen.getByLabelText("Confirmo iniciar la importación de 2 variantes en el destino elegido.")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Formato a importar (opcional)"), { target: { value: "GGUF" } });
    expect(screen.getByText("fixture-safe")).toBeTruthy();
    expect(screen.getByText(/Excluida por el filtro/)).toBeTruthy();
    fireEvent.click(screen.getByLabelText("Confirmo iniciar la importación de 1 variante en el destino elegido."));
    fireEvent.click(screen.getByRole("button", { name: "Iniciar importación" }));
    const request = t.calls.find((call) => call.path === "/api/v1/models/import")!;
    expect(request.body).toMatchObject({ format: "GGUF", mode: "ADOPT" });
  });

  it("invalida el preview y la confirmación al cambiar la carpeta", async () => {
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([]), page([])],
      "GET /api/v1/storage-locations?limit=200": [page([storageLocation])],
      "POST /api/v1/models/import/preview": [json({
        files_seen: 1, recognized_files: 1, bytes_seen: "4", recognized_bytes: "4",
        variants: [{ dir: "", name: "fixture", format: "GGUF", quantization: null, architecture: null, size_bytes: "4", files: [] }],
        ignored: [], skipped: [],
      })],
    });
    open(t);
    fireEvent.click(await screen.findByRole("button", { name: "Importar carpeta" }));
    const source = await screen.findByLabelText("Ruta de la carpeta local");
    fireEvent.change(source, { target: { value: "C:\\Modelos\\fixture" } });
    fireEvent.click(screen.getByRole("button", { name: "Previsualizar carpeta" }));
    await screen.findByRole("heading", { name: "Vista previa de solo lectura" });
    fireEvent.click(screen.getByLabelText("Confirmo iniciar la importación de 1 variante en el destino elegido."));
    fireEvent.change(source, { target: { value: "C:\\Modelos\\otra" } });
    expect(screen.queryByRole("heading", { name: "Vista previa de solo lectura" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Iniciar importación" })).toBeNull();
    expect(t.calls.some((call) => call.path === "/api/v1/models/import")).toBe(false);
  });

  it("resume las acciones por archivo y exige confirmación más aprobación ligada al hash antes de borrar", async () => {
    const approvalId = "0192aaaa-0000-7000-8000-000000000019";
    const plan = {
      ...movePlan(), operation: "model.delete", approval_required: true,
      steps: [
        { phase: "DELETE_FILE", adapter_id: null, request: { relative_path: "models/gemma/model.gguf" } },
        { phase: "KEEP_SHARED", adapter_id: null, request: { relative_path: "models/shared/config.json" } },
      ],
    };
    const t = transport({
      "GET /api/v1/variants?limit=50": [page([variant(localId, "AVAILABLE", "AVAILABLE")]), page([])],
      [`GET /api/v1/variants/${localId}/detail`]: [json(detail(localId, "Gemma local", true, "AVAILABLE"))],
      [`POST /api/v1/models/${localId}/delete-plan`]: [json(plan)],
      "POST /api/v1/approvals": [json({ resource_id: approvalId, row_version: 1, event_id: "1" })],
      [`DELETE /api/v1/models/${localId}?approval_id=${approvalId}&plan_hash=${plan.plan_hash}`]: [{ status: 204, body: null }],
    });
    open(t);
    await screen.findByRole("button", { name: "Eliminar modelo local" });
    fireEvent.click(screen.getByRole("button", { name: "Eliminar modelo local" }));
    fireEvent.click(screen.getByRole("button", { name: "Preparar plan de borrado" }));
    expect(await screen.findByText(/Eliminar archivo gestionado por LARM/)).toBeTruthy();
    expect(screen.getByText(/Conservar el archivo compartido/)).toBeTruthy();
    expect((screen.getByRole("button", { name: "Aprobar y borrar según el plan" }) as HTMLButtonElement).disabled).toBe(true);
    expect(t.calls.some((call) => call.method === "DELETE")).toBe(false);
    fireEvent.click(screen.getByLabelText("He revisado cada acción y confirmo solicitar el borrado de esta variante."));
    fireEvent.click(screen.getByRole("button", { name: "Aprobar y borrar según el plan" }));
    await waitFor(() => expect(t.calls.some((call) => call.method === "DELETE")).toBe(true));
    const approval = t.calls.find((call) => call.path === "/api/v1/approvals")!;
    expect(approval.body).toEqual({ operation_digest: plan.plan_hash, resource_ids: plan.resource_ids, code_hash: null, permissions: {}, approved: true });
    const deletion = t.calls.find((call) => call.method === "DELETE")!;
    expect(deletion.headers?.["If-Match"]).toBe("1");
    expect(deletion.path).toContain(`plan_hash=${plan.plan_hash}`);
  });
});
