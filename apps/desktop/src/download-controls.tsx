import { useRef, useState } from "react";
import { ApiClient, ApiError } from "./api/client";

type Command = "pause" | "resume" | "cancel";
interface ControlRequest { command: Command; key: string; previousState: string; outcome: "sending" | "accepted" | "unknown" | "rejected" }
interface Receipt { job_id: string; status_url: string; state: string }
const ACCEPTED: Record<Command, string> = {
  pause: "Pausa solicitada. LARM conservará el último punto seguro y la reserva de espacio.",
  resume: "Reanudación solicitada. LARM comprobará la fuente antes de continuar.",
  cancel: "Cancelación solicitada. La reserva se liberará después de cerrar el conector y limpiar los temporales.",
};

function rejection(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 403) return "Tu cuenta no tiene permiso para cambiar esta descarga.";
    if (error.status === 404) return "La descarga ya no está disponible. Actualiza la cola.";
    if (error.status === 409) return "El estado de la descarga cambió. Actualiza la cola antes de repetir la acción.";
    if (error.status === 401) return "La sesión caducó. Vuelve a conectar con LARM.";
  }
  return "LARM rechazó la solicitud. Actualiza la cola y revisa el estado de la descarga.";
}

export function DownloadControls({ client, id, state, onRefresh }: { client: ApiClient; id: string; state: string; onRefresh: () => void }) {
  const [confirming, setConfirming] = useState(false);
  const [request, setRequest] = useState<ControlRequest | null>(null);
  const [error, setError] = useState<string | null>(null);
  const busy = useRef(false);
  const sending = request?.outcome === "sending";
  const uncertain = request?.outcome === "unknown";
  const awaitingState = request?.outcome === "accepted" && state === request.previousState;
  const locked = sending || uncertain || awaitingState;
  const canCancel = ["PLANNED", "QUEUED", "RUNNING", "PAUSING", "PAUSED", "VERIFYING", "FAILED"].includes(state);

  const send = async (command: Command, retry = false) => {
    if (busy.current || (locked && !retry)) return;
    const next: ControlRequest = retry && request?.outcome === "unknown"
      ? { ...request, outcome: "sending" }
      : { command, key: `download-${command}-${crypto.randomUUID()}`, previousState: state, outcome: "sending" };
    busy.current = true; setRequest(next); setError(null); setConfirming(false);
    try {
      const response = await client.requestRaw<Receipt>({
        method: "POST", path: `/api/v1/downloads/${id}/${next.command}`,
        headers: { "Idempotency-Key": next.key }, body: { reason: "operator_request", approval_id: null },
      });
      if (response.status !== 202 || response.body?.job_id !== id || response.body.status_url !== `/api/v1/jobs/${id}`) {
        throw new ApiError("UNEXPECTED_RESPONSE", "No se pudo comprobar el recibo", response.status, true, null, "download_control_receipt_invalid");
      }
      setRequest({ ...next, outcome: "accepted" });
      onRefresh();
    } catch (failure) {
      const unknown = !(failure instanceof ApiError) || failure.isOffline || failure.status >= 500 || failure.code === "UNEXPECTED_RESPONSE";
      setRequest({ ...next, outcome: unknown ? "unknown" : "rejected" });
      setError(unknown
        ? "Se perdió la respuesta. Comprueba la solicitud para recuperar el mismo recibo sin repetir la orden."
        : rejection(failure));
    } finally { busy.current = false; }
  };

  return <div className="download-controls" aria-label="Controles de descarga" aria-busy={sending}>
    <div className="actions">
      {state === "RUNNING" && <button type="button" className="secondary" disabled={locked} onClick={() => void send("pause")}>Pausar</button>}
      {state === "PAUSED" && <button type="button" disabled={locked} onClick={() => void send("resume")}>Reanudar</button>}
      {canCancel && <button type="button" className="secondary" disabled={locked} onClick={() => setConfirming(true)}>Cancelar descarga…</button>}
      {uncertain && <button type="button" onClick={() => void send(request.command, true)}>Comprobar solicitud</button>}
    </div>
    {confirming && canCancel && !locked && <section aria-label="Confirmar cancelación">
      <p>Se eliminarán los archivos parciales de esta descarga. Los archivos ya guardados en la biblioteca se conservan.</p>
      <div className="actions">
        <button type="button" onClick={() => void send("cancel")}>Confirmar cancelación</button>
        <button type="button" className="secondary" onClick={() => setConfirming(false)}>Conservar descarga</button>
      </div>
    </section>}
    {sending && <p role="status">Enviando solicitud…</p>}
    {!confirming && request?.outcome === "accepted" && <p role="status">{ACCEPTED[request.command]}</p>}
    {error && <p className="panel-error" role="alert">{error}</p>}
    {state === "PAUSING" && <p className="detail">Guardando el último punto seguro y cerrando el conector…</p>}
    {state === "PAUSED" && <p className="detail">Se conservan los archivos parciales y la reserva de espacio.</p>}
    {state === "CANCELLING" && <p className="detail">Cerrando el conector y limpiando los temporales. La reserva sigue retenida hasta terminar.</p>}
  </div>;
}
