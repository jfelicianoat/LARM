import { useEffect, useState } from "react";
import { ApiClient } from "../../api/client";
import { listNotifications, type NotificationItem } from "../../api/notifications";
import { safeNotificationDestination } from "../Notifications";

export function UpdatesPage({ client, onNavigate }: { client: ApiClient; onNavigate: (screen: string, resourceId?: string) => void }) {
  const [candidates, setCandidates] = useState<NotificationItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let active = true;
    setLoading(true); setError(null);
    void listNotifications(client).then((items) => {
      if (active) setCandidates(items.filter((item) => item.message_code === "RUNTIME_UPDATE" || item.message_code === "MODEL_UPDATE"));
    }).catch(() => { if (active) setError("No se pudieron consultar los candidatos. Comprueba la conexión con LARM y reintenta."); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [client, revision]);

  return <div className="stack updates-page">
    <section className="panel stack" aria-labelledby="updates-title">
      <div className="notification-heading"><h2 id="updates-title">Candidatos de actualización</h2><button type="button" className="secondary" disabled={loading} onClick={() => setRevision((value) => value + 1)}>Actualizar</button></div>
      <p>Esta pantalla informa de nuevas revisiones y runtimes candidatos. LARM no instala ni promueve una versión automáticamente.</p>
      {loading && <p role="status">Buscando candidatos registrados…</p>}
      {!loading && !candidates.length && !error && <p>No hay candidatos de actualización registrados.</p>}
      {candidates.map((item) => {
        const target = safeNotificationDestination(item);
        const isRuntime = item.message_code === "RUNTIME_UPDATE";
        const name = typeof item.metadata.display_name === "string" ? item.metadata.display_name
          : typeof item.metadata.runtime_name === "string" ? item.metadata.runtime_name
            : isRuntime ? "Runtime candidato" : "Modelo con nueva revisión";
        return <article className="device update-candidate" key={item.id} data-severity={item.severity}>
          <div className="notification-heading"><h3>{name}</h3><span className="badge">{isRuntime ? "Runtime" : "Modelo"}</span></div>
          <p>{isRuntime
            ? "Hay una actualización disponible para revisar. Antes de aplicarla, se necesita un plan que detalle la versión, los cambios y la recuperación."
            : "Se detectó una revisión nueva. Revisa el origen y los detalles antes de preparar una descarga."}</p>
          <p className="detail">Detectado {new Date(item.created_at).toLocaleString("es-ES")} · estado: candidato pendiente de revisión</p>
          {target && <button type="button" className="secondary" onClick={() => onNavigate(target.screen, target.resourceId)}>Abrir detalle</button>}
        </article>;
      })}
      {error && <p role="alert" className="state-unknown">{error}</p>}
    </section>
    <section className="panel"><h2>Actualización segura</h2><p>El cambio solo puede continuar cuando LARM disponga de un plan revisable y de una acción confirmada por la persona operadora.</p></section>
  </div>;
}
