import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import { useToasts } from "./components";
import { formatBytes } from "./hardware";
import { gibToBytes, type StorageLocation } from "./storage";
import { MoveModel } from "./move-model";
import { ModelStorageTools } from "./model-storage";
import { ModelImport } from "./model-import";
import { ModelDelete } from "./model-delete";
import { ModelTransform } from "./model-transform";
import { ModelPromote } from "./model-promote";

interface Page<T> { items: T[]; next_cursor: string | null; snapshot_at: string }
interface ModelLimits {
  parameter_count: string | null;
  layers: number | null;
  kv_heads: number | null;
  head_dim: number | null;
  context_limit: number | null;
  kv_bits: number | null;
  model_max_memory_bytes: string | null;
  architecture_metadata_hash: string | null;
}
interface ModelVariant {
  id: string;
  row_version: number;
  revision_id: string;
  format: string | null;
  quantization: string | null;
  architecture: string | null;
  bundle_hash: string | null;
  lifecycle: string;
  availability: string;
  ownership: "LARM_MANAGED" | "HOST_MANAGED" | "EXTERNAL" | "SHARED";
  promotion: string;
  capabilities: string[];
  model_limits: ModelLimits;
  update_available: boolean;
  status: string;
}
interface ModelArtifact {
  id: string;
  filename: string;
  required: boolean;
  actual_size: string | null;
  expected_size: string | null;
  size_estimate: ArtifactSizeEstimate;
  sha256: string | null;
}
interface ArtifactSizeEstimate {
  confidence: "EXACT" | "HIGH" | "ESTIMATED" | "UNKNOWN";
  bytes: string | null;
  lower_bound_bytes: string | null;
  upper_bound_bytes: string | null;
  source: string;
  evidence_refs: string[];
  reason: string | null;
}
export interface ModelDetail {
  variant: ModelVariant;
  revision: { id: string; revision_key: string; state: string; upstream_commit: string | null };
  distribution: { id: string; source_id: string; upstream_namespace: string; upstream_model_id: string; license_id: string | null };
  canonical_model: { id: string; name: string; capabilities: string[] };
  bundle: { id: string; state: string; required_artifact_ids: string[]; optional_artifact_ids: string[] } | null;
  artifacts: ModelArtifact[];
  locations: ArtifactLocation[];
  compatibility: Array<{ status: string; technical_fit: number | null; limitations: string[] }>;
  active_lease_ids: string[];
}
interface ArtifactLocation {
  id: string;
  artifact_id: string;
  storage_location_id: string;
  relative_path: string;
  file_size: string | null;
  sha256: string | null;
  state: "ONLINE" | "OFFLINE" | "CORRUPT" | "MISSING" | "COPYING" | "UNKNOWN";
  ownership: string;
}
interface StorageEstimate {
  download_bytes: string | null;
  final_artifact_bytes: string | null;
  installed_bytes: string | null;
  required_free_bytes: string | null;
  upper_bound_bytes: string | null;
  confidence: string;
  reason: string;
  per_volume: Array<{ volume_key: string; usable_free_bytes: string; required_free_bytes: string; eligible: boolean; reason: string }>;
}
interface DownloadPlanResponse {
  preview_id: string;
  preview_hash: string;
  variant_id: string;
  optional_artifact_ids: string[];
  artifact_estimates: Array<{ artifact_id: string; relative_path: string; required: boolean; selected: boolean; size_estimate: ArtifactSizeEstimate }>;
  destination_options: Array<{ storage_location_id: string; selected: boolean; eligible: boolean; reason: string; storage_estimate: StorageEstimate | null }>;
  selected_storage_location_id: string | null;
  plan_id: string | null;
  plan_hash: string | null;
  storage_estimate: StorageEstimate | null;
  source_id: string | null;
  source_candidates: Array<{
    source_id: string;
    variant_id: string;
    revision_id: string;
    revision_key: string;
    manifest_hash: string;
    source_priority: number;
    confidence: string;
    authenticated: boolean;
    https: boolean;
    artifacts: Array<{ target_artifact_id: string; source_artifact_id: string; relative_path: string; sha256: string; size_bytes: string }>;
  }>;
  hard_budget_bytes: string | null;
  priority: "BACKGROUND" | "NORMAL" | "HIGH";
  expires_at: string;
  operator_confirmation_required: boolean;
}
interface DownloadReceipt { job_id: string; status_url: string; state: string }
interface SourceAccessConfig {
  adapter_id: string;
  enabled: boolean;
  auth_mode: string;
  account_id: string | null;
  policy: Record<string, unknown>;
}
interface SourceInstallation {
  adapter_id: string;
  state: string;
  trust: string;
  metadata: { source_bindings?: unknown };
}
interface SourceAccessSnapshot {
  source: SourceAccessConfig | null;
  installation: Pick<SourceInstallation, "state" | "trust"> | null;
}

const FILTERS = [
  ["ALL", "Todos"], ["AVAILABLE", "Disponibles"], ["OFFLINE", "Sin conexión"],
  ["CORRUPT", "Dañados"], ["UPDATE", "Con actualización"], ["EXTERNAL", "Externos"],
] as const;
type Filter = typeof FILTERS[number][0];
const AVAILABILITY: Record<string, string> = { AVAILABLE: "Disponible", OFFLINE: "Sin conexión", UNKNOWN: "Sin verificar", INCOMPATIBLE: "Incompatible", PREPARING: "Preparando", DEGRADED: "Degradado", CORRUPT: "Dañado" };
const LIFECYCLE: Record<string, string> = { DISCOVERED: "Detectado", SELECTED: "Seleccionado", DOWNLOAD_PLANNED: "Descarga planificada", DOWNLOADING: "Descargando", DOWNLOADED: "Descargado", VERIFIED: "Verificado", PREPARED: "Preparado", TECHNICALLY_VALIDATED: "Validado técnicamente", AVAILABLE: "Disponible", RUNNING: "En ejecución", FAILED: "Fallido", REJECTED: "Rechazado", CORRUPT: "Dañado" };
const OWNERSHIP: Record<string, string> = { LARM_MANAGED: "Gestionado por LARM", HOST_MANAGED: "Gestionado por el host", EXTERNAL: "Externo", SHARED: "Compartido" };

function friendly(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.isOffline) return "Se perdió la conexión con LARM. Reintenta cuando vuelva a estar disponible.";
    if (error.status === 403) return "Tu cuenta no tiene permiso para realizar esta operación.";
    if (error.reason === "download_execution_unavailable") return "El plan está listo, pero la ejecución espera una fuente y un worker de descarga cualificados. No se creó ningún trabajo.";
    if (error.code === "AUTH_REQUIRED") return "La fuente requiere autenticación. Revisa su cuenta y credenciales en Fuentes antes de volver a confirmar.";
    if (error.code === "LICENSE_REQUIRED") return "La descarga requiere una licencia pendiente de autorización. Revisa las condiciones de la fuente.";
    if (error.reason?.startsWith("conformance_") || error.reason === "source_adapter_unqualified") return "La validación del conector no está vigente. Vuelve a validarlo en Fuentes antes de confirmar.";
    if (error.reason === "download_plan_changed" || error.reason === "plan_expired") return "El plan cambió o caducó. Vuelve a comparar destinos y prepara un plan nuevo.";
    if (error.code === "STORAGE_INSUFFICIENT") return "El destino ya no tiene espacio suficiente. Vuelve a comparar las ubicaciones disponibles.";
  }
  return "No se pudo completar la operación. Puedes volver a intentarlo.";
}

async function allPages<T>(client: ApiClient, base: string): Promise<T[]> {
  const result: T[] = [];
  let cursor: string | null = null;
  for (let page = 0; page < 20; page += 1) {
    const queryString: string = cursor ? `${base}&cursor=${encodeURIComponent(cursor)}` : base;
    const response: Page<T> = await client.get<Page<T>>(queryString);
    result.push(...response.items);
    cursor = response.next_cursor;
    if (!cursor) return result;
  }
  return result;
}

async function mapLimited<T, R>(values: T[], limit: number, fn: (value: T) => Promise<R>): Promise<R[]> {
  const result: R[] = [];
  for (let start = 0; start < values.length; start += limit) {
    result.push(...await Promise.all(values.slice(start, start + limit).map(fn)));
  }
  return result;
}

function availabilityOf(detail: ModelDetail): string {
  if (detail.locations.some((item) => item.state === "CORRUPT") || detail.variant.availability === "CORRUPT") return "CORRUPT";
  if (detail.locations.length > 0 && detail.locations.every((item) => item.state === "OFFLINE" || item.state === "MISSING")) return "OFFLINE";
  return detail.variant.availability;
}

function formatEstimate(value: Pick<ArtifactSizeEstimate, "confidence" | "bytes" | "lower_bound_bytes" | "upper_bound_bytes" | "reason">): string {
  if (value.confidence === "EXACT" && value.bytes !== null) return `Exacto · ${formatBytes(value.bytes)}`;
  if (value.confidence === "UNKNOWN") return `Desconocido${value.reason ? ` · ${value.reason}` : ""}`;
  const range = value.lower_bound_bytes !== null && value.upper_bound_bytes !== null
    ? `${formatBytes(value.lower_bound_bytes)}–${formatBytes(value.upper_bound_bytes)}`
    : value.bytes !== null ? formatBytes(value.bytes) : "Sin cota disponible";
  return `${value.confidence === "HIGH" ? "Estimación alta" : "Estimado"} · ${range}`;
}

function explainDestination(reason: string): string {
  if (reason === "fits") return "Cabe en el espacio utilizable observado.";
  if (reason === "size_unknown") return "Tamaño desconocido: indica un presupuesto duro para continuar.";
  if (reason === "state_offline") return "El disco está desconectado.";
  if (reason === "state_read_only") return "La ubicación es de solo lectura.";
  if (reason === "state_low_space") return "La ubicación tiene poco espacio disponible.";
  if (reason === "state_missing") return "No se encuentra la carpeta de destino.";
  if (reason === "volume_identity_unknown") return "No se pudo confirmar el disco que contiene esta ubicación.";
  if (reason === "fat32_file_too_large") return "El sistema de archivos no admite un archivo de este tamaño.";
  const insufficient = /^insufficient_space: requiere (\d+) y hay (\d+) utilizables/.exec(reason);
  if (insufficient) return `Espacio insuficiente: requiere ${formatBytes(insufficient[1])} y hay ${formatBytes(insufficient[2])} utilizables.`;
  return reason || "No se pudo determinar si la ubicación es adecuada.";
}

interface LocalSize { bytes: string | null; status: "observed" | "recorded" | "conflict" | "unknown" }

function artifactLocalSize(artifact: ModelArtifact, locations: ArtifactLocation[]): LocalSize {
  // A copying/corrupt file is not a complete observation. actual_size remains
  // historical unless a complete online copy corroborates it.
  const complete = locations.filter((location) => location.artifact_id === artifact.id && location.state !== "COPYING" && location.state !== "CORRUPT");
  const sizes = new Set(complete.flatMap((location) => location.file_size === null ? [] : [BigInt(location.file_size).toString()]));
  if (artifact.actual_size !== null) sizes.add(BigInt(artifact.actual_size).toString());
  if (sizes.size > 1) return { bytes: null, status: "conflict" };
  const bytes = sizes.values().next().value ?? null;
  if (bytes === null) return { bytes: null, status: "unknown" };
  return { bytes, status: complete.some((location) => location.state === "ONLINE" && location.file_size !== null) ? "observed" : "recorded" };
}

function localSizeLabel(size: LocalSize): string {
  if (size.status === "conflict") return "Tamaños locales en conflicto";
  if (size.bytes === null) return "Tamaño local desconocido";
  return `${size.status === "observed" ? "Observado localmente" : "Último tamaño local registrado"} · ${formatBytes(size.bytes)}`;
}

function modelLocalSizeLabel(detail: ModelDetail): string {
  const sizes = detail.artifacts.map((artifact) => artifactLocalSize(artifact, detail.locations));
  if (sizes.some((size) => size.status === "conflict")) return localSizeLabel({ bytes: null, status: "conflict" });
  const known = sizes.filter((size) => size.bytes !== null);
  if (known.length === 0) return localSizeLabel({ bytes: null, status: "unknown" });
  const bytes = known.reduce((sum, size) => sum + BigInt(size.bytes!), 0n).toString();
  const status = known.every((size) => size.status === "observed") ? "observed" : "recorded";
  const coverage = known.length < sizes.length ? ` · ${known.length} de ${sizes.length} archivos` : "";
  return `${localSizeLabel({ bytes, status })}${coverage}`;
}

function formatVariantSize(detail: ModelDetail): string {
  // model_limits describes execution, not storage. Compare the required
  // upstream files, retaining unknown sizes and the uncertainty of bounds.
  const estimates = detail.artifacts.filter((artifact) => artifact.required).map((artifact) => artifact.size_estimate);
  if (estimates.length === 0) return "Sin archivos necesarios informados";
  if (estimates.some((estimate) => estimate.confidence === "UNKNOWN")) return "Desconocido";
  const sum = (field: "bytes" | "lower_bound_bytes" | "upper_bound_bytes"): string | null => {
    const values = estimates.map((estimate) => estimate[field]);
    return values.some((value) => value == null) ? null : values.reduce((total, value) => total + BigInt(value!), 0n).toString();
  };
  const confidence = estimates.every((estimate) => estimate.confidence === "EXACT") ? "EXACT"
    : estimates.some((estimate) => estimate.confidence === "ESTIMATED") ? "ESTIMATED" : "HIGH";
  const bytes = sum("bytes");
  if (confidence === "EXACT" && bytes === null) return "Desconocido";
  return formatEstimate({ confidence, bytes, lower_bound_bytes: sum("lower_bound_bytes"), upper_bound_bytes: sum("upper_bound_bytes"), reason: null });
}

function DetailSize({ estimate }: { estimate: ArtifactSizeEstimate }) {
  return <span className={`size-confidence size-${estimate.confidence.toLowerCase()}`} title={estimate.reason ?? undefined}>{formatEstimate(estimate)}</span>;
}

async function readSourceAccess(client: ApiClient, sourceId: string): Promise<SourceAccessSnapshot> {
  const source = await client.get<SourceAccessConfig>(`/api/v1/sources/${encodeURIComponent(sourceId)}`).catch(() => null);
  if (!source) return { source: null, installation: null };
  const installations = await allPages<SourceInstallation>(client, "/api/v1/adapters?limit=200").catch(() => []);
  const installation = source.policy.local_import === true
    ? installations.find((item) => item.adapter_id === "larm.builtin.local-folder")
    : installations.find((item) => {
      const bindings = item.metadata?.source_bindings;
      return Array.isArray(bindings) && bindings.some((binding: unknown) =>
        typeof binding === "object" && binding !== null && "id" in binding
        && typeof binding.id === "string" && binding.id === source.adapter_id);
    });
  return {
    source,
    installation: installation ? { state: installation.state, trust: installation.trust } : null,
  };
}

function authModeLabel(mode: string): string {
  const labels: Record<string, string> = {
    NONE: "Sin autenticación configurada", TOKEN: "Token", BEARER: "Bearer", API_TOKEN: "Token de API",
    OAUTH_DEVICE: "OAuth con dispositivo", OAUTH_AUTH_CODE_PKCE: "OAuth con navegador", BASIC: "Usuario y contraseña",
    SSH: "SSH", COOKIE: "Cookie", GIT_LFS: "Git LFS", WINDOWS_INTEGRATED: "Cuenta de Windows",
  };
  return labels[mode] ?? "Autenticación desconocida";
}

function trustLabel(trust: string): string {
  if (trust === "VERIFIED") return "Verificado";
  if (trust === "REVOKED") return "Revocado";
  if (trust === "UNTRUSTED") return "No verificado";
  return "Desconocido";
}

function SourceAccessNotice({ detail, access }: { detail: ModelDetail; access: SourceAccessSnapshot }) {
  const source = access.source;
  const installation = access.installation;
  return <section className="source-access-notice" aria-labelledby={`source-access-${detail.variant.id}`}>
    <h4 id={`source-access-${detail.variant.id}`}>Licencia y acceso</h4>
    <p>{detail.distribution.license_id
      ? <>Licencia declarada: <strong>{detail.distribution.license_id}</strong>. Revisa sus condiciones; este preflight no registra su aceptación.</>
      : "El catálogo no informa la licencia. Comprueba las condiciones de la fuente antes de descargar."}</p>
    {!source
      ? <p className="state-unknown">No se pudo comprobar la configuración de acceso de la fuente.</p>
      : <>
        {!source.enabled && <p className="state-unknown">La fuente está desactivada.</p>}
        {source.auth_mode === "NONE"
          ? <p>Fuente configurada sin autenticación. El acceso anónimo del servicio de origen no se ha comprobado.</p>
          : <p className="state-unknown">Autenticación prevista: {authModeLabel(source.auth_mode)}. {source.account_id ? "Hay una cuenta vinculada; la credencial todavía debe validarse." : "No hay una cuenta vinculada; el acceso puede estar restringido."}</p>}
        {!installation
          ? <p className="state-unknown">No se pudo comprobar la confianza ni la validación del conector.</p>
          : <>
            <p>Confianza del conector: {trustLabel(installation.trust)}.</p>
            {installation.state !== "VALIDATED" && <p className="state-unknown">El conector aún no está validado.</p>}
            {installation.trust === "REVOKED" && <p className="state-unknown">La confianza del conector está revocada.</p>}
            {installation.trust !== "VERIFIED" && installation.trust !== "REVOKED" && <p className="state-unknown">La ejecución requerirá autorización vigente para este conector.</p>}
          </>}
      </>}
    <p className="detail">El preflight compara tamaños y destinos; no prueba credenciales ni inicia transferencias.</p>
  </section>;
}

function DownloadPreflight({ client, detail }: { client: ApiClient; detail: ModelDetail }) {
  const [selectedOptional, setSelectedOptional] = useState<string[]>([]);
  const [preview, setPreview] = useState<DownloadPlanResponse | null>(null);
  const [storageLocations, setStorageLocations] = useState<StorageLocation[]>([]);
  const [destination, setDestination] = useState<string>("");
  const [hardBudgetGiB, setHardBudgetGiB] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sourceAccess, setSourceAccess] = useState<SourceAccessSnapshot | null>(null);
  const [creating, setCreating] = useState(false);
  const [receipt, setReceipt] = useState<DownloadReceipt | null>(null);
  const [uncertain, setUncertain] = useState(false);
  const [clock, setClock] = useState(() => Date.now());
  const pending = useRef(false);
  const optionalArtifacts = detail.artifacts.filter((artifact) => !artifact.required
    && (!detail.bundle || detail.bundle.optional_artifact_ids.includes(artifact.id)));

  const run = async (selectedDestinationId: string | null = null) => {
    if (pending.current || uncertain) return;
    const normalizedBudget = hardBudgetGiB.trim() ? gibToBytes(hardBudgetGiB) : null;
    if (hardBudgetGiB.trim() && normalizedBudget === null) {
      setError("Escribe un presupuesto máximo válido en GiB, por ejemplo 8 o 8,5.");
      return;
    }
    pending.current = true; setLoading(true); setError(null); setReceipt(null);
    try {
      const [response, locations, access] = await Promise.all([client.requestRaw<DownloadPlanResponse>({
        method: "POST", path: "/api/v1/download-plans",
        headers: { "Idempotency-Key": `download-preview-${crypto.randomUUID()}` },
        body: {
          variant_id: detail.variant.id, storage_location_id: selectedDestinationId, hard_budget_bytes: normalizedBudget,
          source_id: detail.distribution.source_id, priority: "NORMAL", optional_artifact_ids: selectedOptional,
        },
      }), allPages<StorageLocation>(client, "/api/v1/storage-locations?limit=200"), readSourceAccess(client, detail.distribution.source_id)]);
      setStorageLocations(locations);
      setPreview(response.body);
      setSourceAccess(access);
      setDestination(response.body.selected_storage_location_id ?? response.body.destination_options.find((option) => option.eligible)?.storage_location_id ?? "");
    } catch (e) { setError(friendly(e)); }
    finally { pending.current = false; setLoading(false); }
  };

  const selectedOption = preview?.destination_options.find((option) => option.storage_location_id === destination) ?? null;
  const selectedLocationEstimate = selectedOption?.storage_estimate ?? null;
  const planMatchesDestination = Boolean(preview?.plan_id && preview.plan_hash && preview.selected_storage_location_id === destination && selectedOption?.eligible && selectedOption.selected);
  const expiresAt = Date.parse(preview?.expires_at ?? "");
  const planExpired = !Number.isFinite(expiresAt) || expiresAt <= clock;
  const controlsLocked = loading || creating || uncertain;

  useEffect(() => {
    if (!preview) return;
    const remaining = Date.parse(preview.expires_at) - Date.now();
    const timer = window.setTimeout(() => setClock(Date.now()), Number.isFinite(remaining) ? Math.max(0, Math.min(remaining, 2_147_483_647)) : 0);
    return () => window.clearTimeout(timer);
  }, [preview]);

  const confirm = async () => {
    if (pending.current || receipt || !preview?.plan_id || !preview.plan_hash || !planMatchesDestination) return;
    if (!uncertain && (!Number.isFinite(expiresAt) || expiresAt <= Date.now())) {
      setClock(Date.now()); setError("El plan caducó. Vuelve a comparar destinos y prepara un plan nuevo."); return;
    }
    pending.current = true; setCreating(true); setError(null);
    try {
      const response = await client.requestRaw<DownloadReceipt>({
        method: "POST", path: "/api/v1/downloads",
        headers: { "Idempotency-Key": `download-confirm-${preview.plan_id}-${preview.plan_hash}` },
        body: { plan_id: preview.plan_id, plan_hash: preview.plan_hash, operator_confirmed: true },
      });
      if (response.status !== 202 || !response.body?.job_id || response.body.status_url !== `/api/v1/jobs/${response.body.job_id}`) {
        throw new ApiError("UNEXPECTED_RESPONSE", "No se pudo comprobar el recibo de descarga", response.status, true, null, "download_receipt_invalid");
      }
      setReceipt(response.body); setUncertain(false);
    } catch (failure) {
      const unknownOutcome = failure instanceof ApiError && (failure.isOffline || failure.status >= 500 || failure.code === "UNEXPECTED_RESPONSE");
      setUncertain(unknownOutcome);
      setError(unknownOutcome
        ? "No se pudo comprobar si LARM creó la descarga. Pulsa «Comprobar solicitud» para recuperar la respuesta sin duplicar el trabajo."
        : friendly(failure));
    } finally { pending.current = false; setCreating(false); }
  };

  return <section className="model-preflight" aria-labelledby="preflight-title" aria-busy={loading || creating}>
    <h3 id="preflight-title">Comparar tamaño y destino</h3>
    <p>Se actualiza el espacio disponible y se comparan las ubicaciones. Este cálculo no reserva espacio ni inicia una descarga.</p>
    <label className="hard-budget-field">Presupuesto máximo para archivos con tamaño desconocido (GiB)
      <input inputMode="decimal" value={hardBudgetGiB} disabled={controlsLocked} onChange={(event) => { setHardBudgetGiB(event.target.value); setPreview(null); }} placeholder="Sin presupuesto" aria-describedby={`hard-budget-help-${detail.variant.id}`} />
    </label>
    <p id={`hard-budget-help-${detail.variant.id}`} className="detail">Sin una cota explícita, un archivo de tamaño desconocido bloquea la ubicación.</p>
    {optionalArtifacts.length > 0 && <fieldset className="model-optionals" disabled={controlsLocked}>
      <legend>Archivos opcionales</legend>
      {optionalArtifacts.map((artifact) => <label key={artifact.id} className="check"><input type="checkbox" checked={selectedOptional.includes(artifact.id)} onChange={(event) => { setSelectedOptional((current) => event.target.checked ? [...current, artifact.id] : current.filter((id) => id !== artifact.id)); setPreview(null); }} />{artifact.filename}</label>)}
    </fieldset>}
    <button type="button" disabled={controlsLocked} onClick={() => void run()}>{loading ? "Calculando…" : "Comparar destinos"}</button>
    {error && <p role="alert" className="state-unknown">{error}</p>}
    {preview && <div className="preflight-results" aria-live="polite">
      {sourceAccess && <SourceAccessNotice detail={detail} access={sourceAccess} />}
      <h4>Tamaño por archivo</h4>
      <ul className="model-file-list">{preview.artifact_estimates.map((artifact) => <li key={artifact.artifact_id}>
        <span>{artifact.relative_path}{artifact.required ? " · necesario" : artifact.selected ? " · opcional incluido" : " · opcional excluido"}</span>
        <DetailSize estimate={artifact.size_estimate} />
      </li>)}</ul>
      <h4>Fuentes de descarga</h4>
      <p>Los archivos se descargan completos desde una fuente. Si una fuente falla por una causa temporal, LARM puede probar la siguiente opción compatible.</p>
      <ol className="model-file-list">
        {preview.source_id && <li><span><strong>Fuente principal</strong> · {preview.source_id}</span><span className="detail">Revisión de la variante seleccionada</span></li>}
        {(preview.source_candidates ?? []).map((candidate, index) => <li key={`${candidate.source_id}:${candidate.revision_key}`}>
          <span><strong>Fuente alternativa {index + 1}</strong> · {candidate.source_id}</span>
          <span className="detail">Revisión {candidate.revision_key} · {candidate.https ? "HTTPS" : "HTTP"} · {candidate.authenticated ? "Autenticación disponible" : "Acceso público"} · {candidate.artifacts.length} archivos compatibles</span>
        </li>)}
      </ol>
      {(preview.source_candidates ?? []).length === 0 && <p className="detail">No hay mirrors exactos para los archivos seleccionados; se conserva la fuente de la variante.</p>}
      <h4>Ubicaciones</h4>
      {preview.destination_options.length === 0 ? <p>No hay ubicaciones de almacenamiento registradas.</p> : <fieldset className="destination-options" disabled={controlsLocked}>
        <legend>Elige una ubicación para revisar su estimación</legend>
        {preview.destination_options.map((option) => <label key={option.storage_location_id} className={`destination-option ${option.eligible ? "" : "destination-blocked"}`}>
          <input type="radio" name={`destination-${detail.variant.id}`} value={option.storage_location_id} checked={destination === option.storage_location_id} onChange={() => setDestination(option.storage_location_id)} />
          <span><strong>{storageLocations.find((location) => location.id === option.storage_location_id)?.name ?? "Ubicación sin nombre"}{storageLocations.find((location) => location.id === option.storage_location_id)?.drive_letter ? ` · ${storageLocations.find((location) => location.id === option.storage_location_id)?.drive_letter}` : ""}</strong>
            {(() => { const location = storageLocations.find((item) => item.id === option.storage_location_id); return location ? <span className="detail">Libre {formatBytes(location.free_bytes)} · reservado {formatBytes(location.reserved_bytes)} · utilizable {location.metadata.volume_budget?.usable_bytes ? formatBytes(location.metadata.volume_budget.usable_bytes) : "desconocido"}</span> : null; })()}
            <span className="detail">{explainDestination(option.eligible ? "fits" : option.reason)}</span></span>
        </label>)}
      </fieldset>}
      {preview.destination_options.length > 0 && <div className="actions">
        <button type="button" disabled={controlsLocked || !destination} onClick={() => void run(destination)}>{loading ? "Preparando plan…" : "Preparar plan para este destino"}</button>
      </div>}
      {planMatchesDestination && !planExpired && !receipt && <p role="status" className="state-verified">Plan preparado para esta ubicación. Caduca {new Date(preview.expires_at).toLocaleTimeString("es-ES")}.</p>}
      {selectedLocationEstimate && <dl className="facts preflight-summary">
        <dt>Espacio que requiere</dt><dd>{selectedLocationEstimate.required_free_bytes ? formatBytes(selectedLocationEstimate.required_free_bytes) : "Desconocido"}</dd>
        <dt>Estimación</dt><dd>{selectedLocationEstimate.confidence === "EXACT" ? "Exacta" : selectedLocationEstimate.confidence === "UNKNOWN" ? "Desconocida" : "Estimado; se reserva la cota superior"}</dd>
        <dt>Motivo</dt><dd>{selectedOption ? explainDestination(selectedOption.reason) : ""}</dd>
      </dl>}
      <section className="download-confirmation" aria-labelledby={`download-confirmation-${detail.variant.id}`}>
        <h4 id={`download-confirmation-${detail.variant.id}`}>Confirmación de descarga</h4>
        {receipt ? <p role="status" className="state-verified">Descarga creada. <a href="#/descargas">Ver descargas</a></p>
          : uncertain ? <p role="status">La respuesta está pendiente de comprobar. Se conservará esta solicitud hasta conocer el resultado.</p>
            : planMatchesDestination && planExpired ? <p role="status">El plan caducó. Compara de nuevo los destinos antes de confirmar.</p>
              : planMatchesDestination ? <p>Al confirmar, LARM vuelve a comprobar la fuente, su autorización y el espacio disponible antes de crear el trabajo.</p>
                : <p>Primero compara destinos y prepara un plan para una ubicación elegible. No se crea ningún trabajo sin una confirmación posterior.</p>}
        <button type="button" onClick={() => void confirm()} disabled={creating || loading || Boolean(receipt) || !planMatchesDestination || (planExpired && !uncertain)}>
          {creating ? "Comprobando y creando…" : uncertain ? "Comprobar solicitud" : "Confirmar y crear descarga"}
        </button>
      </section>
    </div>}
  </section>;
}

export function ModelsPage({ client, focusResourceId }: { client: ApiClient; focusResourceId?: string }) {
  const [items, setItems] = useState<ModelDetail[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [filter, setFilter] = useState<Filter>("ALL");
  const [view, setView] = useState<"local" | "compare">("local");
  const [query, setQuery] = useState("");
  const [importOpen, setImportOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [compareIds, setCompareIds] = useState<string[]>([]);
  const [verifying, setVerifying] = useState<string | null>(null);
  const [verificationMessage, setVerificationMessage] = useState<string | null>(null);
  const [verifyError, setVerifyError] = useState<string | null>(null);
  const alive = useRef(true);
  const { notify } = useToasts();

  const load = useCallback(async () => {
    setLoadError(null);
    try {
      const page = await client.get<Page<ModelVariant>>("/api/v1/variants?limit=50");
      const details = await mapLimited(page.items, 8, async (variant) => client.get<ModelDetail>(`/api/v1/variants/${encodeURIComponent(variant.id)}/detail`));
      const ordered = details.sort((a, b) => a.canonical_model.name.localeCompare(b.canonical_model.name, "es"));
      if (alive.current) {
        setItems(ordered);
        setNextCursor(page.next_cursor);
        const localFirst = ordered.find((detail) => detail.locations.length > 0);
        setSelectedId((current) => current && ordered.some((detail) => detail.variant.id === current) ? current : localFirst?.variant.id ?? ordered[0]?.variant.id ?? null);
      }
    } catch (e) { if (alive.current) setLoadError(friendly(e)); }
  }, [client]);

  const loadMore = async () => {
    if (!nextCursor || loadingMore) return;
    setLoadingMore(true); setLoadError(null);
    try {
      const page = await client.get<Page<ModelVariant>>(`/api/v1/variants?limit=50&cursor=${encodeURIComponent(nextCursor)}`);
      const details = await mapLimited(page.items, 8, async (variant) => client.get<ModelDetail>(`/api/v1/variants/${encodeURIComponent(variant.id)}/detail`));
      if (alive.current) {
        setItems((current) => [...(current ?? []), ...details].sort((a, b) => a.canonical_model.name.localeCompare(b.canonical_model.name, "es")));
        setNextCursor(page.next_cursor);
      }
    } catch (e) { if (alive.current) setLoadError(friendly(e)); }
    finally { if (alive.current) setLoadingMore(false); }
  };

  useEffect(() => { alive.current = true; void load(); return () => { alive.current = false; }; }, [load]);

  const visible = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase("es");
    return (items ?? []).filter((detail) => {
      if (view === "local" && detail.locations.length === 0) return false;
      const status = availabilityOf(detail);
      if (filter === "AVAILABLE" && status !== "AVAILABLE") return false;
      if (filter === "OFFLINE" && status !== "OFFLINE") return false;
      if (filter === "CORRUPT" && status !== "CORRUPT") return false;
      if (filter === "UPDATE" && !detail.variant.update_available) return false;
      if (filter === "EXTERNAL" && detail.variant.ownership !== "EXTERNAL") return false;
      if (!needle) return true;
      return `${detail.canonical_model.name} ${detail.distribution.upstream_namespace} ${detail.distribution.upstream_model_id} ${detail.variant.format ?? ""} ${detail.variant.quantization ?? ""}`.toLocaleLowerCase("es").includes(needle);
    });
  }, [items, filter, query, view]);
  const selected = visible.find((detail) => detail.variant.id === selectedId) ?? visible[0] ?? null;
  const compare = (items ?? []).filter((detail) => compareIds.includes(detail.variant.id));

  const verify = async (detail: ModelDetail) => {
    if (verifying) return;
    setVerifying(detail.variant.id); setVerifyError(null); setVerificationMessage(null);
    try {
      const receipt = await client.request<{ job_id: string; status_url: string; state: string }>({
        method: "POST", path: `/api/v1/models/${encodeURIComponent(detail.variant.id)}/verify`,
        headers: { "Idempotency-Key": `model-verify-${crypto.randomUUID()}` },
        body: { resource_id: detail.variant.id, profile_id: null },
      });
      setVerificationMessage(`Verificación encolada (${receipt.state}). Se comprobarán los archivos locales; referencia ${receipt.job_id}.`);
      notify("info", "La verificación del modelo se ha encolado.");
      await load();
    } catch (e) { setVerifyError(friendly(e)); }
    finally { setVerifying(null); }
  };

  const onMoveCompleted = useCallback(() => { void load(); }, [load]);
  const onImportCompleted = useCallback(() => { void load(); }, [load]);

  useEffect(() => {
    if (!focusResourceId || !items) return;
    if (items.some((detail) => detail.variant.id === focusResourceId)) setSelectedId(focusResourceId);
    const target = document.querySelector<HTMLElement>(`[data-resource-id="${focusResourceId}"]`);
    target?.focus({ preventScroll: true });
    target?.scrollIntoView({ block: "center" });
  }, [focusResourceId, items]);

  const toggleCompare = (id: string) => setCompareIds((current) => current.includes(id) ? current.filter((item) => item !== id) : current.length < 3 ? [...current, id] : current);

  const locationName = (detail: ModelDetail, location: ArtifactLocation) => {
    const artifact = detail.artifacts.find((item) => item.id === location.artifact_id);
    return `${artifact?.filename ?? location.relative_path} · ${location.state}`;
  };

  return <section className="models-page" aria-label="Mis modelos">
    <p className="models-intro">Tareas locales de gestión: inventario físico y verificación, junto con comparación de variantes y preflight de descarga.</p>
    <ModelStorageTools client={client} />
    <div className="models-views" role="group" aria-label="Vista de modelos">
      <button type="button" className={view === "local" ? "filter-active" : "secondary"} aria-pressed={view === "local"} onClick={() => setView("local")}>Biblioteca local</button>
      <button type="button" className={view === "compare" ? "filter-active" : "secondary"} aria-pressed={view === "compare"} onClick={() => setView("compare")}>Comparar y preparar descarga</button>
    </div>
    <div className="actions model-import-action"><button type="button" className="secondary" aria-expanded={importOpen} onClick={() => setImportOpen((open) => !open)}>{importOpen ? "Cerrar importación" : "Importar carpeta"}</button></div>
    {importOpen && <ModelImport client={client} onCompleted={onImportCompleted} />}
    <div className="models-toolbar">
      <label htmlFor="models-search">{view === "local" ? "Buscar en modelos locales" : "Buscar entre todas las variantes"}</label>
      <input id="models-search" type="search" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Nombre, fuente, formato…" />
      <button type="button" className="secondary" disabled={items === null} onClick={() => void load()}>Actualizar modelos</button>
    </div>
    <div className="models-filters" role="group" aria-label="Filtrar modelos">
      {FILTERS.map(([value, label]) => <button type="button" key={value} className={filter === value ? "filter-active" : "secondary"} aria-pressed={filter === value} onClick={() => setFilter(value)}>{label}</button>)}
    </div>
    {loadError && <div className="panel panel-error" role="alert"><p>{loadError}</p><button type="button" onClick={() => void load()}>Reintentar</button></div>}
    {items === null && !loadError && <p role="status" aria-busy="true">Cargando inventario local…</p>}
    {items && (view === "local" ? !items.some((detail) => detail.locations.length > 0) : items.length === 0) && <section className="panel models-empty"><h2>{view === "local" ? "Aún no hay modelos locales" : "No hay variantes en el catálogo local"}</h2><p>{view === "local" ? "Los modelos aparecerán aquí cuando se importen o se descarguen desde una fuente conectada." : "Conecta una fuente para cargar variantes y preparar una comparación."}</p><a href="#/descubrir">Buscar modelos</a></section>}
    {items && items.length > 0 && <div className="models-layout">
      <section className="models-list" aria-label={view === "local" ? "Inventario local" : "Variantes para comparar y descargar"}>
        <p className="detail">{visible.length} variantes{view === "local" ? ` locales de ${items.filter((detail) => detail.locations.length > 0).length}` : " disponibles para comparar"} · compara hasta tres</p>
        {visible.length === 0 && <p>No hay modelos con estos filtros.</p>}
        {visible.map((detail) => {
          const status = availabilityOf(detail);
          return <article key={detail.variant.id} tabIndex={-1} data-resource-id={detail.variant.id} className={`model-row ${selectedId === detail.variant.id ? "model-row-selected" : ""}`}>
            <button type="button" className="model-row-select" aria-current={selectedId === detail.variant.id ? "true" : undefined} onClick={() => setSelectedId(detail.variant.id)}>
              <strong>{detail.canonical_model.name}</strong>
              <span>{detail.variant.format ?? "Formato desconocido"}{detail.variant.quantization ? ` · ${detail.variant.quantization}` : ""}</span>
              <span>{AVAILABILITY[status] ?? `Sin clasificar (${status})`} · {LIFECYCLE[detail.variant.lifecycle] ?? detail.variant.lifecycle}</span>
              <span>{modelLocalSizeLabel(detail)} · {OWNERSHIP[detail.variant.ownership] ?? detail.variant.ownership}</span>
            </button>
            <label className="model-compare-check"><input type="checkbox" checked={compareIds.includes(detail.variant.id)} onChange={() => toggleCompare(detail.variant.id)} disabled={!compareIds.includes(detail.variant.id) && compareIds.length >= 3} />Comparar</label>
          </article>;
        })}
        {nextCursor && <button type="button" className="secondary" disabled={loadingMore} onClick={() => void loadMore()}>{loadingMore ? "Cargando…" : "Cargar más modelos"}</button>}
      </section>
      <div className="models-detail-column">
        {compare.length >= 2 && <section className="panel model-comparison" aria-labelledby="compare-heading">
          <div className="model-section-heading"><h2 id="compare-heading">Comparación de variantes</h2><button type="button" className="link" onClick={() => setCompareIds([])}>Limpiar</button></div>
          <div className="table-scroll"><table className="matrix"><caption>Hasta tres variantes seleccionadas. La descarga incluye los archivos necesarios; los opcionales se eligen al preparar el plan.</caption><thead><tr><th>Dato</th>{compare.map((detail) => <th key={detail.variant.id}>{detail.canonical_model.name}</th>)}</tr></thead><tbody>
            <tr><th>Fuente</th>{compare.map((d) => <td key={d.variant.id}>{d.distribution.upstream_namespace}/{d.distribution.upstream_model_id}</td>)}</tr>
            <tr><th>Formato / cuantización</th>{compare.map((d) => <td key={d.variant.id}>{d.variant.format ?? "Desconocido"} / {d.variant.quantization ?? "Desconocida"}</td>)}</tr>
            <tr><th>Disponibilidad</th>{compare.map((d) => <td key={d.variant.id}>{AVAILABILITY[availabilityOf(d)] ?? availabilityOf(d)}</td>)}</tr>
            <tr><th>Tamaño de descarga</th>{compare.map((d) => <td key={d.variant.id}>{formatVariantSize(d)}</td>)}</tr>
            <tr><th>Tamaño local</th>{compare.map((d) => <td key={d.variant.id}>{modelLocalSizeLabel(d)}</td>)}</tr>
            <tr><th>Ajuste técnico</th>{compare.map((d) => <td key={d.variant.id}>{d.compatibility.find((record) => record.technical_fit !== null)?.technical_fit ?? "Sin cálculo cualificado"}</td>)}</tr>
          </tbody></table></div>
        </section>}
        {selected && <ModelDetailPanel key={selected.variant.id} detail={selected} client={client} verifying={verifying === selected.variant.id} onVerify={() => void verify(selected)} onMoveCompleted={onMoveCompleted} verifyError={verifyError} verificationMessage={verificationMessage} locationName={locationName} />}
      </div>
    </div>}
  </section>;
}

function ModelDetailPanel({ detail, client, verifying, onVerify, onMoveCompleted, verifyError, verificationMessage, locationName }: {
  detail: ModelDetail; client: ApiClient; verifying: boolean; onVerify: () => void;
  onMoveCompleted: () => void; verifyError: string | null; verificationMessage: string | null; locationName: (detail: ModelDetail, location: ArtifactLocation) => string;
}) {
  const [moveOpen, setMoveOpen] = useState(false);
  const [transformOpen, setTransformOpen] = useState(false);
  const status = availabilityOf(detail);
  return <section className="panel model-detail" aria-labelledby="model-detail-heading">
    <div className="model-section-heading"><div><p className="eyebrow">{detail.distribution.upstream_namespace}/{detail.distribution.upstream_model_id}</p><h2 id="model-detail-heading">{detail.canonical_model.name}</h2></div><span className={`model-state model-state-${status.toLowerCase()}`}>{AVAILABILITY[status] ?? status}</span></div>
    <dl className="facts model-facts">
      <dt>Fuente y revisión</dt><dd>{detail.distribution.source_id} · {detail.revision.revision_key}</dd>
      <dt>Variante</dt><dd>{detail.variant.format ?? "Formato desconocido"} · {detail.variant.quantization ?? "Cuantización desconocida"} · {detail.variant.architecture ?? "Arquitectura desconocida"}</dd>
      <dt>Estado local</dt><dd>{LIFECYCLE[detail.variant.lifecycle] ?? detail.variant.lifecycle}; {OWNERSHIP[detail.variant.ownership] ?? detail.variant.ownership}{detail.variant.update_available ? " · hay actualización" : ""}</dd>
      <dt>Licencia</dt><dd>{detail.distribution.license_id ?? "No informada"}</dd>
      <dt>Hash del bundle</dt><dd className="mono">{detail.variant.bundle_hash ?? "No disponible"}</dd>
      <dt>Leases activos</dt><dd>{detail.active_lease_ids.length}</dd>
    </dl>
    <h3>Archivos y ubicaciones</h3>
    {detail.artifacts.length === 0 ? <p>No hay descriptores de archivo.</p> : <div className="table-scroll"><table className="matrix"><thead><tr><th>Archivo</th><th>Requisito</th><th>Tamaño</th><th>Ubicación física</th></tr></thead><tbody>
      {detail.artifacts.map((artifact) => {
        const locations = detail.locations.filter((location) => location.artifact_id === artifact.id);
        return <tr key={artifact.id}><th>{artifact.filename}</th><td>{artifact.required ? "Necesario" : "Opcional"}</td><td><div>{localSizeLabel(artifactLocalSize(artifact, locations))}</div><div><span>Fuente:</span> <DetailSize estimate={artifact.size_estimate} /></div></td><td>{locations.length ? locations.map((location) => locationName(detail, location)).join("; ") : "Sin ubicación registrada"}</td></tr>;
      })}
    </tbody></table></div>}
    <div className="actions"><button type="button" disabled={verifying || detail.variant.ownership === "EXTERNAL" || detail.locations.every((location) => location.state !== "ONLINE")} onClick={onVerify}>{verifying ? "Encolando verificación…" : "Verificar archivos locales"}</button>
      {detail.locations.length > 0 && <button type="button" className="secondary" disabled={detail.variant.ownership !== "LARM_MANAGED"} onClick={() => setMoveOpen(true)}>Mover a otro disco</button>}
      <button type="button" className="secondary" onClick={() => setTransformOpen(true)}>Transformar a otro formato</button>
    </div>
    <p className="detail">Se comprobarán hashes y accesibilidad desde las ubicaciones registradas. No se inicia ningún runtime.</p>
    {verificationMessage && <p role="status" className="state-verified">{verificationMessage}</p>}
    {verifyError && <p role="alert" className="state-unknown">{verifyError}</p>}
    {moveOpen && <MoveModel detail={detail} client={client} onClose={() => setMoveOpen(false)} onCompleted={onMoveCompleted} />}
    {transformOpen && <ModelTransform variantId={detail.variant.id} client={client} onClose={() => setTransformOpen(false)} onCompleted={onMoveCompleted} />}
    {detail.locations.length > 0 && <ModelDelete detail={detail} client={client} onCompleted={onMoveCompleted} />}
    <ModelPromote client={client} variantId={detail.variant.id} promotion={detail.variant.promotion} rowVersion={detail.variant.row_version} onCompleted={onMoveCompleted} />
    <DownloadPreflight client={client} detail={detail} />
  </section>;
}
