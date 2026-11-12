import type { ReactNode } from "react";
import type { StatusSnapshot } from "./api/status";
import { STATUS_LABEL } from "./api/status";
import type { ScreenDef } from "./screens";

/**
 * Marco común de las 14 pantallas. Hasta que su prompt construya el
 * contenido, la pantalla lo dice: no hay datos simulados en producción.
 */
export function ScreenPage({ screen, snapshot, onRetry, onDiagnostics, children }: {
  screen: ScreenDef;
  snapshot: StatusSnapshot;
  onRetry: () => void;
  onDiagnostics: () => void;
  /** Contenido de la pantalla cuando su prompt ya lo ha construido. */
  children?: ReactNode;
}) {
  if (snapshot.status === "OFFLINE") {
    return (
      <section className="panel panel-offline" aria-labelledby="estado-offline">
        <h2 id="estado-offline">No hay conexión con el daemon de LARM</h2>
        <p>Los datos de esta pantalla vienen del daemon. Cuando vuelva a responder se cargarán solos.</p>
        <div className="actions">
          <button type="button" onClick={onRetry}>Reintentar ahora</button>
          <button type="button" className="secondary" onClick={onDiagnostics}>Ver diagnóstico</button>
        </div>
      </section>
    );
  }
  if (snapshot.status === "CONNECTING") {
    return (
      <section className="panel" aria-busy="true">
        <p>Conectando con el daemon…</p>
      </section>
    );
  }
  const degraded = snapshot.status === "DEGRADED" && (
    <p className="notice" role="status">
      El daemon responde pero está degradado. <button type="button" className="link" onClick={onDiagnostics}>Ver diagnóstico</button>
    </p>
  );
  if (children) {
    return (
      <>
        {degraded}
        {children}
      </>
    );
  }
  return (
    <section className="panel">
      {degraded}
      <p>
        El contenido de <strong>{screen.title}</strong> todavía no está disponible en esta versión. Llega con {screen.owners}.
      </p>
    </section>
  );
}

/** Página de diagnóstico cuando la API no responde (P006). */
export function DiagnosticsPage({ snapshot, onRetry }: { snapshot: StatusSnapshot; onRetry: () => void }) {
  const e = snapshot.error;
  return (
    <section className="panel" aria-labelledby="diag-titulo">
      <h2 id="diag-titulo">Estado de la conexión</h2>
      <dl className="facts">
        <dt>Estado</dt>
        <dd data-testid="diag-status">{STATUS_LABEL[snapshot.status]}</dd>
        <dt>Última comprobación</dt>
        <dd>{snapshot.checkedAt ? snapshot.checkedAt.toLocaleString("es-ES") : "todavía no"}</dd>
        {e && (
          <>
            <dt>Error</dt>
            <dd className="mono" data-testid="diag-error">{e.code}{e.reason ? ` (${e.reason})` : ""}</dd>
            <dt>Mensaje</dt>
            <dd>{e.message}</dd>
            {e.correlationId && (
              <>
                <dt>Correlación</dt>
                <dd className="mono">{e.correlationId}</dd>
              </>
            )}
          </>
        )}
      </dl>
      {snapshot.status === "OFFLINE" && (
        <>
          <h3>Qué comprobar</h3>
          <ol>
            <li>Que el daemon esté en marcha: <span className="mono">larm-daemon --serve-api</span>.</li>
            <li>Que se ejecute con el mismo usuario de Windows que esta aplicación: solo ese usuario puede pedir el acceso.</li>
            <li>Los registros en <span className="mono">%LOCALAPPDATA%\LARM\logs</span>.</li>
          </ol>
        </>
      )}
      <div className="actions">
        <button type="button" onClick={onRetry}>Comprobar de nuevo</button>
      </div>
    </section>
  );
}
