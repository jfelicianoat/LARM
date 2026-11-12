import { useId, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import type { ModelDetail } from "./models";

interface PlanStep { phase: string; adapter_id: string | null; request: Record<string, unknown> }
interface DeletePlan {
  plan_id: string;
  plan_hash: string;
  operation: string;
  resource_ids: string[];
  storage_estimate: { reason: string };
  steps: PlanStep[];
  expires_at: string;
  approval_required: boolean;
}

const ACTION_LABEL: Record<string, string> = {
  DELETE_FILE: "Eliminar archivo gestionado por LARM",
  DEREGISTER: "Retirar registro y conservar el archivo",
  KEEP_SHARED: "Conservar el archivo compartido",
};

function explain(error: unknown): string {
  if (error instanceof ApiError && error.isOffline) return "Se perdió la conexión con LARM. Comprueba el estado del plan antes de reintentar.";
  if (error instanceof ApiError && error.status === 403) return "Tu cuenta necesita permisos de modelos y administración para aprobar este borrado.";
  if (error instanceof ApiError && error.reason?.includes("approval")) return "LARM no aceptó la aprobación ligada al plan. Prepara un plan nuevo y vuelve a revisar sus acciones.";
  if (error instanceof ApiError && error.reason?.includes("stale")) return "El modelo cambió desde la vista previa. Prepara y revisa un plan nuevo.";
  return "LARM rechazó el borrado. Actualiza el modelo y vuelve a revisar el plan.";
}

export function ModelDelete({ detail, client, onCompleted }: { detail: ModelDetail; client: ApiClient; onCompleted: () => void }) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const [plan, setPlan] = useState<DeletePlan | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const expired = plan !== null && Date.parse(plan.expires_at) <= Date.now();

  const prepare = async () => {
    setBusy(true); setError(null); setPlan(null); setConfirmed(false); setDone(false);
    try {
      const value = await client.request<DeletePlan>({
        method: "POST", path: `/api/v1/models/${encodeURIComponent(detail.variant.id)}/delete-plan`,
        headers: { "Idempotency-Key": `model-delete-plan-${crypto.randomUUID()}` },
        body: { resource_id: detail.variant.id, profile_id: null },
      });
      if (value.operation !== "model.delete" || !value.approval_required) throw new Error("delete_plan_response_invalid");
      setPlan(value);
    } catch (e) { setError(e instanceof Error && e.message === "delete_plan_response_invalid" ? "LARM devolvió un plan de borrado no válido." : explain(e)); }
    finally { setBusy(false); }
  };

  const execute = async () => {
    if (busy || !plan || !confirmed || expired) return;
    setBusy(true); setError(null);
    try {
      const approval = await client.request<{ resource_id: string }>({
        method: "POST", path: "/api/v1/approvals",
        headers: { "Idempotency-Key": `model-delete-approval-${crypto.randomUUID()}` },
        body: { operation_digest: plan.plan_hash, resource_ids: plan.resource_ids, code_hash: null, permissions: {}, approved: true },
      });
      await client.request({
        method: "DELETE", path: `/api/v1/models/${encodeURIComponent(detail.variant.id)}?approval_id=${encodeURIComponent(approval.resource_id)}&plan_hash=${encodeURIComponent(plan.plan_hash)}`,
        headers: { "If-Match": String(detail.variant.row_version) },
      });
      setDone(true); setPlan(null); onCompleted();
    } catch (e) { setError(explain(e)); }
    finally { setBusy(false); }
  };

  if (done) return <p role="status" className="state-verified">El borrado terminó según el plan y se actualizó el inventario local.</p>;
  return <section className="model-delete">
    <button type="button" className="secondary" aria-expanded={open} onClick={() => { setOpen((value) => !value); setError(null); setPlan(null); setConfirmed(false); }}>{open ? "Cerrar borrado" : "Eliminar modelo local"}</button>
    {open && <div className="delete-review">
      <h3>Revisar borrado</h3>
      <p>LARM preparará un plan separado por archivo. Los archivos compartidos o externos se conservan. Nada se borra al preparar el plan.</p>
      {!plan && <button type="button" disabled={busy} onClick={() => void prepare()}>{busy ? "Preparando plan…" : "Preparar plan de borrado"}</button>}
      {error && <p role="alert" className="state-unknown">{error}</p>}
      {plan && <section aria-labelledby={`${id}-plan`}>
        <h4 id={`${id}-plan`}>Acciones previstas por archivo</h4>
        {plan.steps.length ? <ul>{plan.steps.map((step, index) => <li key={`${step.phase}-${index}`}><strong>{ACTION_LABEL[step.phase] ?? step.phase}</strong> · {String(step.request.relative_path ?? "Ruta no disponible")}</li>)}</ul> : <p>El plan no contiene ubicaciones de archivo.</p>}
        <p className="detail">Caduca {new Date(plan.expires_at).toLocaleString("es-ES")} · hash <span className="mono">{plan.plan_hash}</span>. Para continuar se exige aprobación ligada a este hash y al estado actual de la variante.</p>
        {expired && <p role="alert" className="state-unknown">El plan caducó. Prepara otro y vuelve a revisar sus acciones.</p>}
        <label className="check"><input type="checkbox" checked={confirmed} disabled={busy || expired} onChange={(event) => setConfirmed(event.target.checked)} />He revisado cada acción y confirmo solicitar el borrado de esta variante.</label>
        <button type="button" disabled={!confirmed || busy || expired} onClick={() => void execute()}>{busy ? "Solicitando aprobación y borrado…" : "Aprobar y borrar según el plan"}</button>
      </section>}
    </div>}
  </section>;
}
