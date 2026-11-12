import { useEffect, useState } from "react";
import { ApiClient, ApiError } from "./api/client";

/** Etiquetas en español del ciclo P120. */
export const PROMOTION_LABEL: Record<string, string> = {
  NONE: "Sin promoción",
  CANDIDATE: "Candidata",
  TECHNICALLY_APPROVED: "Aprobada técnicamente",
  EVALUATION_APPROVED: "Aprobada en evaluación",
  DEFAULT: "Predeterminada",
  PINNED: "Anclada",
  REJECTED: "Rechazada",
};

/** El siguiente escalón legal según la máquina de estados P120. */
function nextPromotion(from: string): string | null {
  switch (from) {
    case "NONE": return "CANDIDATE";
    case "CANDIDATE": return "TECHNICALLY_APPROVED";
    case "TECHNICALLY_APPROVED": return "EVALUATION_APPROVED";
    case "EVALUATION_APPROVED": return "DEFAULT";
    default: return null;
  }
}

function explain(error: unknown): string {
  if (error instanceof ApiError && error.isOffline) return "Se perdió la conexión con LARM. Comprueba la conexión e inténtalo de nuevo.";
  if (error instanceof ApiError && error.status === 403) return "Tu cuenta no tiene permiso para promocionar variantes.";
  if (error instanceof ApiError && error.status === 409) return "La variante cambió mientras decidías. Revisa su estado y vuelve a intentarlo.";
  if (error instanceof ApiError && error.status === 422) return "Esa transición de promoción no está permitida.";
  return "No se pudo completar la operación. Comprueba la conexión e inténtalo de nuevo.";
}

/** Panel de promoción P120: el siguiente escalón legal más las salidas laterales. */
export function ModelPromote({ client, variantId, promotion, rowVersion, onCompleted }: {
  client: ApiClient;
  variantId: string;
  promotion: string;
  rowVersion: number;
  onCompleted: () => void;
}) {
  const [confirming, setConfirming] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [current, setCurrent] = useState(promotion);
  const [version, setVersion] = useState(rowVersion);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    setCurrent(promotion);
    setVersion(rowVersion);
  }, [promotion, rowVersion]);

  const next = nextPromotion(current);
  const laterals = current === "DEFAULT" ? ["PINNED"] : current === "REJECTED" ? [] : ["REJECTED", "PINNED"];

  const promote = async (to: string) => {
    setBusy(true);
    setError(null);
    try {
      const reply = await client.request<{ row_version: number }>({
        method: "PATCH",
        path: `/api/v1/variants/${encodeURIComponent(variantId)}/promotion`,
        body: { promotion: to, expected_row_version: version },
      });
      setCurrent(to);
      setVersion(reply.row_version);
      setMessage(`La variante es ahora: ${PROMOTION_LABEL[to] ?? to}.`);
      setConfirming(null);
      onCompleted();
    } catch (e) {
      setError(explain(e));
    } finally {
      setBusy(false);
    }
  };

  return <section className="panel model-promote" aria-labelledby="promote-heading">
    <h3 id="promote-heading">Promoción de la variante</h3>
    <p>Estado actual: <strong>{PROMOTION_LABEL[current] ?? current}</strong></p>
    {!open && next && <div className="actions">
      <button type="button" onClick={() => setOpen(true)}>Promocionar a {PROMOTION_LABEL[next] ?? next}</button>
    </div>}
    {open && next && !message && <p className="detail">
      ¿Promocionar esta variante a <strong>{PROMOTION_LABEL[next] ?? next}</strong>?{next === "DEFAULT" ? " La variante predeterminada anterior de esta revisión y formato pasará a Aprobada en evaluación." : ""}
    </p>}
    {open && next && !message && <div className="actions">
      <button type="button" disabled={busy} onClick={() => void promote(next!)}>{busy ? "Promocionando…" : "Confirmar promoción"}</button>
      <button type="button" className="secondary" disabled={busy} onClick={() => setOpen(false)}>Cancelar</button>
    </div>}
    {laterals.length > 0 && !message && <div className="actions">
      {laterals.map((lateral) => confirming === lateral
        ? <button key={lateral} type="button" className="secondary" disabled={busy} onClick={() => void promote(lateral)}>Confirmar {PROMOTION_LABEL[lateral] ?? lateral}</button>
        : <button key={lateral} type="button" className="link" disabled={busy} onClick={() => setConfirming(lateral)}>Marcar como {PROMOTION_LABEL[lateral] ?? lateral}…</button>)}
    </div>}
    {message && <p role="status" className="state-verified">{message}</p>}
    {error && <p role="alert" className="state-unknown">{error}</p>}
  </section>;
}
