import { useCallback, useEffect, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import { allPages } from "./api/runtimes";
import { formatBytes } from "./hardware";
import { DownloadControls } from "./download-controls";

interface DownloadJob {
  id: string; created_at: string; row_version: number; variant_id: string; storage_location_id: string;
  state: string; attempt: number; progress: Record<string, unknown>; error: Record<string, unknown> | null;
  priority: "HIGH" | "NORMAL" | "BACKGROUND";
}
interface TransformationJob {
  id: string; created_at: string; row_version: number; input_variant_id: string; output_variant_id: string | null;
  adapter_id: string; tool_version: string; parameters: Record<string, unknown>; state: string; attempt: number;
  metadata: Record<string, unknown>; error: Record<string, unknown> | null;
}
interface SpeedSample { bytes: bigint; at: number; bytesPerSecond: number | null }

const DOWNLOAD_LABEL: Record<string, string> = {
  PLANNED: "Preparada", QUEUED: "En cola", RUNNING: "En curso", PAUSING: "Pausando", PAUSED: "En pausa",
  VERIFYING: "Verificando archivos", COMPLETED: "Completada", CANCELLING: "Cancelando", CANCELLED: "Cancelada", FAILED: "Fallida",
};
const TRANSFORM_LABEL: Record<string, string> = {
  PLANNED: "Preparada", QUEUED: "En cola", RUNNING: "En curso", PAUSING: "Pausando", PAUSED: "En pausa",
  VERIFYING: "Verificando", COMPLETED: "Completada", CANCELLING: "Cancelando", CANCELLED: "Cancelada",
  FAILED: "Fallida", FAILED_RECOVERABLE: "Interrumpida; se puede recuperar",
};

function object(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function decimal(value: unknown): bigint | null {
  if (typeof value !== "string" || !/^\d+$/.test(value)) return null;
  try { return BigInt(value); } catch { return null; }
}

function bytesLabel(value: bigint | null): string {
  if (value === null || value > BigInt(Number.MAX_SAFE_INTEGER)) return "No informado";
  try { return formatBytes(value.toString()); } catch { return "No informado"; }
}

function shortId(value: string): string { return value.slice(0, 8); }

function explain(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.isOffline) return "LARM no responde. La cola se actualizará cuando vuelva la conexión.";
    if (error.status === 403) return "Tu cuenta no tiene permiso para consultar esta cola.";
    if (error.status === 404) return "La consulta de esta cola aún no está disponible en el daemon.";
  }
  return "No se pudo actualizar la cola. Vuelve a intentarlo.";
}

function durableBytes(progress: DownloadJob["progress"]): bigint | null {
  const artifacts = object(progress.artifacts);
  if (!artifacts || Object.keys(artifacts).length === 0) return null;
  let sum = 0n;
  for (const artifact of Object.values(artifacts)) {
    const offset = decimal(object(artifact)?.durable_offset);
    if (offset === null) return null;
    sum += offset;
  }
  return sum;
}

function progressBytes(job: DownloadJob): { done: bigint | null; total: bigint | null } {
  const completed = job.state === "COMPLETED" ? decimal(job.progress.completed_bytes) : null;
  return {
    done: completed ?? decimal(job.progress.bytes_done) ?? decimal(job.progress.bytes_copied) ?? durableBytes(job.progress),
    total: decimal(job.progress.bytes_total),
  };
}

function etaLabel(done: bigint | null, total: bigint | null, speed: number | null): string | null {
  if (done === null || total === null || total < done || speed === null || speed <= 0 || !Number.isFinite(speed)) return null;
  const seconds = Number(total - done) / speed;
  if (!Number.isFinite(seconds) || seconds < 0) return null;
  if (seconds < 60) return `${Math.ceil(seconds)} s`;
  if (seconds < 3600) return `${Math.ceil(seconds / 60)} min`;
  return `${Math.floor(seconds / 3600)} h ${Math.ceil((seconds % 3600) / 60)} min`;
}

function errorLabel(error: Record<string, unknown> | null): string | null {
  if (!error) return null;
  if (typeof error.message === "string" && error.message.trim()) return error.message;
  if (error.code === "CHECKSUM_MISMATCH" || error.reason === "download_hash_mismatch") return "La verificación SHA-256 no coincide.";
  if (typeof error.code === "string") return `No se pudo completar (${error.code}).`;
  return "No se pudo completar este trabajo.";
}

function transformationPeak(job: TransformationJob): bigint | null {
  const snapshot = object(job.metadata.plan_snapshot);
  const estimate = object(snapshot?.storage_estimate);
  return decimal(estimate?.temporary_peak_bytes) ?? decimal(estimate?.peak_extra_bytes);
}

export function DownloadsPage({ client, pollMs = 5000 }: { client: ApiClient; pollMs?: number }) {
  const [tab, setTab] = useState<"downloads" | "transformations">("downloads");
  const [downloads, setDownloads] = useState<DownloadJob[] | null>(null);
  const [transformations, setTransformations] = useState<TransformationJob[] | null>(null);
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const [transformError, setTransformError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [refreshKey, setRefreshKey] = useState(0);
  const [samples, setSamples] = useState<Record<string, SpeedSample>>({});
  const [priorityDrafts, setPriorityDrafts] = useState<Record<string, DownloadJob["priority"]>>({});
  const [priorityBusy, setPriorityBusy] = useState<Record<string, boolean>>({});
  const [priorityErrors, setPriorityErrors] = useState<Record<string, string>>({});

  const updatePriority = useCallback(async (job: DownloadJob) => {
    const priority = priorityDrafts[job.id] ?? job.priority;
    if (priority === job.priority || priorityBusy[job.id]) return;
    setPriorityBusy((current) => ({ ...current, [job.id]: true }));
    setPriorityErrors((current) => ({ ...current, [job.id]: "" }));
    try {
      const response = await client.requestRaw<DownloadJob>({
        method: "PATCH",
        path: `/api/v1/downloads/${job.id}/priority`,
        headers: { "If-Match": `"${job.row_version}"` },
        body: { priority },
      });
      setDownloads((current) => current?.map((item) => item.id === job.id ? response.body : item) ?? current);
      setPriorityDrafts((current) => ({ ...current, [job.id]: response.body.priority }));
    } catch (error) {
      const message = error instanceof ApiError && error.code === "STALE_REVISION"
        ? "La descarga cambió. Actualiza la cola y revisa su estado actual."
        : error instanceof ApiError && error.code === "INVALID_STATE_TRANSITION"
          ? "La descarga ya no admite cambios de prioridad."
          : explain(error);
      setPriorityErrors((current) => ({ ...current, [job.id]: message }));
    } finally {
      setPriorityBusy((current) => ({ ...current, [job.id]: false }));
    }
  }, [client, priorityBusy, priorityDrafts]);

  const refresh = useCallback(async (current: () => boolean) => {
    setRefreshing(true);
    const now = Date.now();
    const [downloadResult, transformResult] = await Promise.allSettled([
      allPages<DownloadJob>(client, "/api/v1/downloads", current),
      allPages<TransformationJob>(client, "/api/v1/transformations", current),
    ]);
    if (!current()) return;
    if (downloadResult.status === "fulfilled") {
      setDownloads(downloadResult.value);
      setDownloadError(null);
      setSamples((previous) => {
        const next = { ...previous };
        for (const job of downloadResult.value) {
          const bytes = progressBytes(job).done;
          if (bytes === null) continue;
          const old = previous[job.id];
          const elapsed = old ? (now - old.at) / 1000 : 0;
          const speed = old && bytes >= old.bytes && elapsed > 0 ? Number(bytes - old.bytes) / elapsed : null;
          next[job.id] = { bytes, at: now, bytesPerSecond: speed && Number.isFinite(speed) ? speed : null };
        }
        return next;
      });
    } else setDownloadError(explain(downloadResult.reason));
    if (transformResult.status === "fulfilled") {
      setTransformations(transformResult.value);
      setTransformError(null);
    } else setTransformError(explain(transformResult.reason));
    setRefreshing(false);
  }, [client]);

  useEffect(() => {
    let active = true;
    const current = () => active;
    void refresh(current);
    const timer = window.setInterval(() => void refresh(current), pollMs);
    return () => { active = false; window.clearInterval(timer); };
  }, [refresh, pollMs, refreshKey]);

  return (
    <section className="download-manager" aria-label="Cola de descargas y transformaciones">
      <div className="download-toolbar">
        <div role="tablist" aria-label="Tipo de trabajo">
          <button type="button" role="tab" aria-selected={tab === "downloads"} onClick={() => setTab("downloads")}>Descargas</button>
          <button type="button" role="tab" aria-selected={tab === "transformations"} onClick={() => setTab("transformations")}>Transformaciones</button>
        </div>
        <button type="button" className="secondary" onClick={() => setRefreshKey((value) => value + 1)} disabled={refreshing}>
          {refreshing ? "Actualizando…" : "Actualizar"}
        </button>
      </div>

      {tab === "downloads" ? (
        <section className="panel download-queue" aria-labelledby="downloads-heading" aria-busy={refreshing}>
          <h2 id="downloads-heading">Cola de descargas</h2>
          <p className="detail">El progreso, la velocidad y el tiempo restante solo aparecen cuando LARM dispone de datos suficientes.</p>
          {downloadError && <p className="panel-error" role="alert">{downloadError}</p>}
          {downloads === null && !downloadError && <p role="status">Cargando descargas…</p>}
          {downloads?.length === 0 && !downloadError && <p role="status">No hay descargas registradas.</p>}
          {downloads && downloads.length > 0 && <div className="download-job-list">
            {downloads.map((job) => {
              const { done, total } = progressBytes(job);
              const speed = job.state === "RUNNING" ? samples[job.id]?.bytesPerSecond ?? null : null;
              const eta = etaLabel(done, total, speed);
              const percent = done !== null && total !== null && total > 0n && done <= total ? Number((done * 100n) / total) : null;
              return <article className="download-job" key={job.id} aria-label={`Descarga ${shortId(job.variant_id)}`}>
                <div className="download-job-heading">
                  <h3>Modelo {shortId(job.variant_id)}</h3>
                  <span className={`download-state download-state-${job.state.toLowerCase()}`}>{DOWNLOAD_LABEL[job.state] ?? "Estado desconocido"}</span>
                </div>
                <dl className="download-facts">
                  <dt>Destino</dt><dd>{shortId(job.storage_location_id)}</dd>
                  <dt>Prioridad</dt><dd>
                    {job.priority === "HIGH" ? "Alta" : job.priority === "BACKGROUND" ? "Baja" : "Normal"}
                    {(["QUEUED", "PAUSED", "PAUSING", "RUNNING"] as string[]).includes(job.state) && <div className="download-priority-control">
                      <label className="visually-hidden" htmlFor={`download-priority-${job.id}`}>Nueva prioridad para modelo {shortId(job.variant_id)}</label>
                      <select
                        id={`download-priority-${job.id}`}
                        value={priorityDrafts[job.id] ?? job.priority}
                        disabled={priorityBusy[job.id]}
                        onChange={(event) => setPriorityDrafts((current) => ({ ...current, [job.id]: event.target.value as DownloadJob["priority"] }))}
                      >
                        <option value="HIGH">Alta</option>
                        <option value="NORMAL">Normal</option>
                        <option value="BACKGROUND">Baja</option>
                      </select>
                      <button type="button" className="secondary" onClick={() => void updatePriority(job)} disabled={priorityBusy[job.id] || (priorityDrafts[job.id] ?? job.priority) === job.priority}>
                        {priorityBusy[job.id] ? "Guardando…" : "Guardar"}
                      </button>
                    </div>}
                  </dd>
                  <dt>Intento</dt><dd>{job.attempt}</dd>
                  <dt>Transferido</dt><dd>{bytesLabel(done)}{total === null ? " · total desconocido" : ` de ${bytesLabel(total)}`}</dd>
                  {speed !== null && speed > 0 && <><dt>Velocidad</dt><dd>{bytesLabel(BigInt(Math.floor(speed)))}/s</dd></>}
                  {eta && <><dt>Tiempo restante</dt><dd>{eta}</dd></>}
                  <dt>Verificación</dt><dd>{job.state === "VERIFYING" ? "Verificando SHA-256" : job.state === "COMPLETED" ? "Completada" : "Pendiente"}</dd>
                </dl>
                {percent !== null ? <progress max={100} value={percent} aria-label={`Progreso de descarga ${percent}%`}>{percent}%</progress>
                  : <p className="detail">Progreso porcentual no calculable con los datos disponibles.</p>}
                {priorityErrors[job.id] && <p className="panel-error" role="alert">{priorityErrors[job.id]}</p>}
                {errorLabel(job.error) && <p className="panel-error" role="alert">{errorLabel(job.error)}</p>}
                <DownloadControls client={client} id={job.id} state={job.state} onRefresh={() => setRefreshKey((value) => value + 1)} />
              </article>;
            })}
          </div>}
        </section>
      ) : (
        <section className="panel transformation-queue" aria-labelledby="transformations-heading" aria-busy={refreshing}>
          <h2 id="transformations-heading">Cola de transformaciones</h2>
          <p className="detail">Las transformaciones interrumpidas conservan su diagnóstico. Solo se puede reiniciar cuando el daemon ofrece el nuevo preflight requerido.</p>
          {transformError && <p className="panel-error" role="alert">{transformError}</p>}
          {transformations === null && !transformError && <p role="status">Cargando transformaciones…</p>}
          {transformations?.length === 0 && !transformError && <p role="status">No hay transformaciones registradas.</p>}
          {transformations && transformations.length > 0 && <div className="download-job-list">
            {transformations.map((job) => {
              const parameters = JSON.stringify(job.parameters);
              const peak = transformationPeak(job);
              return <article className="download-job" key={job.id} aria-label={`Transformación ${shortId(job.input_variant_id)}`}>
                <div className="download-job-heading">
                  <h3>{job.adapter_id} · {job.tool_version}</h3>
                  <span className={`download-state download-state-${job.state.toLowerCase()}`}>{TRANSFORM_LABEL[job.state] ?? "Estado desconocido"}</span>
                </div>
                <dl className="download-facts">
                  <dt>Entrada</dt><dd>{shortId(job.input_variant_id)}</dd>
                  <dt>Salida</dt><dd>{job.output_variant_id ? shortId(job.output_variant_id) : "Pendiente"}</dd>
                  <dt>Espacio temporal máximo</dt><dd>{bytesLabel(peak)}</dd>
                  <dt>Intento</dt><dd>{job.attempt}</dd>
                </dl>
                <details><summary>Parámetros</summary><pre className="transformation-parameters">{parameters}</pre></details>
                {job.state === "FAILED_RECOVERABLE" && <p className="notice" role="status">Para reiniciar hace falta validar de nuevo los archivos de entrada y calcular otro preflight. El reinicio desde esta pantalla aún no está conectado.</p>}
                {errorLabel(job.error) && <p className="panel-error" role="alert">{errorLabel(job.error)}</p>}
              </article>;
            })}
          </div>}
        </section>
      )}
    </section>
  );
}
