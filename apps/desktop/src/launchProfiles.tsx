import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { ApiClient, ApiError } from "./api/client";
import { launchProfiles, type LaunchProfile, type LaunchProfileCreate, type LaunchProfileKind } from "./api/launchProfiles";

const KINDS: LaunchProfileKind[] = ["ECO", "BALANCED", "PERFORMANCE", "LONG_CONTEXT", "CUSTOM"];

function explainProfileError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 403) return "Tu cuenta no tiene permiso para modificar perfiles de lanzamiento.";
    if (error.status === 409) return "El perfil cambió desde que lo abriste. Actualiza la lista antes de guardar.";
    if (error.status === 400) return "El perfil no cumple el contrato. Revisa los parámetros JSON y sus valores.";
    if (error.isOffline) return "Se perdió la conexión con LARM. Reintenta cuando vuelva a estar disponible.";
  }
  return "No se pudo completar la operación. Puedes volver a intentarlo.";
}

function effectiveDiff(left: LaunchProfile, right: LaunchProfile): string[] {
  const changes: string[] = [];
  const walk = (path: string, a: unknown, b: unknown) => {
    if (a && b && typeof a === "object" && typeof b === "object" && !Array.isArray(a) && !Array.isArray(b)) {
      const first = a as Record<string, unknown>, second = b as Record<string, unknown>;
      for (const key of [...new Set([...Object.keys(first), ...Object.keys(second)])].sort()) walk(`${path}/${key.replaceAll("~", "~0").replaceAll("/", "~1")}`, first[key], second[key]);
    } else if (JSON.stringify(a) !== JSON.stringify(b)) changes.push(path || "/");
  };
  walk("/backend_id", left.backend_id, right.backend_id);
  walk("/parameters", left.parameters, right.parameters);
  return changes;
}

export function LaunchProfiles({ client }: { client: ApiClient }) {
  const [profiles, setProfiles] = useState<LaunchProfile[]>([]);
  const [selectedId, setSelectedId] = useState("");
  const [kind, setKind] = useState<LaunchProfileKind>("BALANCED");
  const [backendId, setBackendId] = useState("");
  const [parameters, setParameters] = useState("");
  const [compareId, setCompareId] = useState("");
  const [loading, setLoading] = useState(true), [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null), [notice, setNotice] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const pending = useRef(false), createAttempt = useRef<{ body: string; key: string } | null>(null), parametersId = useId();
  const selected = profiles.find((profile) => profile.id === selectedId) ?? null;
  const comparisonProfiles = profiles.filter((profile) => profile.id !== selected?.id);
  const compared = comparisonProfiles.find((profile) => profile.id === compareId) ?? comparisonProfiles[0] ?? null;

  const selectProfile = (profile: LaunchProfile | null) => {
    setSelectedId(profile?.id ?? "");
    setKind(profile?.name ?? "BALANCED");
    setBackendId(profile?.backend_id ?? "");
    setParameters(profile ? JSON.stringify(profile.parameters, null, 2) : "");
    setError(null); setNotice(null);
  };

  useEffect(() => {
    let alive = true;
    setLoading(true); setError(null);
    void launchProfiles(client).then((items) => {
      if (!alive) return;
      setProfiles(items);
      if (selectedId && items.some((item) => item.id === selectedId)) {
        const active = items.find((item) => item.id === selectedId)!;
        setKind(active.name); setBackendId(active.backend_id ?? ""); setParameters(JSON.stringify(active.parameters, null, 2));
      } else if (items.length) selectProfile(items[0]);
      else selectProfile(null);
      setCompareId((current) => items.some((item) => item.id === current) ? current : items[0]?.id ?? "");
    }).catch((cause: unknown) => { if (alive) setError(explainProfileError(cause)); })
      .finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; };
  }, [client, revision]);

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (pending.current) return;
    let parsed: unknown;
    try { parsed = JSON.parse(parameters); }
    catch { setError("Los parámetros deben ser un documento JSON válido."); return; }
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) { setError("Los parámetros deben ser un objeto JSON."); return; }
    const body: LaunchProfileCreate = { name: kind, parameters: parsed as Record<string, unknown>, backend_id: backendId.trim() || null };
    pending.current = true; setBusy(true); setError(null); setNotice(null);
    try {
      let saved: LaunchProfile;
      if (selected) {
        saved = await client.request<LaunchProfile>({ method: "PATCH", path: `/api/v1/launch-profiles/${selected.id}`, body, headers: { "If-Match": String(selected.row_version) } });
      } else {
        const serialized = JSON.stringify(body);
        if (createAttempt.current?.body !== serialized) createAttempt.current = { body: serialized, key: crypto.randomUUID() };
        const attempt = createAttempt.current;
        if (!attempt) throw new Error("idempotency_key_missing");
        saved = await client.request<LaunchProfile>({ method: "POST", path: "/api/v1/launch-profiles", body, headers: { "Idempotency-Key": attempt.key } });
        createAttempt.current = null;
      }
      setSelectedId(saved.id); setNotice("Perfil guardado y hash recalculado."); setRevision((value) => value + 1);
    } catch (cause) { setError(explainProfileError(cause)); }
    finally { pending.current = false; setBusy(false); }
  };

  const remove = async () => {
    if (!selected || pending.current) return;
    pending.current = true; setBusy(true); setError(null); setNotice(null);
    try {
      await client.request<unknown>({ method: "DELETE", path: `/api/v1/launch-profiles/${selected.id}`, headers: { "If-Match": String(selected.row_version) } });
      setSelectedId(""); setRevision((value) => value + 1); setNotice("Perfil eliminado.");
    } catch (cause) { setError(explainProfileError(cause)); }
    finally { pending.current = false; setBusy(false); }
  };

  return <section className="panel launch-profiles" aria-labelledby="launch-profiles-title">
    <h2 id="launch-profiles-title">Perfiles de lanzamiento</h2>
    <p>Revisa y guarda parámetros efectivos. Cada cambio genera un hash; las comparaciones excluyen metadatos y marcas de tiempo.</p>
    {loading && <p role="status">Cargando perfiles guardados…</p>}
    {!loading && !profiles.length && !error && <p>Todavía no hay perfiles guardados. Añade parámetros JSON explícitos para crear uno.</p>}
    <div className="actions launch-profile-selectors">
      <label htmlFor="launch-profile-record">Perfil</label>
      <select id="launch-profile-record" value={selectedId} disabled={busy || loading} onChange={(event) => selectProfile(profiles.find((profile) => profile.id === event.target.value) ?? null)}>
        <option value="">Nuevo perfil</option>
        {profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.name} · rev. {profile.row_version}</option>)}
      </select>
      <button type="button" className="secondary" disabled={busy || loading} onClick={() => setRevision((value) => value + 1)}>Actualizar perfiles</button>
      {selected && <button type="button" className="secondary" disabled={busy} onClick={() => selectProfile(null)}>Nuevo perfil</button>}
      {selected && <button type="button" className="secondary" disabled={busy} onClick={() => { setSelectedId(""); setNotice("Copia preparada. Guarda para crear un perfil independiente."); }}>Copiar perfil</button>}
    </div>
    <form className="form" onSubmit={(event) => void save(event)}>
      <label htmlFor="launch-profile-kind">Tipo</label>
      <select id="launch-profile-kind" value={kind} disabled={busy} onChange={(event) => setKind(event.target.value as LaunchProfileKind)}>{KINDS.map((value) => <option key={value}>{value}</option>)}</select>
      <label htmlFor="launch-profile-backend">Preferencia de backend (UUIDv7) o vacío</label>
      <input id="launch-profile-backend" value={backendId} disabled={busy} onChange={(event) => setBackendId(event.target.value)} autoComplete="off" />
      <label htmlFor={parametersId}>Parámetros efectivos (JSON)</label>
      <textarea id={parametersId} className="launch-profile-json mono" rows={12} value={parameters} disabled={busy} onChange={(event) => setParameters(event.target.value)} spellCheck={false} required />
      {selected && <dl className="facts"><dt>SHA-256</dt><dd className="mono">{selected.profile_hash}</dd><dt>Revisión</dt><dd>{selected.row_version}</dd></dl>}
      {error && <p role="alert" className="state-unknown">{error}</p>}
      {notice && <p role="status" className="state-verified">{notice}</p>}
      <div className="actions"><button type="submit" disabled={busy || loading || !parameters.trim()}>{busy ? "Guardando…" : selected ? "Guardar revisión" : "Guardar perfil"}</button>{selected && <button type="button" className="secondary" disabled={busy} onClick={() => void remove()}>Eliminar perfil</button>}</div>
    </form>
    {profiles.length > 1 && selected && <div className="launch-profile-compare">
      <label htmlFor="launch-profile-compare">Comparar con</label>
      <select id="launch-profile-compare" value={compared?.id ?? ""} onChange={(event) => setCompareId(event.target.value)}>{comparisonProfiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.name} · rev. {profile.row_version}</option>)}</select>
      {selected && compared && <p role="status">{(() => { const changes = effectiveDiff(selected, compared); return changes.length ? `Cambios efectivos: ${changes.join(", ")}` : "Los parámetros efectivos son iguales."; })()}</p>}
    </div>}
  </section>;
}
