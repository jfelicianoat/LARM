import { useEffect, useId, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import { formatBytes } from "./hardware";
import type { StorageLocation } from "./storage";

interface Page<T> { items: T[]; next_cursor: string | null; snapshot_at?: string }
interface CategoryUsage {
  kind: "MODELS" | "RUNTIMES" | "CACHES" | "PARTIALS" | "TEMPORARIES" | "ORPHANS";
  used_bytes: string | null;
  reclaimable_bytes: string | null;
  resource_ids: string[];
  blocker_codes: string[];
}
interface StorageAnalysis {
  storage_location_id: string;
  volume_key: string;
  free_bytes: string | null;
  reserved_bytes: string;
  categories: CategoryUsage[];
  snapshot_at: string;
}
interface PlanStep {
  phase: string;
  adapter_id: string | null;
  request: Record<string, unknown>;
}
interface CleanupPlan {
  plan_id: string;
  plan_hash: string;
  operation: string;
  resource_ids: string[];
  storage_estimate: { reason: string };
  steps: PlanStep[];
  expires_at: string;
  approval_required: boolean;
}

const CATEGORY_LABEL: Record<CategoryUsage["kind"], string> = {
  MODELS: "Modelos", RUNTIMES: "Runtimes", CACHES: "Cachés", PARTIALS: "Descargas parciales",
  TEMPORARIES: "Temporales", ORPHANS: "Ficheros sin registrar",
};

function explain(error: unknown): string {
  if (error instanceof ApiError && error.isOffline) return "No hay conexión con LARM. Actualiza el análisis cuando vuelva a estar disponible.";
  if (error instanceof ApiError && error.status === 403) return "Tu cuenta no tiene permiso para analizar esta ubicación.";
  return "No se pudo analizar esta ubicación. Comprueba que el disco esté conectado y vuelve a intentarlo.";
}

export function ModelStorageTools({ client }: { client: ApiClient }) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const [locations, setLocations] = useState<StorageLocation[]>([]);
  const [selectedId, setSelectedId] = useState("");
  const [loadingLocations, setLoadingLocations] = useState(false);
  const [analysis, setAnalysis] = useState<StorageAnalysis | null>(null);
  const [loadingAnalysis, setLoadingAnalysis] = useState(false);
  const [plan, setPlan] = useState<CleanupPlan | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open || locations.length) return;
    let active = true;
    setLoadingLocations(true); setError(null);
    void client.get<Page<StorageLocation>>("/api/v1/storage-locations?limit=200").then((page) => {
      if (!active) return;
      setLocations(page.items);
      setSelectedId(page.items.find((item) => item.state === "ONLINE")?.id ?? page.items[0]?.id ?? "");
    }).catch((e: unknown) => { if (active) setError(explain(e)); })
      .finally(() => { if (active) setLoadingLocations(false); });
    return () => { active = false; };
  }, [client, locations.length, open]);

  useEffect(() => {
    if (!open || !selectedId) { setAnalysis(null); setPlan(null); return; }
    let active = true;
    setLoadingAnalysis(true); setError(null); setPlan(null);
    void client.get<StorageAnalysis>(`/api/v1/storage-locations/${encodeURIComponent(selectedId)}/analysis`).then((value) => {
      if (active) setAnalysis(value);
    }).catch((e: unknown) => { if (active) { setAnalysis(null); setError(explain(e)); } })
      .finally(() => { if (active) setLoadingAnalysis(false); });
    return () => { active = false; };
  }, [client, open, selectedId]);

  const previewCleanup = async () => {
    if (!selectedId) return;
    setError(null); setPlan(null);
    try {
      const value = await client.request<CleanupPlan>({
        method: "POST", path: `/api/v1/storage-locations/${encodeURIComponent(selectedId)}/cleanup-plan`,
        headers: { "Idempotency-Key": `model-storage-plan-${crypto.randomUUID()}` },
        body: { resource_id: selectedId, profile_id: null },
      });
      if (value.operation !== "storage.cleanup" || !value.approval_required) throw new Error("cleanup_plan_response_invalid");
      setPlan(value);
    } catch (e) {
      setError(e instanceof Error && e.message === "cleanup_plan_response_invalid" ? "LARM devolvió un plan de limpieza no válido." : explain(e));
    }
  };

  const selected = locations.find((location) => location.id === selectedId);
  const reclaimable = analysis?.categories.some((category) => category.reclaimable_bytes !== null && BigInt(category.reclaimable_bytes) > 0n) ?? false;

  return <section className="panel model-storage-tools" aria-labelledby={`${id}-heading`}>
    <div className="model-section-heading">
      <div><p className="eyebrow">Herramientas locales</p><h2 id={`${id}-heading`}>Analizar almacenamiento</h2></div>
      <button type="button" className="secondary" aria-expanded={open} onClick={() => setOpen((value) => !value)}>{open ? "Cerrar análisis" : "Analizar discos"}</button>
    </div>
    {!open && <p className="detail">Consulta el espacio y los ficheros recuperables de cada ubicación; el análisis no borra archivos.</p>}
    {open && <div className="storage-analysis-body">
      {loadingLocations && <p role="status" aria-busy="true">Cargando ubicaciones…</p>}
      {!loadingLocations && locations.length === 0 && !error && <p>No hay ubicaciones registradas para analizar.</p>}
      {locations.length > 0 && <>
        <label htmlFor={`${id}-location`}>Ubicación</label>
        <select id={`${id}-location`} value={selectedId} disabled={loadingLocations || loadingAnalysis} onChange={(event) => setSelectedId(event.target.value)}>
          {locations.map((location) => <option key={location.id} value={location.id}>{location.name}{location.drive_letter ? ` · ${location.drive_letter}` : ""} · {location.state}</option>)}
        </select>
        {selected && <p className="detail">Libre {formatBytes(selected.free_bytes)} · reservado {formatBytes(selected.reserved_bytes)} · utilizable {selected.metadata.volume_budget?.usable_bytes ? formatBytes(selected.metadata.volume_budget.usable_bytes) : "desconocido"}</p>}
      </>}
      {loadingAnalysis && <p role="status" aria-busy="true">Analizando el contenido registrado…</p>}
      {error && <p role="alert" className="state-unknown">{error}</p>}
      {analysis && <>
        <p className="detail">Volumen {analysis.volume_key} · análisis del {new Date(analysis.snapshot_at).toLocaleString("es-ES")}</p>
        <div className="table-scroll"><table className="matrix storage-analysis-table"><caption>Uso observado por categoría</caption><thead><tr><th>Categoría</th><th>Ocupado</th><th>Recuperable</th><th>Bloqueos</th></tr></thead><tbody>
          {analysis.categories.map((category) => <tr key={category.kind}><th>{CATEGORY_LABEL[category.kind]}</th><td>{category.used_bytes === null ? "Desconocido" : formatBytes(category.used_bytes)}</td><td>{category.reclaimable_bytes === null ? "Desconocido" : formatBytes(category.reclaimable_bytes)}</td><td>{category.blocker_codes.length ? category.blocker_codes.join(", ") : "—"}</td></tr>)}
        </tbody></table></div>
        {reclaimable && <button type="button" className="secondary" onClick={() => void previewCleanup()}>Preparar vista previa de limpieza</button>}
      </>}
      {plan && <section className="cleanup-preview" aria-labelledby={`${id}-cleanup-heading`}>
        <h3 id={`${id}-cleanup-heading`}>Archivos que el plan propone retirar</h3>
        <p>{plan.steps.length} elementos · {plan.storage_estimate.reason}. El plan requiere aprobación asociada al hash y aquí solo se muestra; no se borra ningún archivo.</p>
        <ul>{plan.steps.map((step, index) => <li key={`${step.phase}-${index}`}><strong>{CATEGORY_LABEL[String(step.request.category) as CategoryUsage["kind"]] ?? String(step.request.category ?? step.phase)}</strong> · {String(step.request.relative_path ?? "Ruta no disponible")} · {typeof step.request.size_bytes === "string" ? formatBytes(step.request.size_bytes) : "Tamaño desconocido"} · {String(step.request.reason ?? "")}</li>)}</ul>
        <p className="detail">Plan {plan.plan_hash.slice(0, 12)}… · caduca {new Date(plan.expires_at).toLocaleString("es-ES")}.</p>
      </section>}
    </div>}
  </section>;
}
