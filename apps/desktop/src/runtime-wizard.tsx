import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { ApiClient, ApiError } from "./api/client";
import type { AdapterInstallation, ExternalHostCreate, JobReceipt } from "./api/runtimes";

type JsonRecord = Record<string, unknown>;

function record(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringAt(value: unknown, key: string): string | null {
  return record(value) && typeof value[key] === "string" ? value[key] as string : null;
}

function stringArray(value: unknown, key: string): string[] {
  if (!record(value) || !Array.isArray(value[key])) return [];
  return (value[key] as unknown[]).filter((entry): entry is string => typeof entry === "string");
}

function parseDeclarativeRuntime(source: string): JsonRecord {
  let value: unknown;
  try { value = JSON.parse(source); } catch { throw new Error("El borrador no es JSON válido."); }
  if (!record(value) || value.schema_version !== "1.0" || !record(value.manifest)) throw new Error("El documento debe incluir schema_version 1.0 y un manifest.");
  const { manifest } = value;
  if (manifest.family !== "runtime") throw new Error("El asistente sólo admite adaptadores de la familia runtime.");
  if (!stringAt(manifest, "id") || !stringAt(manifest, "version") || !stringAt(manifest, "publisher") || manifest.protocol_version !== "1.0") throw new Error("El manifest requiere id, versión, publicador y protocol_version 1.0.");
  const permissions = manifest.permissions;
  if (!record(permissions) || !["read_roots", "write_roots", "network_origins", "process_allowlist", "secret_refs"].every((key) => Array.isArray(permissions[key]))) throw new Error("Declara todas las listas de permisos antes de guardar.");
  if (!record(manifest.executable) || !stringAt(manifest.executable, "relative_path") || !Array.isArray(manifest.executable.args)) throw new Error("Indica la ruta relativa y los argumentos del ejecutable del adaptador.");
  if (!record(value.version_probe) || !stringAt(value.version_probe, "executable") || !Array.isArray(value.version_probe.args)) throw new Error("Completa el comando de consulta de versión.");
  if (!record(value.start) || !stringAt(value.start, "executable") || !Array.isArray(value.start.args) || !stringAt(value.start, "cwd")) throw new Error("Completa el comando de inicio y su directorio de trabajo.");
  if (!record(value.health) || !stringAt(value.health, "url") || !stringAt(value, "protocol_profile_id")) throw new Error("Indica el endpoint de salud y el perfil de protocolo.");
  if (!record(value.environment) || !Array.isArray(value.environment.allowlist) || !Array.isArray(value.environment.variables)) throw new Error("Completa la allowlist y los bindings de entorno.");
  if (!Array.isArray(manifest.installation_strategies) || manifest.installation_strategies.length === 0) throw new Error("Declara las estrategias anunciadas por este adaptador.");
  return value;
}

function commandText(executable: string | null, args: unknown): string {
  const values = Array.isArray(args) ? args.filter((value): value is string => typeof value === "string") : [];
  return [executable, ...values].filter((value): value is string => !!value).map((value) => JSON.stringify(value)).join(" ");
}

function isInstallation(value: unknown): value is AdapterInstallation {
  return record(value) && typeof value.id === "string" && typeof value.adapter_id === "string" && typeof value.version === "string" && typeof value.family === "string";
}

function isJobReceipt(value: unknown): value is JobReceipt {
  return record(value) && typeof value.job_id === "string" && typeof value.status_url === "string" && typeof value.state === "string";
}

interface AdapterProbeState {
  job_id: string;
  state: string;
  resource_type: string;
  resource_id: string | null;
  progress: Record<string, unknown>;
  result: Record<string, unknown> | null;
  error: { message?: string } | null;
  evidence_refs: string[];
}

/** Wizard P037 basado en el contrato declarativo ya disponible (P023/P026). */
export function DeclarativeRuntimeWizard({ client, onCreated, onCancel }: {
  client: ApiClient;
  onCreated: () => void;
  onCancel: () => void;
}) {
  const id = useId(), heading = useRef<HTMLHeadingElement>(null), createAttempt = useRef<{ body: string; key: string } | null>(null), probeKey = useRef<string | null>(null);
  const [source, setSource] = useState(""), [draft, setDraft] = useState<JsonRecord | null>(null), [installation, setInstallation] = useState<AdapterInstallation | null>(null);
  const [probeReceipt, setProbeReceipt] = useState<JobReceipt | null>(null), [probeState, setProbeState] = useState<AdapterProbeState | null>(null);
  const [busy, setBusy] = useState(false), [checking, setChecking] = useState(false), [error, setError] = useState<string | null>(null), [notice, setNotice] = useState<string | null>(null);
  useEffect(() => { heading.current?.focus(); }, []);

  const validate = (): JsonRecord | null => {
    try { const parsed = parseDeclarativeRuntime(source); setDraft(parsed); setError(null); setNotice("Estructura básica válida. El servidor volverá a validar el contrato completo y las rutas antes de guardar."); return parsed; }
    catch (cause) { setDraft(null); setError(cause instanceof Error ? cause.message : "No se pudo validar el borrador."); return null; }
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy || installation) return;
    const parsed = validate();
    if (!parsed) return;
    const body = JSON.stringify(parsed);
    if (createAttempt.current?.body !== body) createAttempt.current = { body, key: crypto.randomUUID() };
    setBusy(true); setError(null); setNotice(null);
    try {
      const result: unknown = await client.request({ method: "POST", path: "/api/v1/adapters/declarative", body: parsed, headers: { "Idempotency-Key": createAttempt.current.key } });
      if (!isInstallation(result)) throw new Error("adapter_create_receipt_invalid");
      setInstallation(result); createAttempt.current = null; onCreated();
      setNotice("Adaptador guardado como CUSTOM y UNTRUSTED. Revisa la prueba de conformidad; la ejecución requiere la autorización correspondiente.");
    } catch (cause) {
      setError(cause instanceof Error && cause.message === "adapter_create_receipt_invalid" ? "El servidor respondió sin un recibo válido; comprueba el catálogo antes de reintentar." : explain(cause));
    } finally { setBusy(false); }
  };

  const probe = async () => {
    if (!installation || busy) return;
    probeKey.current ??= crypto.randomUUID(); setBusy(true); setError(null); setNotice(null);
    try {
      const receipt: unknown = await client.request({ method: "POST", path: `/api/v1/adapters/${encodeURIComponent(installation.id)}/probe`, body: { resource_id: null, profile_id: "conformance.v1" }, headers: { "Idempotency-Key": probeKey.current } });
      if (!isJobReceipt(receipt)) throw new Error("adapter_probe_receipt_invalid");
      setProbeReceipt(receipt); setProbeState(null); probeKey.current = null;
      setNotice("Prueba encolada. Aún no confirma conformidad: consulta el estado del job cuando termine.");
    } catch (cause) {
      setError(cause instanceof Error && cause.message === "adapter_probe_receipt_invalid" ? "La respuesta no incluye un recibo de job válido; comprueba los jobs antes de reintentar." : explain(cause));
    } finally { setBusy(false); }
  };

  const refreshProbe = async () => {
    if (!probeReceipt || checking) return;
    setChecking(true); setError(null);
    try {
      const state = await client.get<AdapterProbeState>(probeReceipt.status_url);
      if (state.job_id !== probeReceipt.job_id || state.resource_id !== installation?.id) throw new Error("adapter_probe_job_binding_invalid");
      setProbeState(state);
      if (["COMPLETED", "FAILED", "CANCELLED"].includes(state.state)) setNotice(`La prueba terminó: ${state.state}. Revisa su resultado y evidencias antes de habilitar cualquier ejecución.`);
    } catch (cause) {
      setError(cause instanceof Error && cause.message === "adapter_probe_job_binding_invalid" ? "El job consultado no corresponde a este adaptador." : "No se pudo consultar el job de conformidad.");
    } finally { setChecking(false); }
  };

  const manifest = draft && record(draft.manifest) ? draft.manifest : null;
  const executable = manifest && record(manifest.executable) ? manifest.executable : null;
  const permissions = manifest && record(manifest.permissions) ? manifest.permissions : null;
  const versionProbe = draft && record(draft.version_probe) ? draft.version_probe : null;
  const start = draft && record(draft.start) ? draft.start : null;
  const health = draft && record(draft.health) ? draft.health : null;

  return <section className="panel form runtime-wizard" aria-labelledby={`${id}-title`}>
    <h2 id={`${id}-title`} ref={heading} tabIndex={-1}>Asistente de runtime declarativo</h2>
    <p>Prepara un adaptador runtime sin recompilar LARM. El borrador sigue la forma DeclarativeAdapter; el servidor valida permisos y rutas reales antes de materializarlo.</p>
    <form onSubmit={(event) => void submit(event)}>
      <label htmlFor={`${id}-document`}>Documento DeclarativeAdapter (JSON)</label>
      <textarea id={`${id}-document`} className="runtime-wizard-json" value={source} disabled={busy || !!installation} onChange={(event) => { setSource(event.target.value); setDraft(null); setError(null); setNotice(null); }} spellCheck={false} aria-describedby={`${id}-secret-help`} required />
      <p className="detail" id={`${id}-secret-help`}>Usa referencias larm/… para credenciales. No pegues tokens ni contraseñas en valores de entorno.</p>
      <div className="actions"><button type="button" className="secondary" disabled={busy || !!installation || !source.trim()} onClick={validate}>Validar borrador</button>
        <button type="submit" disabled={busy || !!installation || !draft}>{busy ? "Guardando…" : "Guardar adaptador"}</button></div>
    </form>
    {draft && manifest && <section className="runtime-wizard-preview" aria-label="Vista previa del adaptador">
      <h3>Revisión antes de guardar</h3>
      <dl className="facts"><dt>Identidad</dt><dd>{stringAt(manifest, "id")} · {stringAt(manifest, "version")} · {stringAt(manifest, "publisher")}</dd>
        <dt>Protocolo</dt><dd>{stringAt(draft, "protocol_profile_id")}</dd>
        <dt>Estrategias anunciadas</dt><dd>{stringArray(manifest, "installation_strategies").join(", ")}</dd>
        <dt>Ejecutable del adaptador</dt><dd className="mono">{commandText(stringAt(executable, "relative_path"), executable?.args)}</dd>
        <dt>Comando de versión</dt><dd className="mono">{commandText(stringAt(versionProbe, "executable"), versionProbe?.args)}</dd>
        <dt>Comando de inicio</dt><dd className="mono">{commandText(stringAt(start, "executable"), start?.args)} · cwd {stringAt(start, "cwd")}</dd>
        <dt>Prueba de salud</dt><dd className="mono">{stringAt(health, "url")}</dd>
        <dt>Lectura</dt><dd className="mono">{stringArray(permissions, "read_roots").join(", ") || "Sin rutas declaradas"}</dd>
        <dt>Escritura</dt><dd className="mono">{stringArray(permissions, "write_roots").join(", ") || "Sin rutas declaradas"}</dd>
        <dt>Red</dt><dd className="mono">{stringArray(permissions, "network_origins").join(", ") || "Sin orígenes declarados"}</dd>
        <dt>Procesos</dt><dd className="mono">{stringArray(permissions, "process_allowlist").join(", ") || "Sin procesos declarados"}</dd>
        <dt>Referencias secretas</dt><dd className="mono">{stringArray(permissions, "secret_refs").join(", ") || "Ninguna"}</dd>
      </dl>
      <p className="notice">Guardar requiere adapters:write. LARM asignará confianza UNTRUSTED y modo CUSTOM; el documento no concede permiso de ejecución.</p>
    </section>}
    {installation && <section className="runtime-wizard-preview" aria-label="Adaptador registrado">
      <h3>Adaptador registrado</h3><p><strong>{installation.adapter_id} · {installation.version}</strong></p>
      <p>Instalación <span className="mono">{installation.id}</span> · {installation.family} · UNTRUSTED</p>
      <div className="actions"><button type="button" disabled={busy} onClick={() => void probe()}>{busy ? "Encolando…" : "Solicitar prueba de conformidad"}</button>
        <button type="button" className="secondary" disabled={!probeReceipt || checking} onClick={() => void refreshProbe()}>{checking ? "Consultando…" : "Consultar prueba"}</button></div>
      {probeReceipt && <p role="status">Job <span className="mono">{probeReceipt.job_id}</span> · {probeState?.state ?? probeReceipt.state}</p>}
      {probeState?.error && <p role="alert" className="state-unknown">{probeState.error.message ?? "La prueba terminó con error."}</p>}
    </section>}
    {notice && <p role="status" className="note">{notice}</p>}
    {error && <p role="alert" className="state-unknown">{error}</p>}
    <div className="actions"><button type="button" className="secondary" disabled={busy} onClick={onCancel}>{installation ? "Cerrar" : "Cancelar"}</button></div>
  </section>;
}

const UUID7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

function explain(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 400) return "La solicitud no cumple el contrato. Revisa la URL, el tipo de host y la referencia de credencial.";
    if (error.status === 403) return "Tu cuenta no tiene permiso para registrar hosts externos.";
    if (error.isOffline) return "Se perdió la conexión con LARM. Comprueba la lista antes de volver a guardar.";
  }
  return "No se pudo registrar el host externo. Comprueba la lista antes de volver a guardar.";
}

export function RuntimeWizard({ client, onSaved, onCancel }: {
  client: ApiClient;
  onSaved: () => void;
  onCancel: () => void;
}) {
  const id = useId(), heading = useRef<HTMLHeadingElement>(null), pending = useRef(false);
  const [name, setName] = useState(""), [baseUrl, setBaseUrl] = useState(""), [hostType, setHostType] = useState("");
  const [credentialReference, setCredentialReference] = useState(""), [registrationMode, setRegistrationMode] = useState<ExternalHostCreate["registration_mode"]>("EXTERNAL");
  const [busy, setBusy] = useState(false), [error, setError] = useState<string | null>(null);

  useEffect(() => { heading.current?.focus(); }, []);

  const validate = (): string | null => {
    if (!name.trim() || !hostType.trim()) return "Indica un nombre y un tipo de host.";
    if (!baseUrl || baseUrl.trim() !== baseUrl) return "Escribe una URL HTTP o HTTPS sin espacios al inicio o al final.";
    try {
      const url = new URL(baseUrl);
      if (!["http:", "https:"].includes(url.protocol) || !url.hostname || url.username || url.password || url.search || url.hash) {
        return "La URL debe usar HTTP o HTTPS y no puede incluir usuario, contraseña, consulta ni fragmento.";
      }
    } catch {
      return "Escribe una URL HTTP o HTTPS válida.";
    }
    if (credentialReference && !UUID7.test(credentialReference.trim())) return "La referencia de credencial debe ser un UUIDv7; nunca pegues aquí el secreto.";
    return null;
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (pending.current) return;
    const validation = validate();
    if (validation) { setError(validation); return; }
    pending.current = true; setBusy(true); setError(null);
    const request: ExternalHostCreate = {
      name: name.trim(), base_url: baseUrl, host_type: hostType.trim(),
      credential_reference_id: credentialReference.trim() || null, registration_mode: registrationMode,
    };
    try {
      await client.request({ method: "POST", path: "/api/v1/external-hosts", body: request });
      onSaved();
    } catch (cause) {
      setError(explain(cause));
    } finally {
      pending.current = false; setBusy(false);
    }
  };

  return <section className="panel form runtime-wizard" aria-labelledby={`${id}-title`}>
    <h2 id={`${id}-title`} ref={heading} tabIndex={-1}>Registrar host externo</h2>
    <p>Guarda una referencia a un servicio HTTP existente. LARM no instala ni inicia ese servicio.</p>
    <form onSubmit={(event) => void submit(event)}>
      <label htmlFor={`${id}-name`}>Nombre</label>
      <input id={`${id}-name`} value={name} disabled={busy} onChange={(event) => setName(event.target.value)} required />
      <label htmlFor={`${id}-url`}>URL base HTTP o HTTPS</label>
      <input id={`${id}-url`} type="url" value={baseUrl} disabled={busy} autoComplete="url" onChange={(event) => setBaseUrl(event.target.value)} placeholder="https://host.example/v1" required />
      <p className="detail">No incluyas tokens ni credenciales en la URL, ni parámetros de consulta.</p>
      <label htmlFor={`${id}-type`}>Tipo de host</label>
      <input id={`${id}-type`} value={hostType} disabled={busy} onChange={(event) => setHostType(event.target.value)} placeholder="vendor.openai-compatible" required />
      <label htmlFor={`${id}-credential`}>Referencia de credencial (opcional)</label>
      <input id={`${id}-credential`} className="mono" value={credentialReference} disabled={busy} autoComplete="off" onChange={(event) => setCredentialReference(event.target.value)} aria-describedby={`${id}-credential-help`} />
      <p className="detail" id={`${id}-credential-help`}>Usa el identificador de Credential Manager. Este formulario nunca acepta el valor secreto.</p>
      <label htmlFor={`${id}-mode`}>Modo de registro</label>
      <select id={`${id}-mode`} value={registrationMode} disabled={busy} onChange={(event) => setRegistrationMode(event.target.value as ExternalHostCreate["registration_mode"])}>
        <option value="EXTERNAL">Servicio externo</option>
        <option value="ADOPTED">Servicio existente adoptado</option>
      </select>
      <div className="runtime-wizard-permissions" role="note">
        <strong>Permiso y estado</strong>
        <p>Guardar requiere el permiso runtimes:write. El host aparecerá con conectividad desconocida hasta que exista una operación contractual de prueba.</p>
      </div>
      {error && <p role="alert" className="state-unknown">{error}</p>}
      <div className="actions">
        <button type="submit" disabled={busy}>{busy ? "Guardando…" : "Guardar host externo"}</button>
        <button type="button" className="secondary" disabled={busy} onClick={onCancel}>Cancelar</button>
      </div>
    </form>
  </section>;
}
