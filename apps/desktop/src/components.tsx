import { Component, createContext, useCallback, useContext, useMemo, useState, type ErrorInfo, type ReactNode } from "react";
import type { DaemonStatus } from "./api/status";
import { STATUS_LABEL } from "./api/status";

/** Captura errores de render de una pantalla sin tumbar el shell. */
export class ErrorBoundary extends Component<{ children: ReactNode; resetKey: string }, { error: Error | null }> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // Solo a la consola del WebView; sin datos de usuario.
    console.error("error de pantalla", error.message, info.componentStack?.slice(0, 500));
  }

  componentDidUpdate(prev: { resetKey: string }) {
    if (prev.resetKey !== this.props.resetKey && this.state.error) {
      this.setState({ error: null });
    }
  }

  render() {
    if (this.state.error) {
      return (
        <section role="alert" className="panel panel-error">
          <h2>Esta pantalla ha fallado</h2>
          <p>El resto de LARM sigue funcionando. Puedes volver a intentarlo o ir a otra pantalla.</p>
          <p className="mono">{this.state.error.message}</p>
          <button type="button" onClick={() => this.setState({ error: null })}>
            Reintentar
          </button>
        </section>
      );
    }
    return this.props.children;
  }
}

export interface Toast {
  id: number;
  kind: "info" | "error";
  text: string;
}

interface ToastApi {
  notify: (kind: Toast["kind"], text: string) => void;
}

const ToastContext = createContext<ToastApi>({ notify: () => undefined });

export function useToasts(): ToastApi {
  return useContext(ToastContext);
}

/** Avisos no intrusivos: región `status`/`alert`, se cierran solos o a mano. */
export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const dismiss = useCallback((id: number) => setToasts((t) => t.filter((x) => x.id !== id)), []);
  const notify = useCallback(
    (kind: Toast["kind"], text: string) => {
      const id = Date.now() + Math.random();
      setToasts((t) => [...t.slice(-3), { id, kind, text }]);
      window.setTimeout(() => dismiss(id), kind === "error" ? 10000 : 5000);
    },
    [dismiss],
  );
  const api = useMemo(() => ({ notify }), [notify]);
  return (
    <ToastContext.Provider value={api}>
      {children}
      <div className="toasts" aria-live="polite" aria-relevant="additions">
        {toasts.map((t) => (
          <div key={t.id} className={`toast toast-${t.kind}`} role={t.kind === "error" ? "alert" : "status"}>
            <span>{t.text}</span>
            <button type="button" className="toast-close" aria-label="Cerrar aviso" onClick={() => dismiss(t.id)}>
              ×
            </button>
          </div>
        ))}
      </div>
    </ToastContext.Provider>
  );
}

/** Estado del daemon con texto (no solo color). */
export function StatusBadge({ status }: { status: DaemonStatus }) {
  return (
    <span className={`badge badge-${status.toLowerCase()}`} data-testid="daemon-status" data-status={status}>
      <span aria-hidden="true" className="dot" />
      Daemon: {STATUS_LABEL[status]}
    </span>
  );
}
