/**
 * THESIS: inventario y política separados; un canal excluido sigue visible.
 * OWN-WORLD: extensión del shell Segoe UI, panel claro/oscuro y foco amarillo.
 * STORY: elegir runtime, consultar versiones y cambiar canales de descubrimiento.
 * FIRST VIEWPORT: búsqueda y alta arriba, lista de runtimes y detalle seleccionado.
 * FORM: filas de inventario y editor inline; conserva los componentes existentes.
 */
import { useCallback, useEffect, useId, useRef, useState, type FormEvent } from "react";
import { ApiClient, ApiError } from "./api/client";
import { cancelRuntimeInstallJob, CHANNELS, CHANNEL_LABEL, INSTALLATION_STRATEGIES, allPages, runtimeConnections, type AdapterInstallation, type ChannelPolicy, type Connector, type ExecutionHost, type ExternalHost, type InstallationStrategy, type JobReceipt, type Page, type Runtime, type RuntimeBackend, type RuntimeChannel, type RuntimeCreate, type RuntimeInstallation, type RuntimeInstallJobState, type RuntimeInstallRequest, type RuntimeProfileLock, type RuntimeRetentionPolicy, type RuntimeVersion } from "./api/runtimes";
import { useToasts } from "./components";
import type { StorageLocation } from "./storage";
import { LaunchProfiles } from "./launchProfiles";
import { DeclarativeRuntimeWizard, RuntimeWizard } from "./runtime-wizard";
import { RuntimePinManager } from "./runtime-pins";

function explain(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 409) return "Este runtime ha cambiado. Conservamos tu selección; descarta el borrador y recarga para editar la revisión actual.";
    if (error.status === 400) return "El documento no cumple el contrato de LARM. Revisa los campos y el hash antes de reintentar.";
    if (error.status === 403) return "Tu cuenta no tiene permiso para guardar este cambio.";
    if (error.status === 404) return "No se encontró un lock de perfil con ese hash.";
    if (error.isOffline) return "Se perdió la conexión con LARM. Reintenta cuando vuelva a estar disponible.";
  }
  return "No se pudo completar la operación. Puedes volver a intentarlo.";
}
function explainInstall(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.reason) {
      case "runtime_profile_strategy_unqualified":
      case "runtime_profile_qualification_fixture_mismatch":
        return "Este perfil no tiene evidencia válida para la estrategia elegida. No se ha encolado la instalación.";
      case "runtime_install_effects_unqualified":
      case "runtime_custom_install_effects_unqualified":
        return "Esta estrategia no tiene sus efectos y permisos cualificados. No se ha encolado la instalación.";
      case "runtime_install_strategy_not_in_profile":
      case "runtime_install_strategy_profile_mismatch":
        return "La estrategia no coincide con las permitidas por el perfil seleccionado.";
      case "runtime_install_profile_lock_missing":
        return "No se encontró el lock de perfil. Comprueba su SHA-256 y vuelve a consultarlo.";
      case "runtime_external_adoption_admission_unavailable":
        return "La adopción externa todavía no está disponible para instalaciones durables.";
    }
    if (error.status === 403) return "Tu cuenta no tiene permiso para solicitar instalaciones.";
    if (error.status === 409) return "LARM rechazó la admisión porque el runtime, el plan o sus recursos cambiaron.";
  }
  return explain(error);
}
function ChannelFields({ value, onChange, disabled }: { value: RuntimeChannel[]; onChange: (channels: RuntimeChannel[]) => void; disabled: boolean }) {
  return <fieldset className="runtime-channels" disabled={disabled}>
    <legend>Canales permitidos</legend>
    {CHANNELS.map((channel) => <label className="check" key={channel}><input type="checkbox" checked={value.includes(channel)} onChange={(e) => onChange(CHANNELS.filter((c) => c === channel ? e.target.checked : value.includes(c)))} />{CHANNEL_LABEL[channel]} <span className="detail">({channel})</span></label>)}
    <p className="detail">Selecciona al menos uno. Esta política se aplica al descubrimiento y las actualizaciones automáticas; las versiones registradas seguirán visibles.</p>
  </fieldset>;
}

function RuntimeForm({ connectors, onSave, onCancel }: { connectors: Connector[]; onSave: (request: RuntimeCreate) => Promise<void>; onCancel: () => void }) {
  const [name, setName] = useState("");
  const [binding, setBinding] = useState("");
  const [channels, setChannels] = useState<RuntimeChannel[]>(["stable"]);
  const [busy, setBusy] = useState(false), [error, setError] = useState<string | null>(null);
  const pending = useRef(false), input = useRef<HTMLInputElement>(null), id = useId();
  useEffect(() => { input.current?.focus(); }, []);
  const connector = connectors.find((c) => c.id === binding);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (pending.current) return;
    if (!name.trim() || !connector || !channels.length) { setError("Indica un nombre, un conector y al menos un canal."); return; }
    pending.current = true; setBusy(true); setError(null);
    try { await onSave({ name: name.trim(), adapter_id: connector.id, capabilities: connector.capabilities, default_version_id: null, channel_policy: { allowed_channels: channels }, retention_policy: { keep_last_validated: 3 } }); }
    catch (e) { setError(explain(e)); }
    finally { pending.current = false; setBusy(false); }
  };
  return <form className="panel form" aria-labelledby={`${id}-title`} onSubmit={(event) => void submit(event)}>
    <h2 id={`${id}-title`}>Registrar runtime</h2>
    <p>Guarda su nombre, conector y política. El registro no instala ni inicia un motor.</p>
    <label htmlFor={`${id}-name`}>Nombre del runtime</label>
    <input id={`${id}-name`} ref={input} value={name} disabled={busy} onChange={(e) => setName(e.target.value)} required />
    <label htmlFor={`${id}-connector`}>Conector de runtime</label>
    <select id={`${id}-connector`} value={binding} disabled={busy} onChange={(e) => setBinding(e.target.value)} required>
      <option value="">Selecciona un conector registrado</option>
      {connectors.map((c) => <option key={c.id} value={c.id}>{c.label}</option>)}
    </select>
    {connector && <p className="detail">Capacidades declaradas: {connector.capabilities.join(", ") || "Ninguna"}.</p>}
    <ChannelFields value={channels} onChange={setChannels} disabled={busy} />
    {error && <p role="alert" className="state-unknown">{error}</p>}
    <div className="actions"><button type="submit" disabled={busy}>{busy ? "Guardando…" : "Guardar runtime"}</button><button type="button" className="secondary" disabled={busy} onClick={onCancel}>Cancelar</button></div>
  </form>;
}

function PolicyEditor({ runtime, client, onSaved, onCancel }: { runtime: Runtime; client: ApiClient; onSaved: (runtime: Runtime) => void; onCancel: () => void }) {
  const [base] = useState(runtime);
  const [channels, setChannels] = useState(runtime.channel_policy.allowed_channels);
  const [busy, setBusy] = useState(false), [error, setError] = useState<string | null>(null), [stale, setStale] = useState(false);
  const pending = useRef(false), heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { heading.current?.focus(); }, []);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (pending.current) return;
    if (!channels.length) { setError("Selecciona al menos un canal."); return; }
    pending.current = true; setBusy(true); setError(null); setStale(false);
    try { onSaved(await client.request<Runtime>({ method: "PATCH", path: `/api/v1/runtimes/${base.id}/channel-policy`, body: { allowed_channels: channels } satisfies ChannelPolicy, headers: { "If-Match": String(base.row_version) } })); }
    catch (e) { setError(explain(e)); setStale(e instanceof ApiError && e.status === 409); }
    finally { pending.current = false; setBusy(false); }
  };
  const reload = async () => {
    if (pending.current) return;
    pending.current = true; setBusy(true);
    try { onSaved(await client.get<Runtime>(`/api/v1/runtimes/${runtime.id}`)); }
    catch (e) { setError(explain(e)); }
    finally { pending.current = false; setBusy(false); }
  };
  return <form className="form runtime-policy" onSubmit={(event) => void submit(event)}>
    <h3 ref={heading} tabIndex={-1}>Editar política de {runtime.name}</h3>
    <ChannelFields value={channels} onChange={setChannels} disabled={busy} />
    {runtime.row_version !== base.row_version && <p role="status">Los datos registrados han cambiado. Tu borrador conserva la revisión con la que empezaste a editar.</p>}
    <p className="detail">Las versiones fijadas, la versión predeterminada y las sesiones existentes se conservan.</p>
    {error && <p role="alert" className="state-unknown">{error}</p>}
    <div className="actions"><button type="submit" disabled={busy}>{busy ? "Guardando…" : "Guardar política"}</button><button type="button" className="secondary" disabled={busy} onClick={onCancel}>Cancelar edición</button>{stale && <button type="button" className="secondary" disabled={busy} onClick={() => void reload()}>Descartar y recargar</button>}</div>
  </form>;
}

function RetentionPolicyEditor({ runtime, client, onSaved }: { runtime: Runtime; client: ApiClient; onSaved: (runtime: Runtime) => void }) {
  const [keep, setKeep] = useState(String(runtime.retention_policy.keep_last_validated));
  const [busy, setBusy] = useState(false), [error, setError] = useState<string | null>(null), [stale, setStale] = useState(false);
  const pending = useRef(false), attempt = useRef<{ body: string; key: string } | null>(null);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (pending.current) return;
    const count = Number(keep);
    if (!Number.isInteger(count) || count < 2 || count > 50) { setError("Elige un número entero entre 2 y 50."); return; }
    const policy: RuntimeRetentionPolicy = { keep_last_validated: count };
    const body = JSON.stringify(policy);
    if (attempt.current?.body !== body) attempt.current = { body, key: crypto.randomUUID() };
    pending.current = true; setBusy(true); setError(null); setStale(false);
    try {
      onSaved(await client.request<Runtime>({
        method: "PATCH", path: `/api/v1/runtimes/${runtime.id}/retention-policy`, body: policy,
        headers: { "If-Match": String(runtime.row_version), "Idempotency-Key": attempt.current.key },
      }));
      attempt.current = null;
    } catch (cause) { setError(explain(cause)); setStale(cause instanceof ApiError && cause.status === 409); }
    finally { pending.current = false; setBusy(false); }
  };
  const reload = async () => {
    if (pending.current) return;
    pending.current = true; setBusy(true);
    try {
      const current = await client.get<Runtime>(`/api/v1/runtimes/${runtime.id}`);
      setKeep(String(current.retention_policy.keep_last_validated)); attempt.current = null; onSaved(current); setStale(false);
    }
    catch (cause) { setError(explain(cause)); }
    finally { pending.current = false; setBusy(false); }
  };
  return <form className="form runtime-retention" onSubmit={(event) => void submit(event)}>
    <label htmlFor={`retention-${runtime.id}`}>Versiones validadas que conservar</label>
    <div className="actions"><input id={`retention-${runtime.id}`} type="number" min={2} max={50} step={1} value={keep} disabled={busy} onChange={(event) => setKeep(event.target.value)} />
      <button type="submit" className="secondary" disabled={busy}>{busy ? "Guardando…" : "Guardar retención"}</button></div>
    <p className="detail">La política se guarda en el catálogo. La limpieza automática aún requiere la integración del worker de retención; guardar el valor no borra versiones.</p>
    {error && <p role="alert" className="state-unknown">{error}</p>}
    {stale && <button type="button" className="secondary" disabled={busy} onClick={() => void reload()}>Descartar y recargar retención</button>}
  </form>;
}

const PROMOTION = { NONE: "Sin promoción", CANDIDATE: "Candidata", DEFAULT: "Predeterminada", PINNED: "Fijada", REJECTED: "Rechazada" };
const VERSION_LIFECYCLE: Record<RuntimeVersion["lifecycle"], string> = {
  CANDIDATE: "Candidata", INSTALLING: "Instalando", VALIDATING: "Validando", VALIDATED: "Validada",
  DEFAULT: "Predeterminada", RETAINED: "Conservada", BROKEN: "No operativa", REJECTED: "Rechazada",
};
const INVENTORY_LABEL: Record<string, string> = {
  DISCOVERED: "Detectada", INSTALL_PLANNED: "Instalación planificada", INSTALLING: "Instalando", INSTALLED: "Instalada",
  VALIDATING: "Validando", VALIDATED: "Validada", ACTIVE: "Activa", DEGRADED: "Degradada", BROKEN: "No operativa",
  ROLLED_BACK: "Revertida", FAILED: "Fallida", RETIRED: "Retirada", MANAGED: "Gestionado", ADOPTED: "Adoptado",
  EXTERNAL: "Externo", LARM_MANAGED: "Gestionado por LARM", ONLINE: "Conectado", OFFLINE: "Desconectado",
  PREBUILT_BINARY: "Binario precompilado", GITHUB_RELEASE: "Publicación de GitHub", SOURCE_BUILD: "Compilación desde código",
  WINGET: "WinGet", PIP: "pip", UV: "uv", CONDA: "Conda", MAMBA: "Mamba", NUGET: "NuGet", WSL: "WSL",
  CONTAINER: "Contenedor", CUSTOM: "Personalizada", MSI_MANAGED: "Instalador MSI", PYTHON_VENV: "Entorno virtual Python",
  EXTERNAL_ADOPTION: "Adopción externa",
};
const inventoryLabel = (value: string) => INVENTORY_LABEL[value] ?? `Sin clasificar (${value})`;
function VersionRow({ version, isDefault, onUpdate, onRollback }: { version: RuntimeVersion; isDefault: boolean; onUpdate: () => void; onRollback: () => void }) {
  const original = version.metadata.upstream_channel;
  const upstream = original && typeof original === "object" && "value" in original && typeof original.value === "string" && "reason" in original && original.reason === "UNKNOWN"
    ? original.value
    : version.channel === "custom" && version.metadata.channel_reason === "UNKNOWN" ? "" : null;
  return <li className="runtime-version">
    <div><strong className="runtime-version-name">{version.version_string}</strong><p className="detail">{PROMOTION[version.promotion] || version.promotion} · {VERSION_LIFECYCLE[version.lifecycle]}</p>
      {version.lifecycle === "CANDIDATE" && <button type="button" className="secondary" onClick={onUpdate}>Actualizar a esta versión</button>}
      {!isDefault && (version.lifecycle === "VALIDATED" || version.lifecycle === "RETAINED") && <button type="button" className="secondary" onClick={onRollback}>Preparar rollback a esta versión</button>}
    </div>
    <div><span>{CHANNEL_LABEL[version.channel] || "Canal no reconocido"} <span className="detail">({version.channel})</span></span>
      {upstream !== null && <p className="state-unknown">Canal original sin clasificar: {upstream || "Sin valor"}</p>}
      <details><summary>Identidad y publicación</summary><dl className="facts">
        <dt>Commit</dt><dd className="mono">{version.commit ?? "No informado"}</dd>
        <dt>Build</dt><dd>{version.build ?? "No informado"}</dd>
        <dt>Publicación</dt><dd>{version.released_at ?? "No informada"}</dd>
        <dt>SHA-256</dt><dd className="mono">{version.artifact_hash ?? "No informado"}</dd>
      </dl></details>
    </div>
  </li>;
}

function validProfileLock(value: unknown): value is RuntimeProfileLock {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const candidate = value as Record<string, unknown>;
  const key = candidate.key;
  const fields = candidate.fields;
  const keyFields = ["runtime", "source_url", "source_revision", "source_sha256", "upstream_version", "os", "os_build", "backend", "protocol"];
  return candidate.schema_version === "larm.runtime-profile-lock/1"
    && candidate.qualification === "UNQUALIFIED"
    && typeof candidate.lock_sha256 === "string"
    && /^[0-9a-f]{64}$/.test(candidate.lock_sha256)
    && !!key && typeof key === "object" && !Array.isArray(key)
    && keyFields.every((field) => typeof (key as Record<string, unknown>)[field] === "string")
    && !!fields && typeof fields === "object" && !Array.isArray(fields);
}

function RuntimeProfileLocks({ client }: { client: ApiClient }) {
  const [document, setDocument] = useState("");
  const [hash, setHash] = useState("");
  const [lock, setLock] = useState<RuntimeProfileLock | null>(null);
  const [busyAction, setBusyAction] = useState<"acquire" | "lookup" | null>(null), [error, setError] = useState<string | null>(null);
  const pending = useRef(false), documentId = useId(), hashId = useId();

  const run = async (mode: "acquire" | "lookup", action: () => Promise<RuntimeProfileLock>) => {
    if (pending.current) return;
    pending.current = true; setBusyAction(mode); setError(null);
    try {
      const received = await action();
      if (!validProfileLock(received)) throw new Error("profile_lock_response_invalid");
      setLock(received); setHash(received.lock_sha256); setDocument(JSON.stringify(received, null, 2));
    } catch (e) {
      setError(e instanceof SyntaxError
        ? "El documento no es JSON válido. Revisa comas, comillas y llaves."
        : e instanceof Error && e.message === "profile_lock_shape_invalid"
          ? "El documento debe ser un lock P099 íntegro y conservar el estado UNQUALIFIED."
          : e instanceof Error && e.message === "profile_lock_response_invalid"
            ? "La respuesta del daemon no cumple el contrato del lock. Actualiza LARM e inténtalo de nuevo."
        : e instanceof ApiError && e.status === 403
          ? "Tu cuenta no tiene permiso para adquirir locks de perfil."
          : explain(e));
    } finally { pending.current = false; setBusyAction(null); }
  };

  const acquire = (event: FormEvent) => {
    event.preventDefault();
    void run("acquire", async () => {
      const parsed: unknown = JSON.parse(document);
      if (!validProfileLock(parsed)) throw new Error("profile_lock_shape_invalid");
      const saved = await client.request<RuntimeProfileLock>({
        method: "PUT",
        path: `/api/v1/runtime-profile-locks/${parsed.lock_sha256}`,
        body: parsed,
      });
      return saved;
    });
  };

  const lookup = (event: FormEvent) => {
    event.preventDefault();
    const requested = hash.trim();
    if (!/^[0-9a-f]{64}$/.test(requested)) {
      setError("Escribe un SHA-256 de 64 caracteres hexadecimales en minúsculas.");
      return;
    }
    void run("lookup", () => client.get<RuntimeProfileLock>(`/api/v1/runtime-profile-locks/${requested}`));
  };

  return <section className="panel runtime-profile-locks" aria-labelledby="profile-locks-title">
    <h2 id="profile-locks-title">Perfiles externos</h2>
    <p>Pega el lock JSON generado por la adquisición P099 o consulta uno guardado mediante su SHA-256.</p>
    <form className="form" onSubmit={(event) => void acquire(event)}>
      <label htmlFor={documentId}>Documento del lock (JSON)</label>
      <textarea id={documentId} className="runtime-lock-json mono" rows={8} value={document} disabled={busyAction !== null} onChange={(event) => setDocument(event.target.value)} spellCheck={false} />
      <div className="actions"><button type="submit" disabled={busyAction !== null || !document.trim()}>{busyAction === "acquire" ? "Guardando…" : "Adquirir lock"}</button></div>
    </form>
    <form className="form runtime-lock-lookup" onSubmit={(event) => void lookup(event)}>
      <label htmlFor={hashId}>Consultar lock por SHA-256</label>
      <div className="actions"><input id={hashId} value={hash} maxLength={64} disabled={busyAction !== null} onChange={(event) => setHash(event.target.value)} autoComplete="off" />
        <button type="submit" className="secondary" disabled={busyAction !== null || !hash.trim()}>{busyAction === "lookup" ? "Consultando…" : "Consultar lock"}</button></div>
    </form>
    {error && <p role="alert" className="state-unknown">{error}</p>}
    {lock && <div className="runtime-profile-lock-result" role="status">
      <p className="state-unknown"><strong>Sin cualificación (UNQUALIFIED).</strong> Este registro no acredita compatibilidad ni permite instalar el runtime.</p>
      <dl className="facts"><dt>Runtime</dt><dd>{lock.key.runtime}</dd>
        <dt>Versión upstream</dt><dd>{lock.key.upstream_version}</dd>
        <dt>Sistema y build</dt><dd>{lock.key.os} · {lock.key.os_build}</dd>
        <dt>Backend / protocolo</dt><dd>{lock.key.backend} · {lock.key.protocol}</dd>
        <dt>SHA-256</dt><dd className="mono">{lock.lock_sha256}</dd></dl>
    </div>}
  </section>;
}

const STRATEGY_LABEL: Record<InstallationStrategy, string> = {
  PREBUILT_BINARY: "Binario precompilado", GITHUB_RELEASE: "Publicación de GitHub", SOURCE_BUILD: "Compilar desde código",
  WINGET: "WinGet", PIP: "pip", UV: "uv", CONDA: "Conda", MAMBA: "Mamba", NUGET: "NuGet", WSL: "WSL",
  CONTAINER: "Contenedor", CUSTOM: "Personalizada", MSI_MANAGED: "Instalador MSI", PYTHON_VENV: "Entorno virtual Python",
  EXTERNAL_ADOPTION: "Adopción externa",
};
const INSTALLABLE_STRATEGIES = INSTALLATION_STRATEGIES.filter((strategy) =>
  !["WINGET", "CUSTOM", "MSI_MANAGED", "EXTERNAL_ADOPTION"].includes(strategy),
);
const JOB_LABEL: Record<JobReceipt["state"], string> = {
  QUEUED: "En cola", RUNNING: "Instalando", COMPLETED: "Instalación completada", FAILED: "Instalación fallida", CANCELLED: "Instalación cancelada",
};

function lockStrategies(lock: RuntimeProfileLock): InstallationStrategy[] {
  const declared = lock.fields.installation_strategies;
  if (!Array.isArray(declared)) return [];
  const fixed = lock.fields.install_strategy;
  return INSTALLABLE_STRATEGIES.filter((strategy) => declared.includes(strategy) && (typeof fixed !== "string" || fixed === strategy));
}

function RuntimeInstallForm({ runtime, versions, locations, client, onClose, onSubmitted, updateVersionId }: {
  runtime: Runtime; versions: RuntimeVersion[]; locations: StorageLocation[]; client: ApiClient;
  onClose: () => void; onSubmitted: () => void; updateVersionId?: string;
}) {
  const defaultVersion = versions.find((version) => version.id === runtime.default_version_id) ?? versions[0];
  const [versionId, setVersionId] = useState(updateVersionId ?? defaultVersion?.id ?? "");
  const [locationId, setLocationId] = useState("");
  const [profileHash, setProfileHash] = useState("");
  const [profile, setProfile] = useState<RuntimeProfileLock | null>(null);
  const [strategy, setStrategy] = useState<InstallationStrategy | "">("");
  const [busy, setBusy] = useState(false), [checking, setChecking] = useState(false), [refreshing, setRefreshing] = useState(false), [cancelling, setCancelling] = useState(false);
  const [error, setError] = useState<string | null>(null), [receipt, setReceipt] = useState<JobReceipt | null>(null);
  const [job, setJob] = useState<RuntimeInstallJobState | null>(null);
  const [cancelRequested, setCancelRequested] = useState(false);
  const [confirmedUpdate, setConfirmedUpdate] = useState(false);
  const pending = useRef(false), attempt = useRef<{ body: string; key: string } | null>(null), hashId = useId();
  const currentVersion = versions.find((version) => version.id === versionId);
  const availableLocations = locations.filter((location) => location.state === "ONLINE");
  const strategies = profile ? lockStrategies(profile) : [];

  const checkProfile = async () => {
    if (pending.current) return;
    const requested = profileHash.trim();
    if (!/^[0-9a-f]{64}$/.test(requested)) { setError("Escribe un SHA-256 de 64 caracteres hexadecimales en minúsculas."); return; }
    if (!currentVersion) { setError("Selecciona una versión registrada antes de validar el perfil."); return; }
    pending.current = true; setChecking(true); setError(null); setProfile(null); setStrategy("");
    try {
      const result: unknown = await client.get(`/api/v1/runtime-profile-locks/${requested}`);
      if (!validProfileLock(result)) throw new Error("runtime_install_profile_response_invalid");
      if (result.lock_sha256 !== requested) throw new Error("runtime_install_profile_hash_mismatch");
      if (result.key.upstream_version !== currentVersion.version_string) throw new Error("runtime_install_profile_version_mismatch");
      const supported = lockStrategies(result);
      if (!supported.length) throw new Error("runtime_install_profile_strategy_missing");
      setProfile(result); setStrategy(supported[0]);
    } catch (cause) {
      if (cause instanceof Error && cause.message === "runtime_install_profile_response_invalid") setError("La respuesta del daemon no cumple el contrato del lock P099.");
      else if (cause instanceof Error && cause.message === "runtime_install_profile_hash_mismatch") setError("El daemon devolvió un lock con un SHA-256 distinto al solicitado.");
      else if (cause instanceof Error && cause.message === "runtime_install_profile_version_mismatch") setError(`El lock corresponde a ${profileHash}, pero la versión seleccionada es ${currentVersion.version_string}.`);
      else if (cause instanceof Error && cause.message === "runtime_install_profile_strategy_missing") setError("El perfil no anuncia una estrategia de instalación que LARM pueda ejecutar.");
      else setError(explain(cause));
    } finally { pending.current = false; setChecking(false); }
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (pending.current || receipt) return;
    if (!currentVersion || !availableLocations.some((location) => location.id === locationId) || !profile || !strategy || profile.key.upstream_version !== currentVersion.version_string || (updateVersionId && !confirmedUpdate)) {
      setError("Selecciona una versión, una ubicación conectada, un perfil validado y una estrategia anunciada."); return;
    }
    const request = updateVersionId ? {
      strategy, storage_location_id: locationId, profile_lock_hash: profile.lock_sha256, external_host_id: null,
    } : {
      runtime_id: runtime.id, upstream_version: currentVersion.version_string, strategy,
      storage_location_id: locationId, profile_lock_hash: profile.lock_sha256, external_host_id: null,
    } satisfies RuntimeInstallRequest;
    const body = JSON.stringify(request);
    if (attempt.current?.body !== body) attempt.current = { body, key: crypto.randomUUID() };
    pending.current = true; setBusy(true); setError(null);
    try {
      const result = await client.request<JobReceipt>({
        method: "POST", path: updateVersionId ? `/api/v1/runtime-versions/${updateVersionId}/update` : "/api/v1/runtime-installations", body: request,
        headers: { "Idempotency-Key": attempt.current.key },
      });
      if (!result || typeof result.job_id !== "string" || typeof result.status_url !== "string" || !(result.state in JOB_LABEL)) throw new Error("runtime_install_job_receipt_invalid");
      attempt.current = null; setReceipt(result); setJob({ job_id: result.job_id, state: result.state, error: null }); onSubmitted();
    } catch (cause) {
      setError(cause instanceof Error && cause.message === "runtime_install_job_receipt_invalid"
        ? "El daemon aceptó la petición, pero el recibo del job no cumple el contrato. Conserva la respuesta y consulta el historial de jobs."
        : explainInstall(cause));
    } finally { pending.current = false; setBusy(false); }
  };

  const refreshJob = async () => {
    if (!receipt || refreshing) return;
    setRefreshing(true); setError(null);
    try {
      const current = await client.get<RuntimeInstallJobState>(receipt.status_url);
      if (current.job_id !== receipt.job_id || !(current.state in JOB_LABEL)) throw new Error("runtime_install_job_binding_invalid");
      setJob(current);
      if (current.state === "COMPLETED" || current.state === "FAILED" || current.state === "CANCELLED") onSubmitted();
    } catch (cause) {
      setError(cause instanceof Error && cause.message === "runtime_install_job_binding_invalid"
        ? "El estado consultado no pertenece al job de esta solicitud."
        : "No se pudo consultar el job; la solicitud ya está registrada y puedes volver a comprobarla.");
    } finally { setRefreshing(false); }
  };

  const cancelJob = async () => {
    if (!receipt || !job || (job.state !== "QUEUED" && job.state !== "RUNNING") || cancelling || refreshing || cancelRequested) return;
    setCancelling(true); setError(null);
    try {
      const result = await cancelRuntimeInstallJob(client, receipt.job_id);
      if (result.job_id !== receipt.job_id || !(result.state === "CANCELLED" || result.state === "RUNNING" || result.state === "COMPLETED")) throw new Error("runtime_install_cancel_receipt_invalid");
      setReceipt(result); setJob({ job_id: result.job_id, state: result.state, error: null });
      setCancelRequested(result.state === "RUNNING");
      if (result.state !== "RUNNING") onSubmitted();
    } catch (cause) {
      setError(cause instanceof Error && cause.message === "runtime_install_cancel_receipt_invalid"
        ? "El daemon no confirmó la cancelación. Actualiza el estado antes de volver a intentarlo."
        : explainInstall(cause));
    } finally { setCancelling(false); }
  };

  return <section className="runtime-install-form" aria-labelledby="runtime-install-title">
    <h3 id="runtime-install-title">{updateVersionId ? "Actualizar runtime" : "Solicitar instalación"}</h3>
    <p>{updateVersionId ? "Se enviará una solicitud explícita para instalar la versión candidata elegida. El daemon vuelve a comprobar la política, el perfil y el espacio antes de encolar." : "Elige una versión, un perfil P099, su estrategia y una ubicación conectada. El daemon valida la cualificación y el espacio antes de encolar."}</p>
    {!availableLocations.length && <p role="status" className="state-unknown">No hay ubicaciones de almacenamiento conectadas. Conecta una antes de solicitar una instalación.</p>}
    <form className="form" onSubmit={(event) => void submit(event)}>
      {updateVersionId ? <p><strong>Versión candidata:</strong> {currentVersion?.version_string ?? "Versión no disponible"}</p> : <>
        <label htmlFor={`${hashId}-version`}>Versión registrada</label>
        <select id={`${hashId}-version`} value={versionId} disabled={busy || !!receipt} onChange={(event) => { setVersionId(event.target.value); setProfile(null); setStrategy(""); setError(null); }} required>
          <option value="">Selecciona una versión</option>
          {versions.map((version) => <option key={version.id} value={version.id}>{version.version_string} · {CHANNEL_LABEL[version.channel] || version.channel}</option>)}
        </select>
      </>}
      <label htmlFor={`${hashId}-location`}>Ubicación de almacenamiento</label>
      <select id={`${hashId}-location`} value={locationId} disabled={busy || !!receipt || !availableLocations.length} onChange={(event) => { setLocationId(event.target.value); setConfirmedUpdate(false); }} required>
        <option value="">Selecciona una ubicación conectada</option>
        {availableLocations.map((location) => <option key={location.id} value={location.id}>{location.name} · {location.drive_letter ?? location.canonical_path}</option>)}
      </select>
      <label htmlFor={`${hashId}-profile`}>SHA-256 del perfil P099</label>
      <div className="actions"><input id={`${hashId}-profile`} className="mono" value={profileHash} maxLength={64} disabled={busy || checking || !!receipt} autoComplete="off" onChange={(event) => { setProfileHash(event.target.value); setProfile(null); setStrategy(""); setConfirmedUpdate(false); setError(null); }} />
        <button type="button" className="secondary" disabled={busy || checking || !!receipt} onClick={() => void checkProfile()}>{checking ? "Consultando…" : "Validar perfil"}</button></div>
      {profile && <div role="status" className="runtime-install-profile">
        <p><strong>{profile.key.runtime} · {profile.key.upstream_version}</strong></p>
        <p className="state-unknown">El lock permanece UNQUALIFIED. La evidencia separada y la estrategia se vuelven a comprobar en el servidor.</p>
      </div>}
      <label htmlFor={`${hashId}-strategy`}>Estrategia anunciada</label>
      <select id={`${hashId}-strategy`} value={strategy} disabled={!profile || busy || !!receipt} onChange={(event) => { setStrategy(event.target.value as InstallationStrategy); setConfirmedUpdate(false); }} required>
        <option value="">Selecciona una estrategia</option>
        {strategies.map((option) => <option key={option} value={option}>{STRATEGY_LABEL[option]}</option>)}
      </select>
      {updateVersionId && <label className="check"><input type="checkbox" checked={confirmedUpdate} disabled={busy || !!receipt} onChange={(event) => setConfirmedUpdate(event.target.checked)} />Confirmo solicitar la actualización a {currentVersion?.version_string ?? "la versión seleccionada"}.</label>}
      {error && <p role="alert" className="state-unknown">{error}</p>}
      {receipt && job && <div role="status" className="runtime-install-job">
        <p><strong>{JOB_LABEL[job.state]}</strong> · job <span className="mono">{receipt.job_id}</span></p>
        {cancelRequested && job.state === "RUNNING" && <p>Cancelación solicitada; esperando a que el worker detenga y drene la instalación.</p>}
        {job.state === "FAILED" && <p role="alert" className="state-unknown">{job.error?.message ?? "El job terminó con error."}</p>}
        {(job.state === "QUEUED" || job.state === "RUNNING") && <button type="button" className="secondary" disabled={refreshing || cancelling || cancelRequested} onClick={() => void cancelJob()}>{cancelling ? "Cancelando…" : job.state === "QUEUED" ? "Cancelar instalación en cola" : "Cancelar instalación"}</button>}
        <button type="button" className="secondary" disabled={refreshing || cancelling || job.state === "COMPLETED" || job.state === "FAILED" || job.state === "CANCELLED"} onClick={() => void refreshJob()}>{refreshing ? "Consultando…" : "Actualizar estado de la instalación"}</button>
      </div>}
      <div className="actions"><button type="submit" disabled={busy || checking || !!receipt || !versions.length || !availableLocations.length || !profile || !strategy || (!!updateVersionId && !confirmedUpdate)}>{busy ? "Encolando…" : updateVersionId ? "Confirmar y solicitar actualización" : "Solicitar instalación"}</button>
        <button type="button" className="secondary" disabled={busy || checking || refreshing} onClick={onClose}>Cerrar</button></div>
    </form>
  </section>;
}

function RuntimeDetails({ runtime, client, refresh, onUpdated, onClose }: { runtime: Runtime; client: ApiClient; refresh: number; onUpdated: (runtime: Runtime) => void; onClose: () => void }) {
  const heading = useRef<HTMLHeadingElement>(null), editButton = useRef<HTMLButtonElement>(null), previousEditing = useRef(false);
  const [versions, setVersions] = useState<RuntimeVersion[]>([]), [installations, setInstallations] = useState<RuntimeInstallation[]>([]), [backends, setBackends] = useState<RuntimeBackend[]>([]), [hosts, setHosts] = useState<ExternalHost[]>([]), [locations, setLocations] = useState<StorageLocation[]>([]);
  const [installForm, setInstallForm] = useState(false);
  const [updateVersionId, setUpdateVersionId] = useState<string | null>(null), [rollbackVersionId, setRollbackVersionId] = useState<string | null>(null);
  const [rollbackBusy, setRollbackBusy] = useState(false), [rollbackError, setRollbackError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false), [loading, setLoading] = useState(true), [error, setError] = useState<string | null>(null), [revision, setRevision] = useState(0);
  const rollbackPending = useRef(false), rollbackAttempt = useRef<{ body: string; key: string } | null>(null);
  useEffect(() => { heading.current?.focus(); }, []);
  useEffect(() => { if (previousEditing.current && !editing) editButton.current?.focus(); previousEditing.current = editing; }, [editing]);
  useEffect(() => {
    let active = true;
    setLoading(true); setError(null);
    const read = async () => {
      const [current, vs, ins, bes, hs, locs] = await Promise.all([
        client.get<Runtime>(`/api/v1/runtimes/${runtime.id}`),
        allPages<RuntimeVersion>(client, `/api/v1/runtimes/${runtime.id}/versions`, () => active),
        allPages<RuntimeInstallation>(client, `/api/v1/runtime-installations?runtime_id=${runtime.id}`, () => active),
        allPages<RuntimeBackend>(client, `/api/v1/runtime-backends?runtime_id=${runtime.id}`, () => active),
        allPages<ExternalHost>(client, "/api/v1/external-hosts", () => active),
        allPages<StorageLocation>(client, "/api/v1/storage-locations", () => active),
      ]);
      if (!active) return;
      onUpdated(current);
      setVersions(vs); setInstallations(ins); setBackends(bes); setHosts(hs.filter((h) => ins.some((i) => i.external_host_id === h.id))); setLocations(locs);
    };
    void read().catch((e: unknown) => { if (active) setError(explain(e)); }).finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [client, runtime.id, revision, refresh, onUpdated]);
  const submitRollback = async () => {
    if (!rollbackVersionId || rollbackPending.current) return;
    const request = { expected_default_version_id: runtime.default_version_id };
    const body = JSON.stringify(request);
    if (rollbackAttempt.current?.body !== body) rollbackAttempt.current = { body, key: crypto.randomUUID() };
    rollbackPending.current = true; setRollbackBusy(true); setRollbackError(null);
    try {
      const receipt = await client.request<{ resource_id: string; row_version: number; event_id: string }>({
        method: "POST", path: `/api/v1/runtime-versions/${rollbackVersionId}/rollback`, body: request,
        headers: { "Idempotency-Key": rollbackAttempt.current.key },
      });
      if (receipt.resource_id !== runtime.id || !Number.isInteger(receipt.row_version) || typeof receipt.event_id !== "string") throw new Error("runtime_rollback_receipt_invalid");
      rollbackAttempt.current = null; setRollbackVersionId(null); setRevision((value) => value + 1);
    } catch (cause) {
      setRollbackError(cause instanceof Error && cause.message === "runtime_rollback_receipt_invalid" ? "El daemon no devolvió un recibo válido del rollback." : explain(cause));
    } finally { rollbackPending.current = false; setRollbackBusy(false); }
  };
  return <section className="panel runtime-detail" aria-label={`Detalle de ${runtime.name}`}>
    <div className="runtime-heading"><h2 id="runtime-detail-title" ref={heading} tabIndex={-1}>{runtime.name}</h2><button type="button" className="secondary" onClick={onClose}>Cerrar detalle</button></div>
    {editing ? <PolicyEditor runtime={runtime} client={client} onSaved={(value) => { onUpdated(value); setEditing(false); }} onCancel={() => setEditing(false)} /> : <div>
      <h3>Política de canales</h3><p>{runtime.channel_policy.allowed_channels.map((c) => CHANNEL_LABEL[c] || c).join(" · ")}</p>
      <button ref={editButton} type="button" className="secondary" onClick={() => setEditing(true)}>Editar política</button>
      <h3>Retención de versiones</h3><p>Configuración actual: conservar {runtime.retention_policy.keep_last_validated} versiones validadas.</p>
      <RetentionPolicyEditor runtime={runtime} client={client} onSaved={(value) => { onUpdated(value); setRevision((current) => current + 1); }} />
    </div>}
    <h3>Versiones registradas</h3>
    {loading && <p role="status">Cargando versiones, instalaciones y ubicaciones…</p>}
    {error && <div><p role="alert" className="state-unknown">{error}</p><button type="button" className="secondary" onClick={() => setRevision((v) => v + 1)}>Reintentar detalle</button></div>}
    {!loading && !error && <>
      {!versions.length ? <p>No hay versiones registradas para este runtime.</p> : <><p className="detail">Se muestran todos los canales. La política no oculta versiones existentes.</p><ul className="runtime-versions" aria-label="Versiones registradas">{versions.map((version) => <VersionRow key={version.id} version={version} isDefault={runtime.default_version_id === version.id}
        onUpdate={() => { setInstallForm(false); setUpdateVersionId(version.id); }} onRollback={() => { setRollbackError(null); setRollbackVersionId(version.id); rollbackAttempt.current = null; }} />)}</ul></>}
      <RuntimePinManager client={client} versions={versions} refresh={revision} onChanged={() => setRevision((value) => value + 1)} />
      {rollbackVersionId && <section className="runtime-rollback-confirm" aria-label="Confirmar rollback">
        <h3>Confirmar rollback</h3>
        <p>La versión predeterminada de {runtime.name} pasará a {versions.find((version) => version.id === rollbackVersionId)?.version_string ?? "la versión elegida"}. La operación solo se acepta si ya existe una instalación validada.</p>
        {rollbackError && <p role="alert" className="state-unknown">{rollbackError}</p>}
        <div className="actions"><button type="button" disabled={rollbackBusy} onClick={() => void submitRollback()}>{rollbackBusy ? "Aplicando…" : "Confirmar rollback"}</button>
          <button type="button" className="secondary" disabled={rollbackBusy} onClick={() => { setRollbackVersionId(null); rollbackAttempt.current = null; }}>Cancelar</button></div>
      </section>}
      <div className="actions runtime-install-entry"><button type="button" disabled={!versions.length} onClick={() => { setUpdateVersionId(null); setInstallForm((open) => !open); }}>{installForm ? "Ocultar solicitud" : "Solicitar una instalación"}</button></div>
      <h3>Instalaciones y backends</h3>
      {!installations.length ? <p>No hay instalaciones registradas.</p> : installations.map((installation) => <div className="runtime-installation" key={installation.id}>
        <dl className="facts"><dt>Versión</dt><dd>{versions.find((v) => v.id === installation.runtime_version_id)?.version_string ?? "Versión no disponible"}</dd>
          <dt>Estado / etapa</dt><dd>{inventoryLabel(installation.state)} / {inventoryLabel(installation.stage)}</dd><dt>Registro / propiedad</dt><dd>{inventoryLabel(installation.registration_mode)} / {inventoryLabel(installation.ownership)}</dd>
          <dt>Estrategia</dt><dd>{inventoryLabel(installation.strategy)}</dd><dt>Ruta relativa</dt><dd>{installation.relative_path ?? "No informada"}</dd></dl>
        {backends.filter((b) => b.runtime_installation_id === installation.id).map((backend) => <p key={backend.id}>Backend: {backend.backend_type} · {backend.status === "UNKNOWN" ? "Disponibilidad desconocida" : backend.status === "AVAILABLE" ? "Disponible según el registro" : "No disponible"}{backend.driver_version ? ` · Driver ${backend.driver_version}` : ""}</p>)}
      </div>)}
      {!!hosts.length && <><h3>Hosts externos asociados</h3>{hosts.map((host) => <p key={host.id}>{host.name} · {host.host_type} · {host.state === "UNKNOWN" ? "Conectividad desconocida" : inventoryLabel(host.state)} · {host.base_url}</p>)}</>}
    </>}
    {(installForm || updateVersionId) && versions.length > 0 && <RuntimeInstallForm key={updateVersionId ?? "install"} runtime={runtime} versions={versions} locations={locations} client={client} updateVersionId={updateVersionId ?? undefined}
      onClose={() => { setInstallForm(false); setUpdateVersionId(null); }} onSubmitted={() => setRevision((value) => value + 1)} />}
  </section>;
}

export function RuntimesPage({ client, focusResourceId }: { client: ApiClient; focusResourceId?: string }) {
  const [items, setItems] = useState<Runtime[]>([]), [connectors, setConnectors] = useState<Connector[]>([]), [hosts, setHosts] = useState<ExternalHost[]>([]), [cursor, setCursor] = useState<string | null>(null); const [executionHosts, setExecutionHosts] = useState<ExecutionHost[]>([]), [execHostError, setExecHostError] = useState(false);
  const [selected, setSelected] = useState<Runtime | null>(null), [adding, setAdding] = useState(false), [addingHost, setAddingHost] = useState(false), [addingAdapter, setAddingAdapter] = useState(false), [loading, setLoading] = useState(true), [moreBusy, setMoreBusy] = useState(false);
  const [error, setError] = useState<string | null>(null), [connectorError, setConnectorError] = useState(false), [hostError, setHostError] = useState(false), [query, setQuery] = useState(""), [draftQuery, setDraftQuery] = useState(""), [revision, setRevision] = useState(0);
  const generation = useRef(0), attempt = useRef<{ body: string; key: string } | null>(null), queryId = useId();
  const { notify } = useToasts();
  const [detailRefresh, setDetailRefresh] = useState(0);
  const updated = useCallback((value: Runtime) => {
    setSelected((current) => current?.id === value.id && current.row_version <= value.row_version ? value : current);
    setItems((old) => old.map((r) => r.id === value.id && r.row_version <= value.row_version ? value : r));
  }, []);
  const path = (next: string | null) => { const params = new URLSearchParams({ limit: "200" }); if (query) params.set("q", query); if (next) params.set("cursor", next); return `/api/v1/runtimes?${params}`; };
  useEffect(() => {
    const active = ++generation.current;
    setLoading(true); setMoreBusy(false); setError(null); setConnectorError(false); setHostError(false); setItems([]); setCursor(null); setConnectors([]); setHosts([]); setExecutionHosts([]); setExecHostError(false);
    const params = new URLSearchParams({ limit: "200" }); if (query) params.set("q", query);
    void Promise.allSettled([
      client.get<Page<Runtime>>(`/api/v1/runtimes?${params}`),
      allPages<AdapterInstallation>(client, "/api/v1/adapters", () => active === generation.current),
      allPages<ExternalHost>(client, "/api/v1/external-hosts", () => active === generation.current),
      client.get<Page<ExecutionHost>>("/api/v1/execution-hosts?limit=200"),
    ])
      .then(([runtimes, adapters, externalHosts, execHosts]) => {
        if (active !== generation.current) return;
        if (runtimes.status === "fulfilled") { setItems(runtimes.value.items); setCursor(runtimes.value.next_cursor); } else setError(explain(runtimes.reason));
        if (adapters.status === "fulfilled") setConnectors(runtimeConnections(adapters.value)); else setConnectorError(true);
        if (externalHosts.status === "fulfilled") setHosts(externalHosts.value); else setHostError(true);
        if (execHosts.status === "fulfilled") setExecutionHosts(execHosts.value.items); else setExecHostError(true);
      }).finally(() => { if (active === generation.current) setLoading(false); });
    return () => { generation.current++; };
  }, [client, query, revision]);
  useEffect(() => {
    if (!focusResourceId) return;
    const runtime = items.find((item) => item.id === focusResourceId);
    if (runtime) setSelected(runtime);
    const target = document.querySelector<HTMLElement>(`[data-resource-id="${focusResourceId}"]`);
    target?.focus({ preventScroll: true });
    target?.scrollIntoView({ block: "center" });
  }, [focusResourceId, items]);
  const more = async () => {
    if (!cursor || moreBusy) return;
    const active = generation.current; setMoreBusy(true); setError(null);
    try { const page = await client.get<Page<Runtime>>(path(cursor)); if (active === generation.current) { setItems((old) => [...old, ...page.items]); setCursor(page.next_cursor); } }
    catch (e) { if (active === generation.current) setError(explain(e)); }
    finally { if (active === generation.current) setMoreBusy(false); }
  };
  const save = async (request: RuntimeCreate) => {
    const body = JSON.stringify(request); if (attempt.current?.body !== body) attempt.current = { body, key: crypto.randomUUID() };
    const created = await client.request<Runtime>({ method: "POST", path: "/api/v1/runtimes", body: request, headers: { "Idempotency-Key": attempt.current.key } });
    attempt.current = null; setAdding(false); setSelected(created); setQuery(""); setDraftQuery(""); setRevision((v) => v + 1); notify("info", "Runtime registrado.");
  };
  const hostSaved = () => {
    setAddingHost(false); setRevision((v) => v + 1);
    notify("info", "Host externo registrado; la conectividad permanece desconocida.");
  };
  const adapterCreated = () => {
    setRevision((v) => v + 1);
    notify("info", "Adaptador declarativo registrado como UNTRUSTED.");
  };
  return <div className="stack runtime-page">
    <p>Consulta los motores registrados, sus versiones y los canales que LARM puede descubrir automáticamente.</p>
    <div className="actions"><button type="button" disabled={loading || adding || addingHost || addingAdapter || !connectors.length} onClick={() => setAdding(true)}>Registrar runtime</button><button type="button" className="secondary" disabled={loading || adding || addingHost || addingAdapter} onClick={() => setAddingHost(true)}>Registrar host externo</button><button type="button" className="secondary" disabled={loading || adding || addingHost || addingAdapter} onClick={() => setAddingAdapter(true)}>Asistente de runtime declarativo</button><button type="button" className="secondary" disabled={loading || adding || addingHost || addingAdapter} onClick={() => { setRevision((v) => v + 1); setDetailRefresh((v) => v + 1); }}>Actualizar runtimes</button></div>
    {adding && <RuntimeForm connectors={connectors} onSave={save} onCancel={() => { setAdding(false); attempt.current = null; }} />}
    {addingHost && <RuntimeWizard client={client} onSaved={hostSaved} onCancel={() => setAddingHost(false)} />}
    {addingAdapter && <DeclarativeRuntimeWizard client={client} onCreated={adapterCreated} onCancel={() => setAddingAdapter(false)} />}
    <section className="panel execution-hosts" aria-labelledby="execution-hosts-title">
      <h2 id="execution-hosts-title">Hosts de ejecución observados</h2>
      <p className="detail">Motores virtualizados detectados por sondeo read-only en este equipo (WSL2 y motor de contenedores). El registro es observación estructurada: no acredita compatibilidad ni habilita ejecución.</p>
      {execHostError && <p role="status">No se pudo cargar el registro de hosts de ejecución.</p>}
      {!execHostError && !executionHosts.length && !loading && <p>Todavía no hay hosts de ejecución observados.</p>}
      {!!executionHosts.length && <ul aria-label="Hosts de ejecución observados">{executionHosts.map((host) => <li key={host.id}>
        <strong>{host.name}</strong><span> · {host.kind === "WSL2" ? `WSL${host.wsl_version ?? 2}` : "Motor de contenedores"} · {host.default ? "Por defecto" : "Secundario"} · {host.status === "ONLINE" ? "En línea" : host.status === "OFFLINE" ? "Fuera de línea" : "Estado desconocido"}</span>
        {host.engine_version && <p className="mono detail">Versión del motor: {host.engine_version}</p>}
      </li>)}</ul>}
    </section>
    <section className="panel external-hosts" aria-labelledby="external-hosts-title">
      <h2 id="external-hosts-title">Hosts externos</h2>
      <p className="detail">Registrar un host guarda su URL y referencia de credencial. La conectividad no se considera verificada por el registro.</p>
      {hostError && <p role="status">No se pudo cargar el catálogo de hosts externos.</p>}
      {!hostError && !hosts.length && !loading && <p>Todavía no hay hosts externos registrados.</p>}
      {!!hosts.length && <ul aria-label="Hosts externos registrados">{hosts.map((host) => <li key={host.id}>
        <strong>{host.name}</strong><span> · {host.host_type} · {host.state === "UNKNOWN" ? "Conectividad desconocida" : inventoryLabel(host.state)}</span>
        <p className="mono detail">{host.base_url}</p>
        {host.credential_reference_id && <span className="detail">Referencia de credencial asociada</span>}
      </li>)}</ul>}
    </section>
    <form className="form runtime-filter" onSubmit={(e) => { e.preventDefault(); setQuery(draftQuery.trim()); setRevision((v) => v + 1); }}>
      <label htmlFor={queryId}>Buscar runtimes por nombre</label><div className="actions"><input id={queryId} value={draftQuery} onChange={(e) => setDraftQuery(e.target.value)} /><button type="submit" className="secondary" disabled={loading}>Buscar runtimes</button></div>
    </form>
    {loading && <p role="status">Cargando runtimes…</p>}
    {error && <p role="alert" className="state-unknown">{error}</p>}
    {!loading && connectorError && <p role="status">No se pudieron cargar los conectores. Actualiza la lista para reintentar.</p>}
    {!loading && !connectorError && !connectors.length && <p>No hay conectores de runtime registrados. Registra una instalación de adaptador para poder añadir runtimes.</p>}
    {!loading && !error && !items.length && <p>{query ? "No hay runtimes que coincidan con la búsqueda." : "Todavía no hay ningún runtime registrado."}</p>}
    {!!items.length && <ul className="runtime-list" aria-label="Runtimes registrados">{items.map((runtime) => <li key={runtime.id} tabIndex={-1} data-resource-id={runtime.id}><div><h2>{runtime.name}</h2><p className="detail">Canales permitidos: {runtime.channel_policy.allowed_channels.map((c) => CHANNEL_LABEL[c] || c).join(" · ")}</p></div><button type="button" className="secondary" aria-pressed={selected?.id === runtime.id} onClick={() => { if (selected?.id === runtime.id) { document.getElementById("runtime-detail-title")?.focus(); setDetailRefresh((v) => v + 1); } else setSelected(runtime); setAdding(false); }}>Ver versiones de {runtime.name}</button></li>)}</ul>}
    {cursor && <button type="button" className="secondary" disabled={loading || moreBusy} onClick={() => void more()}>{moreBusy ? "Cargando…" : "Cargar más runtimes"}</button>}
    <RuntimeProfileLocks client={client} />
    <LaunchProfiles client={client} />
    {selected && <RuntimeDetails key={selected.id} runtime={selected} client={client} refresh={detailRefresh} onClose={() => setSelected(null)} onUpdated={updated} />}
  </div>;
}
