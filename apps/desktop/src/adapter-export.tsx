import { useId, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import { fingerprintLocalAdapterPackage, type AdapterPackageFingerprint, type PackageFingerprinter } from "./adapter-import";

interface AdapterExportReceipt {
  resource_id: string;
  row_version: number;
  event_id: string;
}

function explain(error: unknown): string {
  if (error instanceof ApiError && error.isOffline) return "Se perdió la conexión con el daemon de LARM.";
  if (error instanceof ApiError && error.status === 403) return "Tu cuenta no tiene permiso para exportar adaptadores.";
  return "No se pudo exportar el paquete. Comprueba la ruta y que el destino esté disponible.";
}

export function AdapterExport({ client, adapterId, adapterName, fingerprintPackage = fingerprintLocalAdapterPackage }: {
  client: ApiClient;
  adapterId: string;
  adapterName: string;
  fingerprintPackage?: PackageFingerprinter;
}) {
  const id = useId();
  const [destination, setDestination] = useState("");
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [receipt, setReceipt] = useState<AdapterExportReceipt | null>(null);
  const [fingerprint, setFingerprint] = useState<AdapterPackageFingerprint | null>(null);
  const [fingerprinting, setFingerprinting] = useState(false);
  const [fingerprintFailed, setFingerprintFailed] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const start = async () => {
    const path = destination.trim();
    if (!path || !confirmed || busy || receipt) return;
    setBusy(true); setError(null);
    try {
      const result = await client.request<AdapterExportReceipt>({
        method: "POST",
        path: "/api/v1/adapters/export",
        headers: { "Idempotency-Key": `adapter-export-${crypto.randomUUID()}` },
        body: { adapter_installation_id: adapterId, destination_path: path },
      });
      setReceipt(result);
      setFingerprinting(true);
      try { setFingerprint(await fingerprintPackage(path)); }
      catch { setFingerprintFailed(true); }
      finally { setFingerprinting(false); }
    } catch (cause) { setError(explain(cause)); }
    finally { setBusy(false); }
  };

  const reset = () => { setReceipt(null); setFingerprint(null); setFingerprinting(false); setFingerprintFailed(false); setConfirmed(false); setError(null); };

  return <section className="adapter-export" aria-labelledby={`${id}-heading`}>
    <h3 id={`${id}-heading`}>Exportar paquete</h3>
    {!receipt ? <>
      <label className="model-import-field" htmlFor={`${id}-destination`}>Guardar ZIP en
        <input id={`${id}-destination`} value={destination} onChange={(event) => { setDestination(event.target.value); setConfirmed(false); setError(null); }} placeholder="C:\\Exportaciones\\adaptador.zip" autoComplete="off" />
      </label>
      <label className="check"><input type="checkbox" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} />Confirmo exportar {adapterName} a esta ruta.</label>
      <button type="button" className="secondary" disabled={!destination.trim() || !confirmed || busy} onClick={() => void start()}>{busy ? "Exportando…" : "Exportar ZIP"}</button>
    </> : <div role="status">
      <p>Paquete exportado: <span className="mono">{destination.trim()}</span></p>
      {fingerprinting && <p>Calculando el hash exacto del ZIP exportado…</p>}
      {fingerprint && <dl className="facts">
        <dt>Identidad</dt><dd><span className="mono">{fingerprint.manifest.id}</span> · {fingerprint.manifest.version}</dd>
        <dt>SHA-256 del ZIP</dt><dd className="mono">{fingerprint.sha256}</dd>
        <dt>Tamaño exacto</dt><dd>{fingerprint.size_bytes} bytes</dd>
      </dl>}
      {fingerprintFailed && <p>El ZIP ya se exportó, pero no se pudo volver a leer para mostrar su huella local.</p>}
      <button type="button" className="secondary" disabled={fingerprinting} onClick={reset}>Exportar a otra ubicación</button>
    </div>}
    {error && <p role="alert" className="state-unknown">{error}</p>}
  </section>;
}
