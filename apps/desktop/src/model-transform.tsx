import { useEffect, useId, useRef, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import type { StorageLocation } from "./storage";

/** Instalación de adaptador (subconjunto usado por el panel de transformación). */
export interface TransformAdapter {
  id: string;
  adapter_id: string;
  version: string;
  family: string;
  state: string;
}

interface Page<T> { items: T[]; next_cursor: string | null }

interface PlanStep {
  phase: string;
  adapter_id: string | null;
  request?: Record<string, unknown>;
}

interface PlanVolume {
  volume_key: string;
  peak_extra_bytes: string;
  margin_bytes: string;
  usable_free_bytes: string;
  eligible: boolean;
}

interface StorageEstimate {
  confidence: string;
  existing_bytes: string;
  upper_bound_bytes: string;
  peak_extra_bytes: string;
  required_free_bytes: string;
  per_volume: PlanVolume[];
}

/** Plan sellado (subconjunto mostrado al operador antes de confirmar). */
export interface TransformationPlan {
  plan_id: string;
  plan_hash: string;
  steps: PlanStep[];
  storage_estimate: StorageEstimate;
  expires_at: string;
  approval_required: boolean;
}

interface JobReceipt { job_id: string; status_url: string; state: "QUEUED" | "RUNNING" | "COMPLETED" | "FAILED" | "CANCELLED" }
interface JobState extends JobReceipt {
  progress: Record<string, unknown>;
  result: Record<string, unknown> | null;
  error: { message: string; code: string } | null;
}

function explain(error: unknown): string {
  if (error instanceof ApiError && error.isOffline) return "Se perdió la conexión con LARM. Comprueba la conexión e inténtalo de nuevo.";
  if (error instanceof ApiError && error.status === 403) return "Tu cuenta no tiene permiso para planificar o ejecutar transformaciones.";
  if (error instanceof ApiError && error.status === 404) return "El adaptador de transformación o la variante de entrada no están disponibles.";
  if (error instanceof ApiError && error.status === 422) return "El adaptador no soporta alguno de los parámetros pedidos.";
  if (error instanceof ApiError && error.status === 409) return "El plan cambió o caducó, o el destino no tiene espacio suficiente. Vuelve a preparar el plan.";
  return "No se pudo completar la operación. Comprueba la conexión y el estado del adaptador.";
}

/** El resultado público del job lleva la variante derivada (linaje estructural). */
function derivedVariantId(result: Record<string, unknown> | null): string | null {
  if (!result) return null;
  const variant = result.variant;
  if (typeof variant === "object" && variant !== null) {
    const id = (variant as Record<string, unknown>).id;
    if (typeof id === "string") return id;
  }
  return null;
}

/**
 * Panel "Transformar modelo": plan sellado visible antes de confirmar,
 * confirmación explícita y progreso del job. La UI no inventa enums ni
 * resultados: muestra lo que el plan y el job reportan.
 */
export function ModelTransform({ client, variantId, onCompleted, onClose }: {
  client: ApiClient;
  variantId: string;
  onCompleted: () => void;
  onClose: () => void;
}) {
  const id = useId();
  const [adapters, setAdapters] = useState<TransformAdapter[]>([]);
  const [locations, setLocations] = useState<StorageLocation[]>([]);
  const [loading, setLoading] = useState(true);
  const [adapterId, setAdapterId] = useState("");
  const [toolVersion, setToolVersion] = useState("");
  const [targetFormat, setTargetFormat] = useState("");
  const [quantization, setQuantization] = useState("");
  const [targetId, setTargetId] = useState("");
  const [plan, setPlan] = useState<TransformationPlan | null>(null);
  const [planning, setPlanning] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [receipt, setReceipt] = useState<JobReceipt | null>(null);
  const [job, setJob] = useState<JobState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [cancelRequested, setCancelRequested] = useState(false);
  const notified = useRef(false);
  const pollGeneration = useRef(0);

  useEffect(() => {
    let active = true;
    void Promise.all([
      client.get<Page<TransformAdapter>>("/api/v1/adapters?family=transformation&limit=200"),
      client.get<Page<StorageLocation>>("/api/v1/storage-locations?limit=200"),
    ]).then(([adapterPage, locationPage]) => {
      if (!active) return;
      const qualified = adapterPage.items.filter((item) => item.state === "VALIDATED");
      setAdapters(qualified);
      setLocations(locationPage.items.filter((item) => item.state === "ONLINE"));
      setAdapterId(qualified[0]?.adapter_id ?? "");
      setToolVersion(qualified[0]?.version ?? "");
      setTargetId(locationPage.items.find((item) => item.state === "ONLINE")?.id ?? "");
      setLoading(false);
    }).catch((e) => {
      if (!active) return;
      setError(explain(e));
      setLoading(false);
    });
    return () => { active = false; };
  }, [client]);

  useEffect(() => {
    // El tool_version debe seguir a la instalación elegida (manifest).
    const chosen = adapters.find((item) => item.adapter_id === adapterId);
    if (chosen) setToolVersion(chosen.version);
  }, [adapterId, adapters]);

  useEffect(() => {
    if (!receipt || (job && (job.state === "COMPLETED" || job.state === "FAILED" || job.state === "CANCELLED"))) return;
    const generation = ++pollGeneration.current;
    const pollOnce = () => {
      void client.get<JobState>(`/api/v1/jobs/${encodeURIComponent(receipt.job_id)}`).then((value) => {
        if (pollGeneration.current !== generation) return;
        setJob(value);
        if ((value.state === "COMPLETED" || value.state === "FAILED") && !notified.current) {
          notified.current = true;
          clearInterval(timer);
          if (value.state === "COMPLETED") onCompleted();
        }
      }).catch(() => { /* el siguiente intento reintenta; la conexión se reporta en pantalla principal */ });
    };
    pollOnce();
    const timer = setInterval(pollOnce, 1500);
    return () => clearInterval(timer);
  }, [client, receipt, job, onCompleted]);

  const preparePlan = async () => {
    setError(null);
    setPlan(null);
    setConfirmed(false);
    setReceipt(null);
    setJob(null);
    setCancelRequested(false);
    notified.current = false;
    setPlanning(true);
    try {
      const parameters: Record<string, string> = {};
      if (quantization.trim() !== "") parameters.quantization = quantization.trim();
      const plan = await client.request<TransformationPlan>({
        method: "POST",
        path: "/api/v1/transform-plans",
        body: {
          input_variant_id: variantId,
          adapter_id: adapterId,
          tool_version: toolVersion,
          target_format: targetFormat.trim(),
          parameters,
          storage_location_id: targetId,
          hard_budget_bytes: null,
        },
      });
      setPlan(plan);
    } catch (e) {
      setError(explain(e));
    } finally {
      setPlanning(false);
    }
  };

  const confirmTransformation = async () => {
    if (!plan) return;
    setError(null);
    try {
      const receipt = await client.request<JobReceipt>({
        method: "POST",
        path: "/api/v1/transformations",
        body: { plan_id: plan.plan_id, plan_hash: plan.plan_hash, approval_id: null },
      });
      setReceipt(receipt);
    } catch (e) {
      setError(explain(e));
    }
  };

  const cancelTransformation = async () => {
    if (!receipt) return;
    setError(null);
    try {
      await client.request<JobReceipt>({
        method: "POST",
        path: `/api/v1/jobs/${encodeURIComponent(receipt.job_id)}/cancel`,
      });
      setCancelRequested(true);
    } catch (e) {
      setError(explain(e));
    }
  };

  const jobStateLabel = job?.state === "COMPLETED" ? "Completada"
    : job?.state === "FAILED" ? "Fallida"
    : job?.state === "CANCELLED" ? "Cancelada"
    : job?.state === "RUNNING" ? "En ejecución"
    : job ? "En cola" : null;
  const derived = derivedVariantId(job?.result ?? null);
  const workerProgress = job?.progress && typeof job.progress.worker === "object" && job.progress.worker !== null
    ? job.progress.worker as Record<string, unknown>
    : null;

  return <section className="panel model-transform" aria-labelledby={`transform-heading-${id}`}>
    <div className="model-section-heading">
      <h3 id={`transform-heading-${id}`}>Transformar esta variante</h3>
      <button type="button" className="link" onClick={onClose}>Cerrar</button>
    </div>
    {loading && <p>Cargando adaptadores y destinos…</p>}
    {!loading && adapters.length === 0 && <p>No hay adaptadores de transformación cualificados. Instala y cualifica uno primero (pantalla Adaptadores).</p>}
    {!loading && adapters.length > 0 && <>
      <div className="form-grid">
        <label htmlFor={`${id}-adapter`}>Adaptador de transformación</label>
        <select id={`${id}-adapter`} value={adapterId} onChange={(event) => setAdapterId(event.target.value)} disabled={plan !== null || receipt !== null}>
          {adapters.map((item) => <option key={item.id} value={item.adapter_id}>{item.adapter_id} · v{item.version}</option>)}
        </select>
        <label htmlFor={`${id}-format`}>Formato de salida</label>
        <input id={`${id}-format`} value={targetFormat} onChange={(event) => setTargetFormat(event.target.value)} placeholder="GGUF" disabled={plan !== null || receipt !== null} />
        <label htmlFor={`${id}-quant`}>Cuantización (opcional)</label>
        <input id={`${id}-quant`} value={quantization} onChange={(event) => setQuantization(event.target.value)} placeholder="Q8_0" disabled={plan !== null || receipt !== null} />
        <label htmlFor={`${id}-target`}>Destino</label>
        <select id={`${id}-target`} value={targetId} onChange={(event) => setTargetId(event.target.value)} disabled={plan !== null || receipt !== null}>
          {locations.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}
        </select>
      </div>
      <div className="actions">
        <button type="button" onClick={() => void preparePlan()} disabled={planning || receipt !== null || targetFormat.trim() === "" || adapterId === "" || targetId === ""}>
          {planning ? "Preparando plan…" : plan ? "Volver a preparar el plan" : "Preparar plan"}
        </button>
      </div>
      {error && <p role="alert" className="state-unknown">{error}</p>}
      {plan && <dl className="facts transform-plan-facts">
        <dt>Plan sellado</dt><dd className="mono">{plan.plan_id} · {plan.plan_hash.slice(0, 16)}…</dd>
        <dt>Pasos</dt><dd>{plan.steps.map((step) => step.phase).join(" → ")}</dd>
        <dt>Estimación de almacenamiento</dt><dd>Pico {plan.storage_estimate.peak_extra_bytes} bytes · requerido libre {plan.storage_estimate.required_free_bytes} bytes</dd>
        <dt>Por volumen</dt><dd>{plan.storage_estimate.per_volume.map((volume) => `${volume.volume_key}: +${volume.peak_extra_bytes} (margen ${volume.margin_bytes})`).join("; ")}</dd>
        <dt>Expira</dt><dd>{new Date(plan.expires_at).toLocaleString()}</dd>
      </dl>}
      {plan && !receipt && <label className="confirm-check">
        <input type="checkbox" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} />
        Confirmo ejecutar la transformación con este plan sellado.
      </label>}
      {plan && !receipt && <div className="actions">
        <button type="button" onClick={() => void confirmTransformation()} disabled={!confirmed}>Ejecutar transformación</button>
      </div>}
      {receipt && jobStateLabel && <p role="status" className={job?.state === "FAILED" || job?.state === "CANCELLED" ? "state-unknown" : "state-verified"}>
        Transformación {jobStateLabel.toLowerCase()} · job {receipt.job_id}
        {workerProgress && typeof workerProgress.transformation_state === "string" ? ` · ${workerProgress.transformation_state}` : ""}
        {cancelRequested && job?.state !== "CANCELLED" && job?.state !== "COMPLETED" && job?.state !== "FAILED" ? " · cancelación solicitada" : ""}
      </p>}
      {receipt && (job?.state === "QUEUED" || job?.state === "RUNNING") && !cancelRequested && <div className="actions">
        <button type="button" className="secondary" onClick={() => { setCancelRequested(true); void cancelTransformation(); }}>Solicitar cancelación</button>
      </div>}
      {receipt && (job?.state === "QUEUED" || job?.state === "RUNNING") && cancelRequested && <p className="detail">
        La cancelación se solicitó al daemon; el worker coopera al recibir la señal y el job pasa a Cancelada.
      </p>}
      {job?.state === "FAILED" && job.error && <p role="alert" className="state-unknown">{job.error.message} ({job.error.code})</p>}
      {job?.state === "COMPLETED" && <p className="detail">
        La variante derivada ya está en Mis modelos{derived ? ` (${derived})` : ""}; su cuantización registrada aparece junto al formato.
      </p>}
    </>}
  </section>;
}
