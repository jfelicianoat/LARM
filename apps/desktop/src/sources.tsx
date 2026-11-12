import { useCallback, useEffect, useId, useRef, useState, type FormEvent } from "react";
import { ApiClient, ApiError } from "./api/client";
import { useToasts } from "./components";

const AUTH = {
  NONE: "Sin autenticación", TOKEN: "Token", BEARER: "Bearer", API_TOKEN: "Token de API",
  OAUTH_DEVICE: "OAuth con dispositivo", OAUTH_AUTH_CODE_PKCE: "OAuth con navegador",
  BASIC: "Usuario y contraseña", SSH: "SSH", COOKIE: "Cookie", GIT_LFS: "Git LFS",
  WINDOWS_INTEGRATED: "Cuenta de Windows",
} as const;
type AuthMode = keyof typeof AUTH;
type OperationalStatus = "UNKNOWN" | "SYNCING" | "HEALTHY" | "RATE_LIMITED" | "AUTH_REQUIRED" | "ERROR";
interface RateLimitObservation {
  limit: string | null; remaining: string | null; reset_at: string | null; retry_after_seconds: string | null;
}
interface SourceOperationalSnapshot {
  status: OperationalStatus; last_sync_at: string | null; rate_limit: RateLimitObservation | null;
}
const OPERATIONAL: Record<OperationalStatus, string> = {
  UNKNOWN: "Desconocido", SYNCING: "Sincronizando", HEALTHY: "Operativa",
  RATE_LIMITED: "Limitada por el proveedor", AUTH_REQUIRED: "Autenticación requerida", ERROR: "Error de sincronización",
};
export interface SourceConfig {
  name: string; adapter_id: string; base_url: string; priority: number; enabled: boolean;
  auth_mode: AuthMode; account_id: string | null; policy: Record<string, unknown>;
}
export interface ModelSource extends SourceConfig {
  id: string; row_version: number; created_at: string; updated_at: string; metadata: Record<string, unknown>;
  operational: SourceOperationalSnapshot;
}
interface Page<T> { items: T[]; next_cursor: string | null; snapshot_at: string }
interface SourceAccount {
  id: string; source_id: string; credential_reference_id: string | null; display_name: string;
  accepted_license_hashes: string[]; row_version: number;
}
interface Installation {
  id: string; adapter_id: string; version: string; family: string; state: string; trust: string;
  manifest: { protocol_version?: unknown };
  metadata: Record<string, unknown>;
}
interface Connector { id: string; label: string; qualified: boolean }

function connections(items: Installation[]): Connector[] {
  return items.flatMap((item) => {
    if (item.family !== "source" || item.manifest?.protocol_version !== "1.0" || !Array.isArray(item.metadata.source_bindings)) return [];
    return item.metadata.source_bindings.flatMap((binding: unknown) => {
      if (!binding || typeof binding !== "object" || !("id" in binding) || !("source_type" in binding)
        || typeof binding.id !== "string" || typeof binding.source_type !== "string") return [];
      return [{ id: binding.id, label: `${binding.source_type} · ${item.adapter_id} ${item.version}`,
        qualified: item.state === "VALIDATED" && item.trust !== "REVOKED" && item.manifest?.protocol_version === "1.0" }];
    });
  });
}
function explain(error: unknown): string {
  if (!(error instanceof ApiError)) return "No se pudo completar la operación. Vuelve a intentarlo.";
  if (error.isOffline) return "Se perdió la conexión con LARM. Puedes reintentar cuando vuelva a estar disponible.";
  const reasons: Record<string, string> = {
    stale_revision: "Esta fuente ha cambiado. Conservamos tu formulario; cancela y actualiza la lista antes de editar la versión nueva.",
    source_in_use: "Esta fuente tiene modelos, cuentas o seguimientos asociados. Resuelve esas referencias antes de eliminarla.",
    source_identity_in_use: "La dirección o el tipo de esta fuente no pueden cambiar mientras tenga referencias asociadas.",
    source_adapter_unqualified: "El conector aún no ha superado las pruebas de conformidad. Guarda la fuente desactivada.",
    source_adapter_unavailable: "El conector ya no está disponible. Actualiza la lista y selecciona otro.",
    source_account_mismatch: "La cuenta seleccionada no pertenece a esta fuente.",
    source_secret_in_config: "No incluyas credenciales en la dirección ni en la configuración.",
    basic_requires_https: "Las credenciales Basic solo se guardan para fuentes HTTPS.",
    source_credential_scope_mismatch: "La credencial vinculada no corresponde a esta cuenta.",
    credential_backend_unavailable: "Credential Manager no está disponible. La credencial no se ha guardado.",
  };
  return (error.reason && reasons[error.reason]) || (error.status === 403
    ? "Tu cuenta no tiene permiso para realizar esta operación."
    : error.status === 400 ? "Revisa la dirección y los campos. No incluyas contraseñas ni tokens."
    : "LARM no pudo guardar los cambios. Puedes volver a intentarlo.");
}
function formatSyncAt(value: string | null): string {
  if (!value) return "Sin sincronizaciones registradas";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "Fecha de sincronización no válida" : date.toLocaleString("es-ES");
}
function formatRateLimit(rate: RateLimitObservation | null): string {
  if (!rate) return "Sin observación de límite";
  if (![rate.limit, rate.remaining, rate.reset_at, rate.retry_after_seconds].some((value) => value !== null)) {
    return "El conector no informó valores";
  }
  const reset = rate.reset_at === null ? "desconocido" : formatSyncAt(rate.reset_at);
  return [
    `Límite: ${rate.limit ?? "desconocido"}`,
    `Disponibles: ${rate.remaining ?? "desconocido"}`,
    `Reinicio: ${reset}`,
    `Espera: ${rate.retry_after_seconds === null ? "desconocida" : `${rate.retry_after_seconds} s`}`,
  ].join(" · ");
}
const blank = (): SourceConfig => ({ name: "", adapter_id: "", base_url: "", priority: 1, enabled: false, auth_mode: "NONE", account_id: null, policy: {} });

function SourceAccess({ source, client, onSourceUpdated, onClose }: {
  source: ModelSource; client: ApiClient; onSourceUpdated: (source: ModelSource) => void; onClose: () => void;
}) {
  const [accounts, setAccounts] = useState<SourceAccount[]>([]);
  const [selectedId, setSelectedId] = useState(source.account_id ?? "");
  const [displayName, setDisplayName] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const id = useId();

  const readAccounts = useCallback(async () => {
    const result: SourceAccount[] = [];
    const seen = new Set<string>();
    let cursor: string | null = null;
    do {
      const params = new URLSearchParams({ limit: "200" });
      if (cursor) params.set("cursor", cursor);
      const page = await client.get<Page<SourceAccount>>(`/api/v1/source-accounts?${params}`);
      result.push(...page.items.filter((account) => account.source_id === source.id));
      cursor = page.next_cursor;
      if (cursor && seen.has(cursor)) throw new Error("repeated cursor");
      if (cursor) seen.add(cursor);
    } while (cursor);
    return result;
  }, [client, source.id]);

  useEffect(() => {
    let active = true;
    setLoading(true); setError(null);
    void readAccounts().then((result) => { if (active) setAccounts(result); })
      .catch((e: unknown) => { if (active) setError(explain(e)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [readAccounts]);

  const linkAccount = async (accountId: string | null) => {
    const updated = await client.request<ModelSource>({ method: "PATCH", path: `/api/v1/sources/${source.id}`,
      body: { name: source.name, adapter_id: source.adapter_id, base_url: source.base_url,
        priority: source.priority, enabled: source.enabled, auth_mode: source.auth_mode,
        account_id: accountId, policy: source.policy }, headers: { "If-Match": String(source.row_version) } });
    onSourceUpdated(updated);
    setSelectedId(accountId ?? "");
    setNotice(accountId ? "Cuenta vinculada a la fuente." : "Cuenta desvinculada de la fuente.");
  };

  const createAccount = async () => {
    const name = displayName.trim();
    if (!name) { setError("Indica un nombre para identificar la cuenta."); return; }
    if (accounts.some((account) => account.display_name.toLocaleLowerCase() === name.toLocaleLowerCase())) {
      setError("Ya existe una cuenta con ese nombre para esta fuente. Selecciónala en la lista."); return;
    }
    setBusy(true); setError(null); setNotice(null);
    try {
      const account = await client.request<SourceAccount>({ method: "POST", path: "/api/v1/source-accounts",
        body: { source_id: source.id, credential_reference_id: null, display_name: name, accepted_license_hashes: [] } });
      setAccounts((current) => [...current, account]); setDisplayName("");
      await linkAccount(account.id);
    } catch (e) { setError(explain(e)); }
    finally { setBusy(false); }
  };

  const saveBasicCredential = async (event: FormEvent) => {
    event.preventDefault();
    if (!selectedId || !source.account_id || selectedId !== source.account_id) {
      setError("Vincula primero una cuenta a esta fuente."); return;
    }
    if (!username.trim() || !password) { setError("Indica el usuario y la contraseña."); return; }
    setBusy(true); setError(null); setNotice(null);
    try {
      const reference = await client.request<{ id: string }>({ method: "POST", path: `/api/v1/sources/${source.id}/basic-credential`,
        body: { source_id: source.id, account_id: selectedId, username: username.trim(), password } });
      setAccounts((current) => current.map((account) => account.id === selectedId
        ? { ...account, credential_reference_id: reference.id } : account));
      setNotice("Credencial guardada en el almacén protegido.");
    } catch (e) { setError(explain(e)); }
    finally { setUsername(""); setPassword(""); setBusy(false); }
  };

  const selectedAccount = accounts.find((account) => account.id === selectedId);
  return <section className="panel stack" aria-label={`Acceso de ${source.name}`}>
    <h3>Cuenta y acceso</h3>
    {loading ? <p role="status">Cargando cuentas…</p> : <>
      <label htmlFor={`${id}-account`}>Cuenta de esta fuente</label>
      <select id={`${id}-account`} value={selectedId} disabled={busy} onChange={(event) => setSelectedId(event.target.value)}>
        <option value="">Selecciona una cuenta</option>
        {accounts.map((account) => <option key={account.id} value={account.id}>{account.display_name}</option>)}
      </select>
      {selectedAccount && <p className="detail">Credencial de esta cuenta: {selectedAccount.credential_reference_id
        ? <span aria-label="credencial oculta">••••••••</span> : "No configurada"}</p>}
      {selectedId && selectedId !== source.account_id && <button type="button" className="secondary" disabled={busy}
        onClick={() => { setBusy(true); setError(null); void linkAccount(selectedId).catch((e: unknown) => setError(explain(e))).finally(() => setBusy(false)); }}>
        {busy ? "Vinculando…" : "Vincular cuenta"}
      </button>}
      {source.account_id && selectedId === source.account_id && <button type="button" className="secondary" disabled={busy}
        onClick={() => { setBusy(true); setError(null); void linkAccount(null).catch((e: unknown) => setError(explain(e))).finally(() => setBusy(false)); }}>
        {busy ? "Desvinculando…" : "Desvincular cuenta"}
      </button>}
      {!accounts.length && <p className="detail">Aún no hay cuentas para esta fuente. Crea una para vincular su acceso.</p>}
      <div className="form" aria-label="Crear cuenta de fuente">
        <label htmlFor={`${id}-display-name`}>Nombre para identificar la cuenta</label>
        <input id={`${id}-display-name`} value={displayName} onChange={(event) => setDisplayName(event.target.value)} disabled={busy} autoComplete="off" />
        <button type="button" className="secondary" disabled={busy || !displayName.trim()} onClick={() => void createAccount()}>
          {busy ? "Guardando…" : "Crear y vincular cuenta"}
        </button>
      </div>
      {source.auth_mode === "BASIC" ?
        <form className="form" onSubmit={(event) => void saveBasicCredential(event)}>
          <h4>Credenciales Basic</h4>
          <p className="detail">{selectedAccount?.credential_reference_id ? "Hay una credencial protegida. Si guardas otra, se actualizará en el almacén protegido de Windows." : "Se guardan en el almacén protegido de Windows. No vuelven a mostrarse después de guardarlas."}</p>
          <label htmlFor={`${id}-username`}>Usuario</label>
          <input id={`${id}-username`} value={username} onChange={(event) => setUsername(event.target.value)} disabled={busy || !source.account_id || selectedId !== source.account_id} autoComplete="off" />
          <label htmlFor={`${id}-password`}>Contraseña</label>
          <input id={`${id}-password`} type="password" value={password} onChange={(event) => setPassword(event.target.value)} disabled={busy || !source.account_id || selectedId !== source.account_id} autoComplete="new-password" />
          <button disabled={busy || !source.account_id || selectedId !== source.account_id || !username.trim() || !password}>
            {busy ? "Guardando…" : "Guardar credencial"}
          </button>
        </form> : <p className="notice">La gestión de credenciales para {AUTH[source.auth_mode]} aún no está disponible.</p>}
    </>}
    {notice && <p role="status" className="detail">{notice}</p>}
    {error && <p role="alert" className="state-unknown">{error}</p>}
    <div className="actions"><button type="button" className="secondary" disabled={busy} onClick={onClose}>Cerrar</button></div>
  </section>;
}

function SourceForm({ initial, connectors, editing, onSave, onCancel }: {
  initial: SourceConfig; connectors: Connector[]; editing: boolean;
  onSave: (value: SourceConfig) => Promise<void>; onCancel: () => void;
}) {
  const [draft, setDraft] = useState(initial);
  const [priority, setPriority] = useState(String(initial.priority));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const nameInput = useRef<HTMLInputElement>(null);
  useEffect(() => { nameInput.current?.focus(); }, []);
  const selected = connectors.find((c) => c.id === draft.adapter_id);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const number = Number(priority);
    if (!draft.name.trim() || !draft.base_url.trim() || !draft.adapter_id || !/^\d+$/.test(priority) || !Number.isSafeInteger(number)) {
      setError("Indica nombre, conector y dirección. La prioridad debe ser un entero mayor o igual a cero.");
      return;
    }
    setBusy(true); setError(null);
    try { await onSave({ ...draft, name: draft.name.trim(), base_url: draft.base_url.trim(), priority: number }); }
    catch (e) { setError(explain(e)); }
    finally { setBusy(false); }
  };
  const field = (key: string) => `${id}-${key}`;
  return <form className="panel form" aria-labelledby={field("title")} onSubmit={(event) => void submit(event)}>
    <h2 id={field("title")}>{editing ? "Editar fuente" : "Añadir fuente"}</h2>
    <label htmlFor={field("name")}>Nombre</label>
    <input ref={nameInput} id={field("name")} value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} disabled={busy} required />
    <label htmlFor={field("connector")}>Conector</label>
    <select id={field("connector")} value={draft.adapter_id} onChange={(e) => setDraft({ ...draft, adapter_id: e.target.value, enabled: false })} disabled={busy} required>
      <option value="">Selecciona un conector</option>
      {draft.adapter_id && !selected && <option value={draft.adapter_id}>Conector asociado no disponible</option>}
      {connectors.map((c) => <option key={c.id} value={c.id}>{c.label}{c.qualified ? "" : " · pendiente de validación"}</option>)}
    </select>
    <label htmlFor={field("address")}>Dirección o carpeta</label>
    <input id={field("address")} value={draft.base_url} onChange={(e) => setDraft({ ...draft, base_url: e.target.value })} disabled={busy} aria-describedby={field("address-help")} required />
    <p id={field("address-help")} className="detail">Usa la dirección del repositorio o una carpeta absoluta, según el conector. No incluyas credenciales.</p>
    <label htmlFor={field("auth")}>Autenticación prevista</label>
    <select id={field("auth")} value={draft.auth_mode} onChange={(e) => setDraft({ ...draft, auth_mode: e.target.value as AuthMode })} disabled={busy} aria-describedby={field("auth-help")}>
      {Object.entries(AUTH).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
    </select>
    <p id={field("auth-help")} className="detail">{draft.account_id ? "Se conservará la cuenta asociada a esta fuente." : "Este formulario guarda la configuración. La vinculación de cuentas todavía no está disponible."}</p>
    <label htmlFor={field("priority")}>Prioridad (mayor = preferida)</label>
    <input id={field("priority")} inputMode="numeric" value={priority} onChange={(e) => setPriority(e.target.value)} disabled={busy} />
    <label className="check"><input type="checkbox" checked={draft.enabled} disabled={busy || (!selected?.qualified && !draft.enabled)} aria-describedby={field("enable-help")} onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })} />Activar fuente</label>
    <p id={field("enable-help")} className="detail">{selected?.qualified ? "La activación no comprueba la conexión ni inicia descargas." : "Puedes guardarla desactivada. El conector debe superar las pruebas de conformidad antes de activarla."}</p>
    {error && <p role="alert" className="state-unknown">{error}</p>}
    <div className="actions"><button disabled={busy} type="submit">{busy ? "Guardando…" : "Guardar fuente"}</button><button disabled={busy} className="secondary" type="button" onClick={onCancel}>Cancelar</button></div>
  </form>;
}

export function SourcesPage({ client, focusResourceId }: { client: ApiClient; focusResourceId?: string }) {
  const [items, setItems] = useState<ModelSource[]>([]);
  const [connectors, setConnectors] = useState<Connector[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [draftQuery, setDraftQuery] = useState("");
  const [query, setQuery] = useState("");
  const [revision, setRevision] = useState(0);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState<ModelSource | "new" | null>(null);
  const [access, setAccess] = useState<ModelSource | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);
  const generation = useRef(0);
  const createAttempt = useRef<{ body: string; key: string } | null>(null);
  const { notify } = useToasts();
  const queryId = useId();
  const listPath = useCallback((next: string | null) => {
    const params = new URLSearchParams({ limit: "200" });
    if (query) params.set("q", query);
    if (next) params.set("cursor", next);
    return `/api/v1/sources?${params}`;
  }, [query]);
  useEffect(() => {
    const active = ++generation.current;
    setLoading(true); setError(null); setItems([]); setCursor(null); setConnectors([]);
    const read = async () => {
      const page = await client.get<Page<ModelSource>>(listPath(null));
      if (active !== generation.current) return;
      setItems(page.items); setCursor(page.next_cursor);
      const all: Installation[] = [];
      const seen = new Set<string>();
      let next: string | null = null;
      do {
        const params = new URLSearchParams({ limit: "200" });
        if (next) params.set("cursor", next);
        const adapters = await client.get<Page<Installation>>(`/api/v1/adapters?${params}`);
        if (active !== generation.current) return;
        all.push(...adapters.items); next = adapters.next_cursor;
        if (next && seen.has(next)) throw new Error("repeated cursor");
        if (next) seen.add(next);
      } while (next);
      setConnectors(connections(all));
    };
    void read().catch((e: unknown) => { if (active === generation.current) setError(explain(e)); })
      .finally(() => { if (active === generation.current) setLoading(false); });
    return () => { generation.current++; };
  }, [client, listPath, revision]);
  useEffect(() => {
    if (!focusResourceId || loading) return;
    const target = document.querySelector<HTMLElement>(`[data-resource-id="${focusResourceId}"]`);
    target?.focus({ preventScroll: true });
    target?.scrollIntoView({ block: "center" });
  }, [focusResourceId, items, loading]);
  const refresh = () => { setDeleting(null); setError(null); setRevision((n) => n + 1); };
  const save = async (config: SourceConfig) => {
    if (form === "new") {
      const body = JSON.stringify(config);
      if (createAttempt.current?.body !== body) createAttempt.current = { body, key: crypto.randomUUID() };
      await client.request<ModelSource>({ method: "POST", path: "/api/v1/sources", body: config, headers: { "Idempotency-Key": createAttempt.current.key } });
      createAttempt.current = null;
    } else if (form) {
      await client.request<ModelSource>({ method: "PATCH", path: `/api/v1/sources/${form.id}`, body: config, headers: { "If-Match": String(form.row_version) } });
    }
    setForm(null); notify("info", "Configuración de la fuente guardada."); refresh();
  };
  const remove = async (source: ModelSource) => {
    setDeleteBusy(true); setError(null);
    try {
      await client.request({ method: "DELETE", path: `/api/v1/sources/${source.id}`, headers: { "If-Match": String(source.row_version) } });
      notify("info", "Configuración de la fuente eliminada."); refresh();
    } catch (e) { setError(explain(e)); }
    finally { setDeleteBusy(false); }
  };
  const more = async () => {
    const active = generation.current;
    setLoading(true); setError(null);
    try {
      const page = await client.get<Page<ModelSource>>(listPath(cursor));
      if (active === generation.current) { setItems((old) => [...old, ...page.items]); setCursor(page.next_cursor); }
    } catch (e) { if (active === generation.current) setError(explain(e)); }
    finally { if (active === generation.current) setLoading(false); }
  };
  return <div className="stack source-page">
    <p>Configura los repositorios y carpetas desde los que LARM podrá obtener modelos. Guardar una fuente no inicia ninguna descarga.</p>
    <p>El estado operativo procede de los seguimientos programados; si aún no hay una sincronización registrada, se muestra como desconocido.</p>
    {form ? <SourceForm key={form === "new" ? "new" : form.id} initial={form === "new" ? blank() : { name: form.name, adapter_id: form.adapter_id, base_url: form.base_url, priority: form.priority, enabled: form.enabled, auth_mode: form.auth_mode, account_id: form.account_id, policy: form.policy }} connectors={connectors} editing={form !== "new"} onSave={save} onCancel={() => { setForm(null); refresh(); }} /> : <>
      <div className="actions"><button type="button" disabled={loading || !connectors.length} onClick={() => { createAttempt.current = null; setForm("new"); }}>Añadir fuente</button><button type="button" className="secondary" disabled={loading || deleteBusy} onClick={refresh}>Actualizar fuentes</button></div>
      {!loading && !connectors.length && !error && <p className="notice">No hay conectores de fuentes compatibles y validados. Revisa Adaptadores: se requiere el protocolo 1.0 y superar las pruebas de conformidad.</p>}
      <form className="form source-filter" onSubmit={(e) => { e.preventDefault(); setQuery(draftQuery.trim()); setRevision((n) => n + 1); }}>
        <label htmlFor={queryId}>Buscar por nombre o dirección</label>
        <div className="actions"><input id={queryId} type="search" value={draftQuery} onChange={(e) => setDraftQuery(e.target.value)} /><button className="secondary" disabled={loading} type="submit">Buscar fuentes</button></div>
      </form>
      {!loading && !items.length && !error && <p>{query ? "No hay fuentes que coincidan con la búsqueda." : "Todavía no hay ninguna fuente configurada."}</p>}
      {items.map((source) => <article key={source.id} tabIndex={-1} data-resource-id={source.id} className="device" aria-label={`Fuente ${source.name}`}>
        <h2>{source.name}</h2>
        <dl className="facts"><dt>Configuración</dt><dd>{source.enabled ? "Activada" : "Desactivada"}</dd><dt>Estado operativo</dt><dd>{OPERATIONAL[source.operational.status]}</dd><dt>Dirección</dt><dd className="source-address">{source.base_url}</dd><dt>Conector</dt><dd>{connectors.find((c) => c.id === source.adapter_id)?.label ?? "Conector asociado no disponible"}</dd><dt>Autenticación</dt><dd>{AUTH[source.auth_mode]}{source.auth_mode !== "NONE" && !source.account_id ? " · cuenta pendiente" : ""}</dd><dt>Límite de solicitudes</dt><dd>{formatRateLimit(source.operational.rate_limit)}</dd><dt>Última sincronización</dt><dd>{formatSyncAt(source.operational.last_sync_at)}</dd><dt>Prioridad</dt><dd>{source.priority}</dd></dl>
        {access?.id === source.id && <SourceAccess source={access} client={client} onSourceUpdated={(updated) => {
          setAccess(updated); setItems((current) => current.map((item) => item.id === updated.id ? updated : item));
        }} onClose={() => setAccess(null)} />}
        {deleting === source.id ? <div className="stack" aria-label={`Eliminar ${source.name}`}><p>Se eliminará la configuración de «{source.name}». Los modelos y archivos descargados se conservan. Si tiene referencias asociadas, LARM rechazará la eliminación.</p><div className="actions"><button className="secondary" type="button" disabled={deleteBusy} onClick={() => setDeleting(null)}>Conservar fuente</button><button type="button" disabled={deleteBusy} onClick={() => void remove(source)}>{deleteBusy ? "Eliminando…" : "Eliminar configuración"}</button></div></div> : <div className="actions"><button type="button" className="secondary" disabled={deleteBusy || loading} onClick={() => { setDeleting(null); setForm(source); }}>Editar</button><button type="button" className="secondary" disabled={deleteBusy || loading} onClick={() => setDeleting(source.id)}>Eliminar</button></div>}
        {source.auth_mode !== "NONE" && access?.id !== source.id && <button type="button" className="secondary" disabled={deleteBusy || loading}
          onClick={() => { setDeleting(null); setAccess(source); }}>Gestionar acceso</button>}
      </article>)}
      {cursor && <button type="button" className="secondary" disabled={loading} onClick={() => void more()}>Cargar más fuentes</button>}
    </>}
    {loading && <p role="status">Cargando fuentes y conectores…</p>}
    {error && <p role="alert" className="state-unknown">{error}</p>}
  </div>;
}
