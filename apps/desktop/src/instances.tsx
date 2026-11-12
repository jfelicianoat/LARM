import { useCallback, useEffect, useRef, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import { allPages } from "./api/runtimes";
import { readInstanceStopJob, requestInstanceStop, type InstanceJobState, type InstanceState, type InstanceStopReceipt, type InstanceBackend, type InstanceVariantDetail, type RuntimeInstance } from "./api/instances";
import { formatBytes } from "./hardware";

const STATE_LABEL: Record<InstanceState, string> = {
  CREATED: "Creada", STARTING: "Iniciando", LOADING: "Cargando modelo", READY: "Lista",
  BUSY: "En uso", DEGRADED: "Degradada", FAILED: "Fallida", STOPPING: "Deteniéndose",
  STOPPED: "Detenida", CRASHED: "Se cerró inesperadamente",
};
const OWNERSHIP_LABEL = { LARM_MANAGED: "Gestionada por LARM", EXTERNAL: "Externa" } as const;
const JOB_LABEL: Record<InstanceStopReceipt["state"], string> = {
  QUEUED: "Parada en cola", RUNNING: "Deteniendo instancia", COMPLETED: "Instancia detenida",
  FAILED: "No se pudo detener", CANCELLED: "Parada cancelada",
};

function explain(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.isOffline) return "Se perdió la conexión con LARM. Comprueba el estado y vuelve a intentarlo cuando se restablezca.";
    if (error.status === 403) return "Tu cuenta no tiene permiso para consultar o detener instancias.";
    if (error.status === 409) return "La instancia cambió desde la última consulta. Actualiza su estado antes de continuar.";
    if (error.status === 404) return "La instancia o el trabajo ya no está disponible.";
  }
  return "No se pudo completar la operación. Actualiza el estado e inténtalo de nuevo.";
}

function object(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function launchSummary(instance: RuntimeInstance) {
  const snapshot = object(instance.metadata.larm_runtime_launch_snapshot_v1);
  const plan = object(snapshot?.launch_plan);
  const estimate = object(plan?.memory_estimate);
  const memory = typeof estimate?.total_bytes === "string" ? estimate.total_bytes : null;
  const limit = typeof plan?.effective_memory_limit_bytes === "string" ? plan.effective_memory_limit_bytes : null;
  return { memory, limit };
}

function safeBytes(value: string | null): string {
  if (!value || !/^\d+$/.test(value)) return "No informado";
  try { return formatBytes(value); } catch { return "No informado"; }
}

function localTime(value: string | null): string {
  if (!value) return "No informado";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "No informado" : date.toLocaleString("es-ES");
}

function uptime(value: string | null, now = Date.now()): string {
  if (!value) return "No informado";
  const start = Date.parse(value);
  if (!Number.isFinite(start) || start > now) return "No informado";
  const seconds = Math.floor((now - start) / 1000);
  const days = Math.floor(seconds / 86400), hours = Math.floor((seconds % 86400) / 3600), minutes = Math.floor((seconds % 3600) / 60);
  if (days) return `${days} d ${hours} h`;
  if (hours) return `${hours} h ${minutes} min`;
  return `${minutes} min`;
}

function stateClass(state: InstanceState): string {
  if (state === "READY" || state === "BUSY") return "instance-state-good";
  if (state === "FAILED" || state === "CRASHED") return "instance-state-bad";
  if (state === "DEGRADED" || state === "STARTING" || state === "LOADING" || state === "STOPPING") return "instance-state-warn";
  return "instance-state-muted";
}

function canRequestStop(instance: RuntimeInstance): boolean {
  return instance.ownership === "LARM_MANAGED"
    && ["READY", "BUSY", "DEGRADED", "FAILED", "CRASHED"].includes(instance.state);
}

function InstanceDetails({ client, instanceId, refresh, onUpdated, onRefresh }: {
  client: ApiClient; instanceId: string; refresh: number; onUpdated: (value: RuntimeInstance) => void; onRefresh: () => void;
}) {
  const [instance, setInstance] = useState<RuntimeInstance | null>(null);
  const [model, setModel] = useState<InstanceVariantDetail | null>(null), [backend, setBackend] = useState<InstanceBackend | null>(null);
  const [receipt, setReceipt] = useState<InstanceStopReceipt | null>(null), [job, setJob] = useState<InstanceJobState | null>(null);
  const [loading, setLoading] = useState(true), [busy, setBusy] = useState(false), [refreshingJob, setRefreshingJob] = useState(false);
  const [confirmingStop, setConfirmingStop] = useState(false), [error, setError] = useState<string | null>(null), [jobError, setJobError] = useState<string | null>(null);
  const heading = useRef<HTMLHeadingElement>(null), stopKey = useRef<string | null>(null), pending = useRef(false);
  useEffect(() => { heading.current?.focus(); }, [instanceId]);
  useEffect(() => {
    let active = true;
    setLoading(true); setError(null); setInstance(null); setModel(null); setBackend(null);
    void client.get<RuntimeInstance>(`/api/v1/instances/${encodeURIComponent(instanceId)}`)
      .then(async (value) => {
        if (!active) return;
        setInstance(value); onUpdated(value);
        const [modelResult, backendResult] = await Promise.allSettled([
          client.get<InstanceVariantDetail>(`/api/v1/variants/${encodeURIComponent(value.variant_id)}/detail`),
          client.get<InstanceBackend>(`/api/v1/runtime-backends/${encodeURIComponent(value.backend_id)}`),
        ]);
        if (!active) return;
        if (modelResult.status === "fulfilled") setModel(modelResult.value);
        if (backendResult.status === "fulfilled") setBackend(backendResult.value);
      })
      .catch((cause: unknown) => { if (active) setError(explain(cause)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [client, instanceId, refresh, onUpdated]);

  const refreshJob = async () => {
    if (!receipt || pending.current) return;
    pending.current = true; setRefreshingJob(true); setJobError(null);
    try {
      const value = await readInstanceStopJob(client, receipt.job_id);
      if (value.job_id !== receipt.job_id || value.resource_id !== instanceId) throw new Error("instance_job_binding_invalid");
      setJob(value);
      if (value.state === "COMPLETED") onRefresh();
    } catch (cause) {
      setJobError(cause instanceof Error && cause.message === "instance_job_binding_invalid" ? "El job consultado no corresponde a esta instancia." : explain(cause));
    } finally { pending.current = false; setRefreshingJob(false); }
  };

  const stop = async () => {
    if (!instance || !canRequestStop(instance) || pending.current) return;
    pending.current = true; setBusy(true); setError(null);
    stopKey.current ??= crypto.randomUUID();
    try {
      const value = await requestInstanceStop(client, instance.id, stopKey.current);
      stopKey.current = null; setReceipt(value); setJob(null); setConfirmingStop(false); onRefresh();
    } catch (cause) { setError(explain(cause)); }
    finally { pending.current = false; setBusy(false); }
  };

  return <section className="panel instance-detail" aria-label="Detalle de instancia">
    {loading && <p role="status">Cargando detalle de la instancia…</p>}
    {error && <div><p role="alert" className="state-unknown">{error}</p><button type="button" className="secondary" onClick={onRefresh}>Reintentar consulta</button></div>}
    {!loading && !error && instance && <>
      <div className="runtime-heading"><h2 ref={heading} tabIndex={-1}>Instancia</h2><span className={`instance-state ${stateClass(instance.state)}`} role="status">{STATE_LABEL[instance.state] ?? instance.state}</span></div>
      <dl className="facts instance-facts">
        <dt>Modelo</dt><dd>{model?.canonical_model.name ?? "Nombre no disponible"}<span className="detail"> · variante <span className="mono">{instance.variant_id}</span></span>
          {model && <span className="detail">{[model.variant.format, model.variant.quantization, model.variant.architecture].filter(Boolean).join(" · ") || "Sin formato o arquitectura informados"}</span>}</dd>
        <dt>Runtime instalado</dt><dd className="mono">{instance.runtime_installation_id}</dd>
        <dt>Backend</dt><dd>{backend ? `${backend.backend_type} · ${backend.status === "AVAILABLE" ? "disponible" : backend.status === "UNAVAILABLE" ? "no disponible" : "disponibilidad desconocida"}` : "Backend sin detalle disponible"}<span className="detail mono"> · {instance.backend_id}</span>{backend?.driver_version && <span className="detail">Driver {backend.driver_version}</span>}</dd>
        <dt>Perfil de inicio</dt><dd className="mono">{instance.launch_profile_id}</dd>
        <dt>PID</dt><dd>{instance.pid ?? "No informado"}</dd>
        <dt>Iniciada</dt><dd>{localTime(instance.process_started_at)}</dd>
        <dt>Tiempo activa</dt><dd>{instance.state === "STOPPED" ? "Detenida" : uptime(instance.process_started_at)}</dd>
        <dt>Propiedad</dt><dd>{OWNERSHIP_LABEL[instance.ownership] ?? instance.ownership}</dd>
        <dt>Memoria estimada</dt><dd>{safeBytes(launchSummary(instance).memory)}</dd>
        <dt>Límite configurado</dt><dd>{safeBytes(launchSummary(instance).limit)}</dd>
        <dt>Huella de ejecución</dt><dd className="mono">{instance.execution_fingerprint ?? "No informada"}</dd>
        <dt>Hash del ejecutable</dt><dd className="mono">{instance.executable_hash ?? "No informado"}</dd>
        <dt>Último uso</dt><dd>{localTime(instance.last_used_at)}</dd>
      </dl>
      <p className="note">El contrato de instancia todavía no publica un endpoint, memoria medida ni ubicación de logs. La memoria indicada es una estimación del plan de inicio, no el consumo observado.</p>
      {receipt && <section className="instance-stop-job" aria-live="polite">
        <h3>{JOB_LABEL[job?.state ?? receipt.state]}</h3>
        <p>Trabajo <span className="mono">{receipt.job_id}</span>{job?.error ? ` · ${job.error.message}` : ""}</p>
        {jobError && <p role="alert" className="state-unknown">{jobError}</p>}
        <div className="actions"><button type="button" className="secondary" disabled={refreshingJob} onClick={() => void refreshJob()}>{refreshingJob ? "Consultando…" : "Consultar estado de parada"}</button><button type="button" className="secondary" onClick={onRefresh}>Actualizar instancia</button></div>
      </section>}
      {instance.ownership === "EXTERNAL" && <p className="detail">Esta instancia pertenece a un proceso externo; LARM no puede detenerla.</p>}
      {canRequestStop(instance) && !receipt && !confirmingStop && <div className="actions"><button type="button" className="secondary" disabled={busy} onClick={() => setConfirmingStop(true)}>Detener instancia</button></div>}
      {confirmingStop && <section className="instance-stop-confirmation" aria-label="Confirmar parada">
        <h3>¿Detener esta instancia?</h3><p>LARM pedirá al supervisor que cierre el proceso y espere a que termine.</p>
        {error && <p role="alert" className="state-unknown">{error}</p>}
        <div className="actions"><button type="button" disabled={busy} onClick={() => void stop()}>{busy ? "Solicitando…" : "Sí, detener instancia"}</button><button type="button" className="secondary" disabled={busy} onClick={() => setConfirmingStop(false)}>Cancelar</button></div>
      </section>}
    </>}
  </section>;
}

export function InstancesPage({ client }: { client: ApiClient }) {
  const [items, setItems] = useState<RuntimeInstance[]>([]), [selected, setSelected] = useState<string | null>(null);
  const [loading, setLoading] = useState(true), [error, setError] = useState<string | null>(null), [refresh, setRefresh] = useState(0);
  const generation = useRef(0);
  const updateInstance = useCallback((value: RuntimeInstance) => {
    setItems((current) => current.map((item) => item.id === value.id ? value : item));
  }, []);
  useEffect(() => {
    const active = ++generation.current; setLoading(true); setError(null);
    void allPages<RuntimeInstance>(client, "/api/v1/instances", () => active === generation.current)
      .then((values) => { if (active === generation.current) { setItems(values); setSelected((id) => id && values.some((item) => item.id === id) ? id : values[0]?.id ?? null); } })
      .catch((cause: unknown) => { if (active === generation.current) setError(explain(cause)); })
      .finally(() => { if (active === generation.current) setLoading(false); });
    return () => { generation.current++; };
  }, [client, refresh]);

  return <div className="stack instances-page">
    <p>Consulta el estado de los procesos gestionados por LARM y revisa el modelo, backend e identidad de ejecución.</p>
    <div className="actions"><button type="button" className="secondary" disabled={loading} onClick={() => setRefresh((value) => value + 1)}>{loading ? "Actualizando…" : "Actualizar instancias"}</button></div>
    {loading && <p role="status">Cargando instancias…</p>}
    {error && <p role="alert" className="state-unknown">{error} <button type="button" className="link" onClick={() => setRefresh((value) => value + 1)}>Reintentar</button></p>}
    {!loading && !error && !items.length && <section className="panel instances-empty"><h2>No hay instancias registradas</h2><p>Al iniciar un modelo desde una lease, LARM mostrará aquí su proceso y estado.</p></section>}
    {!!items.length && <div className="instances-layout">
      <section className="panel instances-list-panel" aria-labelledby="instances-list-title">
        <h2 id="instances-list-title">Instancias registradas</h2>
        <ul className="instance-list" aria-label="Instancias registradas">{items.map((instance) => <li key={instance.id} className={selected === instance.id ? "instance-row instance-row-selected" : "instance-row"}>
          <div><strong className={`instance-state ${stateClass(instance.state)}`}>{STATE_LABEL[instance.state] ?? instance.state}</strong><p className="detail">PID {instance.pid ?? "no informado"} · {OWNERSHIP_LABEL[instance.ownership] ?? instance.ownership}</p><span className="mono detail">{instance.id}</span></div>
          <button type="button" className="secondary" aria-pressed={selected === instance.id} onClick={() => setSelected(instance.id)}>{selected === instance.id ? "Instancia seleccionada" : "Ver instancia"}</button>
        </li>)}</ul>
      </section>
      {selected && <InstanceDetails key={selected} client={client} instanceId={selected} refresh={refresh} onUpdated={updateInstance} onRefresh={() => setRefresh((value) => value + 1)} />}
    </div>}
  </div>;
}
