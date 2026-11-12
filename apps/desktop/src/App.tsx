import { useEffect, useMemo, useRef } from "react";
import { ApiClient, tauriTransport, type Transport } from "./api/client";
import { ErrorBoundary, StatusBadge, ToastProvider, useToasts } from "./components";
import { useDaemonStatus, useHashRoute } from "./hooks";
import { HardwarePage } from "./hardware";
import { StoragePage } from "./storage";
import { SourcesPage } from "./sources";
import { AdaptersPage } from "./adapters";
import { CustomRulesPage } from "./customRules";
import { BandwidthSettings } from "./settings";
import { LowDiskThresholdSettings } from "./settings";
import { RuntimesPage } from "./runtimes";
import { InstancesPage } from "./instances";
import { ModelsPage } from "./models";
import { DownloadsPage } from "./downloads";
import { HomePage } from "./home";
import { DiscoverPage } from "./discover";
import { NotificationsPage } from "./pages/Notifications";
import { UpdatesPage } from "./pages/Updates";
import { DiagnosticsPage, ScreenPage } from "./pages";
import { DIAGNOSTICS, SCREENS, screenById } from "./screens";
import "./styles.css";

/** Shell de LARM (P006): navegación de 14 pantallas y estado del daemon. */
export default function App({ transport = tauriTransport, pollMs = 5000 }: { transport?: Transport; pollMs?: number }) {
  return (
    <ToastProvider>
      <Shell transport={transport} pollMs={pollMs} />
    </ToastProvider>
  );
}

function Shell({ transport, pollMs }: { transport: Transport; pollMs: number }) {
  const client = useMemo(() => new ApiClient(transport), [transport]);
  const [snapshot, refresh] = useDaemonStatus(client, pollMs);
  const [route, go] = useHashRoute();
  const [routeId, routeQuery = ""] = route.split("?", 2);
  const resourceId = new URLSearchParams(routeQuery).get("id") ?? undefined;
  const screen = screenById(routeId);
  const heading = useRef<HTMLHeadingElement>(null);
  const { notify } = useToasts();
  const previous = useRef(snapshot.status);

  // Foco en el título al cambiar de pantalla (navegación por teclado y lectores).
  useEffect(() => {
    heading.current?.focus();
  }, [screen.id]);

  // Aviso no intrusivo solo cuando cambia la conexión.
  useEffect(() => {
    const before = previous.current;
    previous.current = snapshot.status;
    if (before === snapshot.status || before === "CONNECTING") return;
    if (snapshot.status === "OFFLINE") notify("error", "Se ha perdido la conexión con el daemon.");
    if (snapshot.status === "ONLINE") notify("info", "Conexión con el daemon restablecida.");
  }, [snapshot.status, notify]);

  return (
    <div className="shell">
      <a className="skip" href="#contenido">Saltar al contenido</a>
      <header className="topbar">
        <span className="brand">LARM</span>
        <StatusBadge status={snapshot.status} />
        <button type="button" className="secondary" onClick={() => go(DIAGNOSTICS.id)}>
          Diagnóstico
        </button>
      </header>
      <nav className="sidebar" aria-label="Secciones">
        <ul>
          {SCREENS.map((s) => (
            <li key={s.id}>
              <a href={`#/${s.id}`} aria-current={s.id === screen.id ? "page" : undefined}>
                {s.title}
              </a>
            </li>
          ))}
        </ul>
      </nav>
      <main id="contenido" className="content">
        <h1 ref={heading} tabIndex={-1}>{screen.title}</h1>
        <ErrorBoundary resetKey={screen.id}>
          {screen.id === DIAGNOSTICS.id ? (
            <DiagnosticsPage snapshot={snapshot} onRetry={() => void refresh()} />
          ) : (
            <ScreenPage screen={screen} snapshot={snapshot} onRetry={() => void refresh()} onDiagnostics={() => go(DIAGNOSTICS.id)}>
              {screen.id === "inicio" ? (
                <HomePage client={client} snapshot={snapshot} />
              ) : screen.id === "descubrir" ? (
                <DiscoverPage client={client} navigate={(target, id) => go(id ? `${target}?id=${encodeURIComponent(id)}` : target)} />
              ) : screen.id === "hardware" ? (
                <HardwarePage client={client} />
              ) : screen.id === "almacenamiento" ? (
                <StoragePage client={client} focusResourceId={resourceId} />
              ) : screen.id === "runtimes" ? (
                <RuntimesPage client={client} focusResourceId={resourceId} />
              ) : screen.id === "instancias" ? (
                <InstancesPage client={client} />
              ) : screen.id === "fuentes" ? (
                <SourcesPage client={client} focusResourceId={resourceId} />
              ) : screen.id === "adaptadores" ? (
                <AdaptersPage client={client} />
              ) : screen.id === "mis-modelos" ? (
                <ModelsPage client={client} focusResourceId={resourceId} />
              ) : screen.id === "descargas" ? (
                <DownloadsPage client={client} />
              ) : screen.id === "configuracion" ? (
                <div className="settings-page">
                  <BandwidthSettings client={client} />
                  <LowDiskThresholdSettings client={client} />
                  <CustomRulesPage client={client} />
                </div>
              ) : screen.id === "notificaciones" ? (
                <NotificationsPage client={client} onNavigate={(target, id) => go(id ? `${target}?id=${encodeURIComponent(id)}` : target)} />
              ) : screen.id === "actualizaciones" ? (
                <UpdatesPage client={client} onNavigate={(target, id) => go(id ? `${target}?id=${encodeURIComponent(id)}` : target)} />
              ) : undefined}
            </ScreenPage>
          )}
        </ErrorBoundary>
      </main>
    </div>
  );
}
