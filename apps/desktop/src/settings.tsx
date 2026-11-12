import { useEffect, useId, useState, type FormEvent } from "react";
import { ApiClient, ApiError } from "./api/client";
import { readSettings, writeSettings, type VersionedSettings } from "./api/settings";
import { useToasts } from "./components";
import { formatBytes } from "./hardware";

function explain(error: unknown): string {
  if (!(error instanceof ApiError)) return "No se pudo guardar la configuración. Revisa la conexión e inténtalo de nuevo.";
  if (error.reason === "stale_revision") return "La configuración cambió en otra sesión. Recarga la versión actual antes de guardar.";
  if (error.reason === "config_invalid" || error.reason === "schema_violation") return "El valor no es válido. Usa un número entero positivo o selecciona “Sin límite”.";
  if (error.status === 403) return "Tu cuenta no tiene permiso para cambiar la configuración.";
  return error.message;
}

function validLimit(value: string): boolean {
  if (!/^[1-9][0-9]*$/.test(value)) return false;
  try { return BigInt(value) <= 18_446_744_073_709_551_615n; } catch { return false; }
}

function speedLabel(value: string): string {
  const mib = Number(value) / 1_048_576;
  return Number.isFinite(mib) ? `≈ ${mib.toLocaleString("es-ES", { maximumFractionDigits: 2 })} MiB/s` : "";
}

export function BandwidthSettings({ client }: { client: ApiClient }) {
  const inputId = useId();
  const [loaded, setLoaded] = useState<VersionedSettings | null>(null);
  const [limitText, setLimitText] = useState("");
  const [unlimited, setUnlimited] = useState(true);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const { notify } = useToasts();

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    void readSettings(client)
      .then((current) => {
        if (!active) return;
        setLoaded(current);
        const limit = current.value.storage.download_bandwidth_limit_bytes_per_second;
        setUnlimited(limit === null);
        setLimitText(limit ?? "");
      })
      .catch((reason: unknown) => { if (active) setError(explain(reason)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [client, refresh]);

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (!loaded || (!unlimited && !validLimit(limitText.trim()))) return;
    setBusy(true);
    setError(null);
    const value = {
      ...loaded.value,
      storage: {
        ...loaded.value.storage,
        download_bandwidth_limit_bytes_per_second: unlimited ? null : limitText.trim(),
      },
    };
    try {
      const saved = await writeSettings(client, loaded.revision, value);
      setLoaded(saved);
      const limit = saved.value.storage.download_bandwidth_limit_bytes_per_second;
      setUnlimited(limit === null);
      setLimitText(limit ?? "");
      notify("info", "El límite de velocidad de descarga se ha guardado.");
    } catch (reason) {
      setError(explain(reason));
    } finally { setBusy(false); }
  };

  if (loading) return <section className="panel" aria-busy="true"><p>Cargando configuración…</p></section>;
  if (!loaded) return <section className="panel"><h2>Velocidad de descarga</h2><p role="alert">{error ?? "No se pudo cargar la configuración."}</p><div className="actions"><button type="button" onClick={() => setRefresh((value) => value + 1)}>Reintentar</button></div></section>;

  const valid = unlimited || validLimit(limitText.trim());
  return <form className="panel bandwidth-settings" onSubmit={(event) => void save(event)}>
    <h2>Velocidad de descarga</h2>
    <p>Este límite se comparte entre todas las descargas de LARM. Los cambios afectan al siguiente bloque de cada transferencia activa.</p>
    <p className="detail">Revisión guardada: <span className="mono">{loaded.revision}</span></p>
    <label className="check-row" htmlFor={`${inputId}-unlimited`}>
      <input id={`${inputId}-unlimited`} type="checkbox" checked={unlimited} onChange={(event) => setUnlimited(event.target.checked)} />
      Sin límite de velocidad
    </label>
    <label htmlFor={inputId}>Límite agregado (bytes por segundo)</label>
    <input
      id={inputId}
      inputMode="numeric"
      autoComplete="off"
      value={limitText}
      disabled={unlimited}
      aria-describedby={`${inputId}-help`}
      aria-invalid={!valid}
      onChange={(event) => setLimitText(event.target.value)}
      placeholder="1048576"
    />
    <p className="detail" id={`${inputId}-help`}>
      {unlimited ? "Las descargas usan todo el ancho de banda disponible." : valid ? speedLabel(limitText.trim()) : "Escribe un número entero mayor que cero."}
    </p>
    <div className="actions">
      <button type="submit" disabled={busy || !valid}>{busy ? "Guardando…" : "Guardar límite"}</button>
      <button type="button" className="secondary" disabled={busy} onClick={() => { setUnlimited(loaded.value.storage.download_bandwidth_limit_bytes_per_second === null); setLimitText(loaded.value.storage.download_bandwidth_limit_bytes_per_second ?? ""); setError(null); }}>Descartar cambios</button>
    </div>
    {error && <p role="alert" className="state-unknown">{error}</p>}
  </form>;
}

type Thresholds = VersionedSettings["value"]["storage"]["low_disk_thresholds"];
const MAX_BYTES = 18_446_744_073_709_551_615n;

function bytesInputValid(value: string): boolean {
  if (!/^(0|[1-9][0-9]*)$/.test(value)) return false;
  try { return BigInt(value) <= MAX_BYTES; } catch { return false; }
}

/** Convierte un porcentaje escrito con hasta dos decimales a basis points exactos. */
export function percentToBasisPoints(value: string): number | null {
  const match = /^(0|[1-9][0-9]?)(?:[,.]([0-9]{1,2}))?$/.exec(value.trim());
  if (!match) return null;
  const whole = Number(match[1]);
  const fractional = Number((match[2] ?? "").padEnd(2, "0"));
  const basisPoints = whole * 100 + fractional;
  return basisPoints <= 10_000 ? basisPoints : null;
}

function percentLabel(basisPoints: number): string {
  return `${(basisPoints / 100).toLocaleString("es-ES", { minimumFractionDigits: 0, maximumFractionDigits: 2 })} %`;
}

export function LowDiskThresholdSettings({ client }: { client: ApiClient }) {
  const id = useId();
  const [loaded, setLoaded] = useState<VersionedSettings | null>(null);
  const [mode, setMode] = useState<"PERCENT" | "BYTES">("PERCENT");
  const [warning, setWarning] = useState("15");
  const [critical, setCritical] = useState("5");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const { notify } = useToasts();

  useEffect(() => {
    let active = true;
    setLoading(true); setError(null);
    void readSettings(client).then((current) => {
      if (!active) return;
      setLoaded(current);
      const thresholds = current.value.storage.low_disk_thresholds;
      setMode(thresholds.mode);
      if (thresholds.mode === "PERCENT") {
        setWarning((thresholds.warning_basis_points / 100).toFixed(2).replace(/\.00$/, ""));
        setCritical((thresholds.critical_basis_points / 100).toFixed(2).replace(/\.00$/, ""));
      } else {
        setWarning(thresholds.warning_bytes);
        setCritical(thresholds.critical_bytes);
      }
    }).catch((reason: unknown) => { if (active) setError(explain(reason)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [client, revision]);

  const warningPoints = mode === "PERCENT" ? percentToBasisPoints(warning) : null;
  const criticalPoints = mode === "PERCENT" ? percentToBasisPoints(critical) : null;
  const valid = mode === "PERCENT"
    ? warningPoints !== null && criticalPoints !== null && warningPoints > 0 && criticalPoints < warningPoints
    : bytesInputValid(warning) && bytesInputValid(critical) && BigInt(critical || "0") < BigInt(warning || "0");

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (!loaded || !valid) return;
    const thresholds: Thresholds = mode === "PERCENT"
      ? { mode, warning_basis_points: warningPoints!, critical_basis_points: criticalPoints! }
      : { mode, warning_bytes: warning, critical_bytes: critical };
    const value = { ...loaded.value, storage: { ...loaded.value.storage, low_disk_thresholds: thresholds } };
    setBusy(true); setError(null);
    try {
      const saved = await writeSettings(client, loaded.revision, value);
      setLoaded(saved);
      notify("info", "Los niveles de poco espacio se han guardado. LARM volverá a analizar los discos.");
      setRevision((current) => current + 1);
    } catch (reason) { setError(explain(reason)); }
    finally { setBusy(false); }
  };

  if (loading) return <section className="panel" aria-busy="true"><h2>Poco espacio en disco</h2><p>Cargando umbrales…</p></section>;
  if (!loaded) return <section className="panel"><h2>Poco espacio en disco</h2><p role="alert">{error ?? "No se pudo cargar la configuración."}</p><button type="button" onClick={() => setRevision((current) => current + 1)}>Reintentar</button></section>;

  return <form className="panel low-disk-settings" onSubmit={(event) => void save(event)}>
    <h2>Poco espacio en disco</h2>
    <p>Los avisos usan el espacio libre físico del disco, una sola vez por volumen. Los avisos críticos pausan nuevas asignaciones; no eliminan archivos ni detienen trabajos activos.</p>
    <label htmlFor={`${id}-mode`}>Unidad de los niveles</label>
    <select id={`${id}-mode`} value={mode} disabled={busy} onChange={(event) => {
      const next = event.target.value as "PERCENT" | "BYTES";
      setMode(next);
      if (next === "PERCENT") { setWarning("15"); setCritical("5"); }
      else { setWarning("16106127360"); setCritical("5368709120"); }
    }}>
      <option value="PERCENT">Porcentaje libre</option><option value="BYTES">Bytes libres exactos</option>
    </select>
    <label htmlFor={`${id}-warning`}>Aviso (warning)</label>
    <input id={`${id}-warning`} inputMode={mode === "PERCENT" ? "decimal" : "numeric"} value={warning} disabled={busy} onChange={(event) => setWarning(event.target.value)} aria-invalid={!valid} />
    <label htmlFor={`${id}-critical`}>Crítico (critical)</label>
    <input id={`${id}-critical`} inputMode={mode === "PERCENT" ? "decimal" : "numeric"} value={critical} disabled={busy} onChange={(event) => setCritical(event.target.value)} aria-invalid={!valid} />
    <p className="detail">
      {mode === "PERCENT"
        ? warningPoints !== null && criticalPoints !== null ? `Aviso por debajo de ${percentLabel(warningPoints)} libres; crítico por debajo de ${percentLabel(criticalPoints)}.` : "Usa porcentajes entre 0 y 100, con hasta dos decimales." 
        : bytesInputValid(warning) && bytesInputValid(critical) ? `Aviso por debajo de ${formatBytes(warning)} (${warning} bytes); crítico por debajo de ${formatBytes(critical)} (${critical} bytes).` : "Usa cantidades enteras de bytes dentro del límite admitido."}
    </p>
    {!valid && <p role="alert" className="state-unknown">El nivel crítico debe ser menor que el nivel de aviso.</p>}
    {error && <p role="alert" className="state-unknown">{error}</p>}
    <div className="actions"><button type="submit" disabled={busy || !valid}>{busy ? "Guardando y reevaluando…" : "Guardar niveles"}</button>
      <button type="button" className="secondary" disabled={busy} onClick={() => setRevision((current) => current + 1)}>Recargar</button></div>
  </form>;
}
