import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { ApiClient, ApiError } from "../../api/client";
import {
  createSourceWatch,
  deleteSourceWatch,
  listNotifications,
  listSourceWatches,
  setNotificationRead,
  updateSourceWatch,
  type NotificationItem,
  type SourceWatch,
  type SourceWatchInput,
} from "../../api/notifications";

interface SourceOption { id: string; name: string; enabled: boolean }
interface Page<T> { items: T[]; next_cursor: string | null; snapshot_at: string }
type Destination = { screen: string; resourceId?: string };

const UUID7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const LABELS: Record<string, string> = {
  NEW_MODEL: "Modelo nuevo detectado",
  NEW_COMPATIBLE_MODEL: "Modelo nuevo compatible",
  MODEL_UPDATE: "Hay una revisión nueva de un modelo",
  LOW_DISK_SPACE: "Cambio de nivel de espacio en disco",
  STORAGE_OFFLINE: "Ubicación de almacenamiento sin conexión",
  MODEL_REGRESSION: "Cambio de compatibilidad del modelo",
  RUNTIME_UPDATE: "Actualización de runtime disponible",
  RUNTIME_REGRESSION: "Cambio detectado en runtime",
  DOWNLOAD_COMPLETED: "Descarga completada",
};

export function safeNotificationDestination(item: NotificationItem): Destination | null {
  const route = item.action?.route;
  const resourceId = item.action?.resource_id ?? item.resource_id;
  if (route === "/storage" && (resourceId === null || resourceId === undefined)) return { screen: "almacenamiento" };
  if (typeof route !== "string" || typeof resourceId !== "string" || !UUID7.test(resourceId)) return null;
  const match = /^\/(sources|models|runtimes|storage)\/([0-9a-f-]+)$/i.exec(route);
  if (!match || match[2].toLowerCase() !== resourceId.toLowerCase()) return null;
  const screen = ({ sources: "fuentes", models: "mis-modelos", runtimes: "runtimes", storage: "almacenamiento" } as const)[match[1].toLowerCase() as "sources" | "models" | "runtimes" | "storage"];
  return screen ? { screen, resourceId } : null;
}

export function dedupeNotifications(items: NotificationItem[]): NotificationItem[] {
  const seen = new Set<string>();
  return items.filter((item) => {
    if (seen.has(item.dedupe_key)) return false;
    seen.add(item.dedupe_key);
    return true;
  });
}

export function notificationTitle(item: NotificationItem): string {
  return LABELS[item.message_code] ?? "Aviso de LARM";
}

function explain(error: unknown): string {
  if (!(error instanceof ApiError)) return "No se pudo completar la operación. Vuelve a intentarlo.";
  if (error.isOffline) return "LARM no está disponible. Los datos se mostrarán cuando vuelva la conexión.";
  if (error.reason === "stale_revision") return "El aviso cambió en otra sesión. Actualiza la lista antes de modificarlo.";
  if (error.status === 403) return "Tu cuenta no tiene permiso para cambiar avisos o seguimientos.";
  if (error.reason === "watch_auto_download_not_implemented") return "La descarga automática todavía no está disponible.";
  return error.message;
}

function notificationDetail(item: NotificationItem): string {
  const metadata = item.metadata;
  if (item.message_code === "LOW_DISK_SPACE") {
    const level = typeof metadata.level === "string" ? metadata.level : "Desconocido";
    const drive = typeof metadata.drive_letter === "string" ? ` · ${metadata.drive_letter}` : "";
    const free = typeof metadata.free_bytes === "string" ? metadata.free_bytes : null;
    const capacity = typeof metadata.capacity_bytes === "string" ? metadata.capacity_bytes : null;
    const exact = free && capacity ? ` · ${free} bytes libres de ${capacity}` : "";
    return `${level === "NORMAL" ? "Espacio recuperado" : `Nivel ${level}`}${drive}${exact}`;
  }
  const eventType = metadata.event_type;
  return typeof eventType === "string" && eventType !== item.message_code ? `Tipo: ${eventType}` : "";
}

function NotificationCard({ item, onRead, onNavigate, busy }: {
  item: NotificationItem;
  busy: boolean;
  onRead: (item: NotificationItem, read: boolean) => void;
  onNavigate: (destination: Destination) => void;
}) {
  const destination = safeNotificationDestination(item);
  const detail = notificationDetail(item);
  return <article className={`device notification-card ${item.read_at ? "is-read" : "is-unread"}`} data-severity={item.severity}>
    <div className="notification-heading">
      <h3>{notificationTitle(item)}</h3>
      {!item.read_at && <span className={`badge severity-${item.severity}`}>{item.severity === "error" ? "Crítico" : item.severity === "warning" ? "Aviso" : "Información"}</span>}
      {item.read_at && <span className="detail">Leída</span>}
    </div>
    {detail && <p>{detail}</p>}
    <p className="detail">{new Date(item.created_at).toLocaleString("es-ES")}</p>
    <div className="actions">
      {destination && <button type="button" className="secondary" onClick={() => onNavigate(destination)}>Ver detalle</button>}
      <button type="button" className="secondary" disabled={busy} onClick={() => onRead(item, !item.read_at)}>
        {busy ? "Guardando…" : item.read_at ? "Marcar como no leída" : "Marcar leída y descartar"}
      </button>
    </div>
  </article>;
}

interface WatchDraft {
  source_id: string;
  q: string;
  capabilities: string;
  compatibility: "ALL" | "COMPATIBLE" | "UNKNOWN" | "INCOMPATIBLE";
  minimum: string;
  maximum: string;
  interval_hours: string;
  enabled: boolean;
}

function draftFor(watch?: SourceWatch): WatchDraft {
  const query = watch?.query ?? {};
  const range = query.size_bytes && typeof query.size_bytes === "object" ? query.size_bytes as Record<string, unknown> : {};
  return {
    source_id: watch?.source_id ?? "",
    q: typeof query.q === "string" ? query.q : "",
    capabilities: Array.isArray(query.capabilities) ? query.capabilities.filter((v): v is string => typeof v === "string").join(", ") : "",
    compatibility: query.compatibility === "COMPATIBLE" || query.compatibility === "UNKNOWN" || query.compatibility === "INCOMPATIBLE" ? query.compatibility : "ALL",
    minimum: typeof range.minimum === "string" ? range.minimum : "",
    maximum: typeof range.maximum === "string" ? range.maximum : "",
    interval_hours: String(Math.max(1, Math.round((watch?.interval_seconds ?? 21_600) / 3600))),
    enabled: watch?.enabled ?? true,
  };
}

const MAX_SIZE = 9_223_372_036_854_775_807n;
function decimalOrEmpty(value: string): boolean {
  if (!value) return true;
  if (!/^(0|[1-9][0-9]*)$/.test(value)) return false;
  try { return BigInt(value) <= MAX_SIZE; } catch { return false; }
}

function inputFor(draft: WatchDraft): SourceWatchInput | null {
  const sourceId = draft.source_id.trim();
  const hours = Number(draft.interval_hours);
  if (!UUID7.test(sourceId) || !Number.isSafeInteger(hours) || hours < 1 || hours > Math.floor(2_147_483_647 / 3600)) return null;
  if (!decimalOrEmpty(draft.minimum) || !decimalOrEmpty(draft.maximum)) return null;
  if (draft.minimum && draft.maximum && BigInt(draft.minimum) > BigInt(draft.maximum)) return null;
  const capabilities = [...new Set(draft.capabilities.split(/[\n,;]/).map((value) => value.trim()).filter(Boolean))];
  const query: Record<string, unknown> = { compatibility: draft.compatibility };
  if (draft.q.trim()) query.q = draft.q.trim();
  if (capabilities.length) query.capabilities = capabilities;
  if (draft.minimum || draft.maximum) query.size_bytes = { minimum: draft.minimum || null, maximum: draft.maximum || null };
  return { source_id: sourceId, query, interval_seconds: hours * 3600, policy: {}, enabled: draft.enabled };
}

function WatchForm({ sources, initial, busy, onSave, onCancel }: {
  sources: SourceOption[];
  initial?: SourceWatch;
  busy: boolean;
  onSave: (input: SourceWatchInput) => Promise<void>;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState(() => draftFor(initial));
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const input = inputFor(draft);
  const field = (name: string) => `${id}-${name}`;
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!input) { setError("Revisa el origen, el intervalo y el rango de tamaño."); return; }
    setError(null);
    try { await onSave(input); } catch (reason) { setError(explain(reason)); }
  };
  const update = <K extends keyof WatchDraft>(key: K, value: WatchDraft[K]) => setDraft((current) => ({ ...current, [key]: value }));
  return <form className="panel form watcher-form" onSubmit={(event) => void submit(event)}>
    <h3>{initial ? "Editar seguimiento" : "Nuevo seguimiento de fuente"}</h3>
    <label htmlFor={field("source")}>Fuente</label>
    <select id={field("source")} value={draft.source_id} onChange={(event) => update("source_id", event.target.value)} disabled={busy}>
      <option value="">Selecciona una fuente</option>
      {sources.map((source) => <option key={source.id} value={source.id}>{source.name}{source.enabled ? "" : " · desactivada"}</option>)}
    </select>
    <label htmlFor={field("q")}>Texto de búsqueda (opcional)</label>
    <input id={field("q")} maxLength={500} value={draft.q} onChange={(event) => update("q", event.target.value)} disabled={busy} />
    <label htmlFor={field("capabilities")}>Capacidades (separadas por coma)</label>
    <input id={field("capabilities")} value={draft.capabilities} onChange={(event) => update("capabilities", event.target.value)} placeholder="chat, vision" disabled={busy} />
    <label htmlFor={field("compatibility")}>Compatibilidad</label>
    <select id={field("compatibility")} value={draft.compatibility} onChange={(event) => update("compatibility", event.target.value as WatchDraft["compatibility"])} disabled={busy}>
      <option value="ALL">Todos los resultados</option><option value="COMPATIBLE">Compatibles verificados</option>
      <option value="UNKNOWN">Compatibilidad desconocida</option><option value="INCOMPATIBLE">No compatibles verificados</option>
    </select>
    <p className="detail">LARM no clasificará compatibilidad desconocida como compatible ni incompatible.</p>
    <fieldset className="watch-size-range"><legend>Rango de tamaño exacto (bytes, opcional)</legend>
      <label htmlFor={field("minimum")}>Mínimo</label><input id={field("minimum")} inputMode="numeric" value={draft.minimum} onChange={(event) => update("minimum", event.target.value)} disabled={busy} />
      <label htmlFor={field("maximum")}>Máximo</label><input id={field("maximum")} inputMode="numeric" value={draft.maximum} onChange={(event) => update("maximum", event.target.value)} disabled={busy} />
      <p className="detail">Si el tamaño no tiene un valor exacto, el resultado se omite de este rango.</p>
    </fieldset>
    <label htmlFor={field("interval")}>Frecuencia (horas)</label>
    <input id={field("interval")} inputMode="numeric" value={draft.interval_hours} onChange={(event) => update("interval_hours", event.target.value)} disabled={busy} />
    <label className="check"><input type="checkbox" checked={draft.enabled} onChange={(event) => update("enabled", event.target.checked)} disabled={busy} />Activar seguimiento</label>
    <p className="detail">El seguimiento registra candidatos; nunca descarga ni instala modelos automáticamente.</p>
    {error && <p role="alert" className="state-unknown">{error}</p>}
    <div className="actions"><button type="submit" disabled={busy || !input}>{busy ? "Guardando…" : "Guardar seguimiento"}</button><button type="button" className="secondary" disabled={busy} onClick={onCancel}>Cancelar</button></div>
  </form>;
}

function querySummary(query: Record<string, unknown>): string {
  const parts: string[] = [];
  if (typeof query.q === "string" && query.q) parts.push(`Texto: ${query.q}`);
  if (Array.isArray(query.capabilities) && query.capabilities.length) parts.push(`Capacidades: ${query.capabilities.join(", ")}`);
  const compatibility = typeof query.compatibility === "string" ? query.compatibility : "ALL";
  if (compatibility !== "ALL") parts.push(`Compatibilidad: ${compatibility.toLowerCase()}`);
  const size = query.size_bytes as { minimum?: unknown; maximum?: unknown } | undefined;
  if (size && (size.minimum || size.maximum)) parts.push(`Tamaño: ${String(size.minimum || "sin mínimo")}–${String(size.maximum || "sin máximo")} bytes`);
  return parts.join(" · ") || "Sin filtros adicionales";
}

function WatchersPanel({ client }: { client: ApiClient }) {
  const [watches, setWatches] = useState<SourceWatch[]>([]);
  const [sources, setSources] = useState<SourceOption[]>([]);
  const [form, setForm] = useState<SourceWatch | "new" | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const createAttempt = useRef<{ body: string; key: string } | null>(null);

  useEffect(() => {
    let active = true;
    setLoading(true); setError(null);
    void Promise.all([
      listSourceWatches(client),
      client.get<Page<SourceOption>>("/api/v1/sources?limit=200"),
    ]).then(([nextWatches, page]) => {
      if (!active) return;
      setWatches(nextWatches);
      setSources(page.items);
    }).catch((reason: unknown) => { if (active) setError(explain(reason)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [client, revision]);

  const save = async (input: SourceWatchInput) => {
    const body = JSON.stringify(input);
    setBusyId(form === "new" ? "new" : form?.id ?? "new"); setError(null);
    try {
      if (form === "new") {
        if (createAttempt.current?.body !== body) createAttempt.current = { body, key: crypto.randomUUID() };
        await createSourceWatch(client, input, createAttempt.current.key);
        createAttempt.current = null;
      } else if (form) await updateSourceWatch(client, form, input);
      setForm(null); setRevision((value) => value + 1);
    } finally { setBusyId(null); }
  };

  const remove = async (watch: SourceWatch) => {
    setBusyId(watch.id); setError(null);
    try { await deleteSourceWatch(client, watch); setDeleting(null); setRevision((value) => value + 1); }
    catch (reason) { setError(explain(reason)); }
    finally { setBusyId(null); }
  };

  return <section className="panel stack watcher-panel" aria-labelledby="watchers-title">
    <div className="notification-heading"><h2 id="watchers-title">Seguimientos de fuentes</h2>
      {!form && <button type="button" disabled={loading || sources.length === 0} onClick={() => { createAttempt.current = null; setForm("new"); }}>Añadir seguimiento</button>}
    </div>
    <p>Elige qué modelos observar en cada fuente. Los filtros de tamaño solo incluyen resultados con tamaño exacto conocido.</p>
    {form && <WatchForm key={form === "new" ? "new" : `${form.id}-${form.row_version}`} sources={sources} initial={form === "new" ? undefined : form} busy={busyId !== null} onSave={save} onCancel={() => setForm(null)} />}
    {loading && <p role="status">Cargando fuentes y seguimientos…</p>}
    {!loading && !sources.length && !error && <p>No hay fuentes configuradas para observar.</p>}
    {!loading && watches.length === 0 && !form && !error && <p>Todavía no hay seguimientos.</p>}
    {watches.map((watch) => {
      const source = sources.find((item) => item.id === watch.source_id);
      return <article className="device watcher-card" key={watch.id}>
        <div className="notification-heading"><h3>{source?.name ?? "Fuente no disponible"}</h3><span className="detail">{watch.enabled ? "Activo" : "Pausado"}</span></div>
        <p>{querySummary(watch.query)}</p>
        <p className="detail">Cada {watch.interval_seconds / 3600} h · siguiente revisión {watch.next_run_at ? new Date(watch.next_run_at).toLocaleString("es-ES") : "sin programar"}</p>
        {deleting === watch.id ? <div className="actions"><span>Se eliminará este seguimiento, no los modelos de la fuente.</span><button type="button" className="secondary" onClick={() => setDeleting(null)}>Conservar</button><button type="button" disabled={busyId !== null} onClick={() => void remove(watch)}>{busyId === watch.id ? "Eliminando…" : "Eliminar seguimiento"}</button></div> : <div className="actions"><button type="button" className="secondary" disabled={busyId !== null} onClick={() => setForm(watch)}>Editar</button><button type="button" className="secondary" disabled={busyId !== null} onClick={() => setDeleting(watch.id)}>Eliminar</button></div>}
      </article>;
    })}
    {error && <p role="alert" className="state-unknown">{error}</p>}
  </section>;
}

export function NotificationsPage({ client, onNavigate }: { client: ApiClient; onNavigate: (screen: string, resourceId?: string) => void }) {
  const [items, setItems] = useState<NotificationItem[]>([]);
  const [filter, setFilter] = useState<"ALL" | "UNREAD">("ALL");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    let active = true;
    setLoading(true); setError(null);
    void listNotifications(client).then((result) => { if (active) setItems(dedupeNotifications(result)); })
      .catch((reason: unknown) => { if (active) setError(explain(reason)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [client, revision]);

  const mark = async (item: NotificationItem, read: boolean) => {
    setBusyId(item.id); setError(null);
    try {
      const updated = await setNotificationRead(client, item, read);
      setItems((current) => dedupeNotifications(current.map((row) => row.id === updated.id ? updated : row)));
    } catch (reason) { setError(explain(reason)); }
    finally { setBusyId(null); }
  };
  const visible = items.filter((item) => filter === "ALL" || !item.read_at);
  const unreadCount = items.filter((item) => !item.read_at).length;
  return <div className="stack notification-page">
    <section className="panel stack" aria-labelledby="notifications-title">
      <div className="notification-heading"><h2 id="notifications-title">Centro de notificaciones</h2><button type="button" className="secondary" disabled={loading} onClick={() => setRevision((value) => value + 1)}>Actualizar</button></div>
      <p>Los avisos se guardan localmente. Los candidatos se muestran aquí; LARM no inicia descargas ni actualizaciones sin una confirmación y un plan disponible.</p>
      <div className="actions" role="group" aria-label="Filtrar notificaciones">
        <button type="button" className={filter === "ALL" ? "" : "secondary"} onClick={() => setFilter("ALL")}>Todas ({items.length})</button>
        <button type="button" className={filter === "UNREAD" ? "" : "secondary"} onClick={() => setFilter("UNREAD")}>Sin leer ({unreadCount})</button>
      </div>
      {loading && <p role="status">Cargando avisos…</p>}
      {!loading && visible.length === 0 && !error && <p>{filter === "UNREAD" ? "No tienes avisos sin leer." : "Todavía no hay avisos."}</p>}
      {visible.map((item) => <NotificationCard key={item.id} item={item} busy={busyId === item.id} onRead={(entry, read) => void mark(entry, read)} onNavigate={(target) => onNavigate(target.screen, target.resourceId)} />)}
      {error && <p role="alert" className="state-unknown">{error}</p>}
    </section>
    <WatchersPanel client={client} />
  </div>;
}
