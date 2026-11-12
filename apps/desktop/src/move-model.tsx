import { useEffect, useId, useMemo, useRef, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import { formatBytes } from "./hardware";
import type { StorageLocation } from "./storage";
import type { ModelDetail } from "./models";

interface Page<T> { items: T[]; next_cursor: string | null; snapshot_at: string }
interface VolumeEstimate {
  volume_key: string;
  peak_extra_bytes: string;
  margin_bytes: string;
  other_reservations_bytes: string;
  usable_free_bytes: string;
  eligible: boolean;
  reason: string;
}
interface Plan {
  plan_id: string;
  plan_hash: string;
  operation: string;
  resource_ids: string[];
  storage_estimate: {
    download_bytes: string | null;
    final_artifact_bytes: string | null;
    installed_bytes: string | null;
    derived_cache_bytes: string | null;
    temporary_peak_bytes: string | null;
    existing_bytes: string | null;
    peak_extra_bytes: string | null;
    required_free_bytes: string | null;
    upper_bound_bytes: string | null;
    confidence: string;
    reason: string;
    evidence_refs: string[];
    per_volume: VolumeEstimate[];
  };
  steps: Array<{ phase: string; adapter_id: string | null; request: Record<string, unknown> }>;
  expires_at: string;
  approval_required: boolean;
}
interface JobState {
  job_id: string;
  state: "QUEUED" | "RUNNING" | "COMPLETED" | "FAILED" | "CANCELLED";
  resource_type: string;
  resource_id: string | null;
  progress: Record<string, unknown>;
  result: Record<string, unknown> | null;
  error: { message: string; code: string } | null;
  evidence_refs: string[];
}

const JOB_LABEL: Record<JobState["state"], string> = {
  QUEUED: "En cola", RUNNING: "En curso", COMPLETED: "Movimiento completado", FAILED: "Movimiento fallido", CANCELLED: "Movimiento cancelado",
};
const PHASE_LABEL: Record<string, string> = {
  PLANNED: "Plan preparado", COPYING: "Copiando archivos", COPIED: "Copia completa", VERIFIED: "Verificando hashes",
  SWITCHED: "Actualizando el registro", ACCESS_TESTED: "Comprobando el acceso", SOURCE_DELETED: "Retirando el origen", COMPLETED: "Completado",
  FAILED: "Fallido", CANCELLED: "Cancelado",
};
const STATE_LABEL: Record<StorageLocation["state"], string> = {
  ONLINE: "Disponible", OFFLINE: "Sin conexión", READ_ONLY: "Solo lectura", LOW_SPACE: "Poco espacio", DEGRADED: "Degradada", MISSING: "Falta la carpeta", ERROR: "Error", UNKNOWN: "Desconocida",
};

function friendlyMove(error: unknown): string {
  if (!(error instanceof ApiError)) return "No se pudo completar la operación. Vuelve a intentarlo.";
  if (error.isOffline) return "Se perdió la conexión con LARM. El estado del movimiento se puede consultar al reconectar.";
  if (error.status === 403) return "Tu cuenta no tiene permiso para mover modelos.";
  if (error.reason?.includes("lease") || error.code === "ACTIVE_LEASE") return "Hay una sesión activa usando este modelo. Detén la sesión y vuelve a preparar el movimiento.";
  if (error.reason?.includes("space") || error.code === "STORAGE_INSUFFICIENT") return "El destino no tiene espacio utilizable suficiente. Revisa las reservas y elige otra ubicación.";
  if (error.reason === "plan_unknown" || error.code === "STALE_REVISION") return "El plan caducó o cambió el estado del modelo. Prepara un plan nuevo antes de moverlo.";
  return "LARM rechazó la operación. Actualiza el detalle y vuelve a preparar el movimiento.";
}

async function locations(client: ApiClient): Promise<StorageLocation[]> {
  const all: StorageLocation[] = [];
  let cursor: string | null = null;
  for (let page = 0; page < 20; page += 1) {
    const requestPath: string = cursor !== null ? `/api/v1/storage-locations?limit=200&cursor=${encodeURIComponent(cursor)}` : "/api/v1/storage-locations?limit=200";
    const responsePage: Page<StorageLocation> = await client.get<Page<StorageLocation>>(requestPath);
    all.push(...responsePage.items);
    cursor = responsePage.next_cursor;
    if (!cursor) return all;
  }
  return all;
}

function usableBytes(location: StorageLocation): string | null {
  return location.metadata.volume_budget?.usable_bytes ?? null;
}

export function MoveModel({ detail, client, onClose, onCompleted }: {
  detail: ModelDetail; client: ApiClient; onClose: () => void; onCompleted: () => void;
}) {
  const headingId = useId();
  const [storageLocations, setStorageLocations] = useState<StorageLocation[]>([]);
  const [targetId, setTargetId] = useState("");
  const [loadingLocations, setLoadingLocations] = useState(true);
  const [busy, setBusy] = useState(false);
  const [plan, setPlan] = useState<Plan | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [approvalId, setApprovalId] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [jobId, setJobId] = useState<string | null>(null);
  const [job, setJob] = useState<JobState | null>(null);
  const [jobError, setJobError] = useState<string | null>(null);
  const alive = useRef(true);
  const notifiedComplete = useRef(false);
  const availableSources = useMemo(() => [...new Set(detail.locations.map((location) => location.storage_location_id))], [detail.locations]);
  const sourceLabels = availableSources.map((id) => storageLocations.find((location) => location.id === id)?.name ?? "Ubicación registrada");
  const targets = storageLocations.filter((location) => location.state === "ONLINE" && !availableSources.includes(location.id));
  const selectedTarget = targets.find((location) => location.id === targetId) ?? null;
  const hasLease = detail.active_lease_ids.length > 0;
  const canMove = detail.variant.ownership === "LARM_MANAGED" && detail.locations.length > 0;
  const expired = plan !== null && Date.parse(plan.expires_at) <= Date.now();

  useEffect(() => {
    alive.current = true;
    void locations(client).then((items) => {
      if (!alive.current) return;
      setStorageLocations(items);
      const current = new Set(detail.locations.map((location) => location.storage_location_id));
      const first = items.find((item) => item.state === "ONLINE" && !current.has(item.id));
      setTargetId(first?.id ?? "");
    }).catch((e: unknown) => { if (alive.current) setError(friendlyMove(e)); })
      .finally(() => { if (alive.current) setLoadingLocations(false); });
    return () => { alive.current = false; };
  }, [client, detail.locations]);

  useEffect(() => {
    if (!jobId) return;
    let active = true;
    let timer: number | undefined;
    const poll = async () => {
      try {
        const current = await client.get<JobState>(`/api/v1/jobs/${encodeURIComponent(jobId)}`);
        if (!active) return;
        setJob(current); setJobError(null);
        if (current.state === "COMPLETED") {
          if (!notifiedComplete.current) { notifiedComplete.current = true; onCompleted(); }
          return;
        }
        if (current.state === "FAILED" || current.state === "CANCELLED") return;
      } catch (e) {
        if (!active) return;
        setJobError(friendlyMove(e));
      }
      if (active) timer = window.setTimeout(() => void poll(), 1000);
    };
    void poll();
    return () => { active = false; if (timer !== undefined) window.clearTimeout(timer); };
  }, [client, jobId, onCompleted]);

  const prepare = async () => {
    if (busy || !targetId || hasLease || !canMove) return;
    setBusy(true); setError(null); setPlan(null); setConfirmed(false); setJob(null); setJobId(null);
    try {
      const preview = await client.request<Plan>({
        method: "POST", path: `/api/v1/models/${encodeURIComponent(detail.variant.id)}/move`,
        headers: { "Idempotency-Key": `model-move-plan-${crypto.randomUUID()}` },
        body: { target_storage_location_id: targetId, plan_hash: null, dry_run: true, approval_id: null },
      });
      if (preview.operation !== "model.move" || preview.plan_hash.length !== 64) throw new Error("move_plan_response_invalid");
      setPlan(preview);
    } catch (e) { setError(e instanceof Error && e.message === "move_plan_response_invalid" ? "La respuesta del daemon no cumple el contrato del plan." : friendlyMove(e)); }
    finally { setBusy(false); }
  };

  const execute = async () => {
    if (busy || !plan || !confirmed || expired || hasLease || !targetId) return;
    const approval = approvalId.trim();
    if (plan.approval_required && !approval) { setError("Este plan requiere una aprobación asociada a su hash."); return; }
    setBusy(true); setError(null); setJobError(null);
    try {
      const started = await client.request<Plan>({
        method: "POST", path: `/api/v1/models/${encodeURIComponent(detail.variant.id)}/move`,
        headers: { "Idempotency-Key": `model-move-execute-${plan.plan_hash}` },
        body: { target_storage_location_id: targetId, plan_hash: plan.plan_hash, dry_run: false, approval_id: approval || null },
      });
      setJobId(started.plan_id);
      setJob({ job_id: started.plan_id, state: "QUEUED", resource_type: "model_variant", resource_id: detail.variant.id, progress: { phase: "PLANNED", items_total: 0, bytes_total: "0", bytes_copied: "0" }, result: null, error: null, evidence_refs: [] });
    } catch (e) { setError(friendlyMove(e)); }
    finally { setBusy(false); }
  };

  const progress = job?.progress ?? {};
  const copied = typeof progress.bytes_copied === "string" && /^\d+$/.test(progress.bytes_copied) ? progress.bytes_copied : null;
  const total = typeof progress.bytes_total === "string" && /^\d+$/.test(progress.bytes_total) ? progress.bytes_total : null;
  const phase = typeof progress.phase === "string" ? progress.phase : "";
  const percent = copied !== null && total !== null && BigInt(total) > 0n ? Math.min(100, Number((BigInt(copied) * 10000n) / BigInt(total)) / 100) : null;
  const targetFree = selectedTarget ? usableBytes(selectedTarget) : null;

  return <section className="panel move-wizard" aria-labelledby={headingId}>
    <div className="model-section-heading"><h3 id={headingId}>Mover modelo a otra ubicación</h3><button type="button" className="link" onClick={onClose}>Cerrar</button></div>
    <p>El plan en seco no mueve archivos. LARM vuelve a comprobar leases, espacio y hashes al ejecutar el movimiento.</p>
    {!canMove && <p role="status" className="state-unknown">Solo se pueden mover modelos gestionados por LARM con archivos registrados. Los archivos externos, del host o compartidos permanecen en su ubicación.</p>}
    {hasLease && <p role="alert" className="state-unknown">Hay {detail.active_lease_ids.length} sesión(es) activas. Deténlas antes de preparar el movimiento.</p>}
    <dl className="facts move-facts"><dt>Ubicaciones de origen</dt><dd>{sourceLabels.join(", ") || "Sin ubicación registrada"}</dd><dt>Archivos registrados</dt><dd>{detail.locations.length}</dd></dl>
    <label className="move-target-field" htmlFor={`${headingId}-target`}>Destino</label>
    {loadingLocations ? <p role="status" aria-busy="true">Cargando ubicaciones…</p> : <>
      <select id={`${headingId}-target`} value={targetId} disabled={busy || !canMove || hasLease || Boolean(jobId)} onChange={(event) => { setTargetId(event.target.value); setPlan(null); setConfirmed(false); }}>
        <option value="">Elige una ubicación disponible</option>
        {targets.map((location) => <option value={location.id} key={location.id}>{location.name}{location.drive_letter ? ` · ${location.drive_letter}` : ""} · {STATE_LABEL[location.state]}</option>)}
      </select>
      {selectedTarget && <p className="detail move-target-space">Libre {formatBytes(selectedTarget.free_bytes)} · reservado {formatBytes(selectedTarget.reserved_bytes)} · utilizable {targetFree ? formatBytes(targetFree) : "desconocido"}</p>}
      {!targets.length && <p className="state-unknown">No hay otra ubicación online registrada para este modelo.</p>}
    </>}
    {!jobId && <button type="button" disabled={busy || loadingLocations || !targetId || hasLease || !canMove} onClick={() => void prepare()}>{busy ? "Preparando plan…" : "Preparar movimiento"}</button>}
    {error && <p role="alert" className="state-unknown">{error}</p>}
    {plan && !jobId && <section className="move-plan" aria-labelledby={`${headingId}-plan`}>
      <h4 id={`${headingId}-plan`}>Plan de movimiento</h4>
      <dl className="facts"><dt>Espacio adicional requerido</dt><dd>{plan.storage_estimate.required_free_bytes ? formatBytes(plan.storage_estimate.required_free_bytes) : "Desconocido"}</dd><dt>Confianza de la estimación</dt><dd>{plan.storage_estimate.confidence === "EXACT" ? "Exacta" : plan.storage_estimate.confidence === "UNKNOWN" ? "Desconocida" : "Estimación; se reserva la cota superior"}</dd><dt>Caduca</dt><dd>{new Date(plan.expires_at).toLocaleString("es-ES")}</dd><dt>Aprobación</dt><dd>{plan.approval_required ? "Requerida" : "No requerida"}</dd></dl>
      {plan.storage_estimate.per_volume.map((volume) => <p className={volume.eligible ? "state-verified" : "state-unknown"} key={volume.volume_key}>{volume.eligible ? "Espacio suficiente" : "Espacio insuficiente"} · requiere {formatBytes(volume.peak_extra_bytes)} · utilizable {formatBytes(volume.usable_free_bytes)}</p>)}
      <h5>Etapas previstas</h5><ol>{plan.steps.map((step, index) => <li key={`${step.phase}-${index}`}>{PHASE_LABEL[step.phase] ?? `Etapa ${step.phase}`}</li>)}</ol>
      {expired && <p role="alert" className="state-unknown">El plan caducó. Prepara otro antes de mover el modelo.</p>}
      {plan.approval_required && <label className="form move-approval">Identificador de aprobación ligada a este plan
        <input value={approvalId} onChange={(event) => setApprovalId(event.target.value)} disabled={busy} aria-describedby={`${headingId}-approval-help`} />
        <span id={`${headingId}-approval-help`} className="detail">La aprobación no se crea automáticamente; debe estar vinculada al hash mostrado por LARM.</span>
      </label>}
      <fieldset className="move-confirmation" disabled={busy || expired || hasLease || plan.approval_required && !approvalId.trim()}>
        <legend>Confirmación</legend>
        <label className="check"><input type="checkbox" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} />Confirmo mover estos archivos de {sourceLabels.join(", ") || "su ubicación actual"} a {selectedTarget?.name ?? "el destino elegido"}.</label>
        <button type="button" disabled={!confirmed || !canMove} onClick={() => void execute()}>{busy ? "Iniciando…" : "Confirmar y mover"}</button>
      </fieldset>
    </section>}
    {jobId && <section className="move-progress" aria-live="polite" aria-labelledby={`${headingId}-progress`}>
      <h4 id={`${headingId}-progress`}>{job ? JOB_LABEL[job.state] : "Movimiento iniciado"}</h4>
      <p>{PHASE_LABEL[phase] ?? (job?.state === "QUEUED" ? "Esperando turno" : "Comprobando estado…")}</p>
      {percent !== null ? <><progress value={percent} max={100} aria-label="Progreso de copia" /><p>{percent.toLocaleString("es-ES", { maximumFractionDigits: 1 })}% · {formatBytes(copied!)} de {formatBytes(total!)}</p></> : job?.state === "RUNNING" ? <progress aria-label="Movimiento en curso" /> : null}
      {job?.state === "COMPLETED" && <p role="status" className="state-verified">El movimiento terminó correctamente. El inventario local se ha actualizado.</p>}
      {job?.state === "FAILED" && <p role="alert" className="state-unknown">{job.error?.message ?? "El movimiento falló. El origen se conserva hasta que LARM reconcilie su estado."}</p>}
      {job?.state === "CANCELLED" && <p role="status">El movimiento se canceló. Revisa el estado de los archivos antes de volver a intentarlo.</p>}
      {jobError && <p role="alert" className="state-unknown">{jobError} Se volverá a consultar el estado del movimiento.</p>}
    </section>}
  </section>;
}
