import { useEffect, useId, useRef, useState } from "react";
import { ApiClient, ApiError } from "./api/client";

export interface AdapterPackageFingerprint {
  size_bytes: string;
  sha256: string;
  manifest: {
    id: string;
    version: string;
    family: string;
    protocol_version: string;
    publisher: string;
    signature_declared: boolean;
    permissions: {
      read_roots: string[];
      write_roots: string[];
      network_origins: string[];
      process_allowlist: string[];
      secret_refs: string[];
    };
  };
}
interface JobReceipt {
  job_id: string;
  status_url: string;
  state: "QUEUED" | "RUNNING" | "COMPLETED" | "FAILED" | "CANCELLED";
}
interface JobState extends JobReceipt {
  progress?: Record<string, unknown>;
  result?: Record<string, unknown> | null;
  error?: { code?: string; message?: string } | null;
}
export type PackageFingerprinter = (path: string) => Promise<AdapterPackageFingerprint>;

export async function fingerprintLocalAdapterPackage(path: string): Promise<AdapterPackageFingerprint> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<AdapterPackageFingerprint>("inspect_adapter_package", { packagePath: path });
}

function explain(error: unknown): string {
  if (error instanceof ApiError && error.isOffline) return "Se perdió la conexión con el daemon de LARM.";
  if (error instanceof ApiError && error.status === 403) return "Tu cuenta no tiene permiso para importar adaptadores.";
  return "No se pudo revisar o importar el paquete. Comprueba la ruta, el archivo ZIP y que el disco esté disponible.";
}

function exactSize(bytes: string): string {
  try {
    const value = BigInt(bytes);
    const formatted = new Intl.NumberFormat("es-ES").format(value);
    const mib = Number(value) / (1024 * 1024);
    return `${formatted} bytes (${mib.toLocaleString("es-ES", { maximumFractionDigits: 2 })} MiB)`;
  } catch {
    return `${bytes} bytes`;
  }
}

const JOB_LABEL: Record<JobReceipt["state"], string> = {
  QUEUED: "En cola", RUNNING: "Importando", COMPLETED: "Completada", FAILED: "Fallida", CANCELLED: "Cancelada",
};

export function AdapterImport({ client, onCompleted, fingerprintPackage = fingerprintLocalAdapterPackage }: {
  client: ApiClient;
  onCompleted: () => void;
  fingerprintPackage?: PackageFingerprinter;
}) {
  const id = useId();
  const [packagePath, setPackagePath] = useState("");
  const [fingerprint, setFingerprint] = useState<{ path: string; value: AdapterPackageFingerprint } | null>(null);
  const [inspecting, setInspecting] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [receipt, setReceipt] = useState<JobReceipt | null>(null);
  const [job, setJob] = useState<JobState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const notified = useRef(false);
  const generation = useRef(0);
  const path = packagePath.trim();
  const activeFingerprint = fingerprint?.path === path ? fingerprint.value : null;

  useEffect(() => {
    if (!receipt) return;
    let active = true;
    let timer: number | undefined;
    const poll = async () => {
      try {
        const current = await client.get<JobState>(`/api/v1/jobs/${encodeURIComponent(receipt.job_id)}`);
        if (!active) return;
        setJob(current);
        if (current.state === "COMPLETED") {
          if (!notified.current) { notified.current = true; onCompleted(); }
          return;
        }
        if (current.state === "FAILED" || current.state === "CANCELLED") return;
      } catch (cause) {
        if (!active) return;
        setError(explain(cause));
      }
      if (active) timer = window.setTimeout(() => void poll(), 1000);
    };
    void poll();
    return () => { active = false; if (timer !== undefined) window.clearTimeout(timer); };
  }, [client, onCompleted, receipt]);

  const inspect = async () => {
    if (!path || inspecting || receipt) return;
    const current = ++generation.current;
    setInspecting(true); setFingerprint(null); setConfirmed(false); setError(null);
    try {
      const value = await fingerprintPackage(path);
      if (generation.current === current) setFingerprint({ path, value });
    } catch (cause) {
      if (generation.current === current) setError(explain(cause));
    } finally {
      if (generation.current === current) setInspecting(false);
    }
  };

  const start = async () => {
    if (!path || !activeFingerprint || !confirmed || receipt) return;
    setError(null);
    try {
      const started = await client.request<JobReceipt>({
        method: "POST",
        path: "/api/v1/adapters/import",
        headers: { "Idempotency-Key": `adapter-import-${crypto.randomUUID()}` },
        body: { package_path: path, expected_hash: activeFingerprint.sha256, approval_id: null },
      });
      setReceipt(started);
      setJob(started);
    } catch (cause) { setError(explain(cause)); }
  };

  const reset = () => {
    generation.current += 1;
    setPackagePath(""); setFingerprint(null); setInspecting(false); setConfirmed(false);
    setReceipt(null); setJob(null); setError(null); notified.current = false;
  };

  return <section className="panel adapter-import" aria-labelledby={`${id}-heading`}>
    <div className="model-section-heading"><div><p className="eyebrow">Paquete local</p><h2 id={`${id}-heading`}>Importar adaptador</h2></div></div>
    <p>Revisa primero el tamaño y la huella exactos del ZIP. LARM vuelve a verificar el archivo antes de extraerlo; la firma y los permisos se evalúan aparte y un paquete sin firma queda como no verificado.</p>
    {!receipt ? <>
      <label className="model-import-field" htmlFor={`${id}-path`}>Ruta del paquete ZIP
        <input id={`${id}-path`} value={packagePath} onChange={(event) => { generation.current += 1; setPackagePath(event.target.value); setFingerprint(null); setConfirmed(false); setError(null); }} placeholder="C:\\Paquetes\\adaptador.zip" autoComplete="off" />
      </label>
      <button type="button" className="secondary" disabled={!path || inspecting} onClick={() => void inspect()}>{inspecting ? "Calculando huella…" : activeFingerprint ? "Volver a calcular" : "Revisar paquete"}</button>
      {activeFingerprint && <section className="import-review" aria-label="Revisión del paquete">
        <h3>Revisión local</h3>
        <dl className="facts">
          <dt>Identidad</dt><dd><span className="mono">{activeFingerprint.manifest.id}</span> · {activeFingerprint.manifest.version}</dd>
          <dt>Familia</dt><dd>{activeFingerprint.manifest.family}</dd>
          <dt>Publicador</dt><dd>{activeFingerprint.manifest.publisher}</dd>
          <dt>Tamaño</dt><dd><span className="size-exact">Exacto</span> · {exactSize(activeFingerprint.size_bytes)}</dd>
          <dt>SHA-256 del ZIP</dt><dd className="mono">{activeFingerprint.sha256}</dd>
          <dt>Protocolo</dt><dd>{activeFingerprint.manifest.protocol_version}</dd>
          <dt>Firma</dt><dd>{activeFingerprint.manifest.signature_declared ? "Declarada; pendiente de validación local." : "Sin firma declarada."}</dd>
          <dt>Confianza</dt><dd>Pendiente de evaluación local al importar; la firma no concede cualificación ni confianza automática.</dd>
        </dl>
        <h4>Permisos declarados en el manifiesto</h4>
        <dl className="facts">
          <dt>Lectura</dt><dd>{activeFingerprint.manifest.permissions.read_roots.length ? activeFingerprint.manifest.permissions.read_roots.join(", ") : "Ninguno"}</dd>
          <dt>Escritura</dt><dd>{activeFingerprint.manifest.permissions.write_roots.length ? activeFingerprint.manifest.permissions.write_roots.join(", ") : "Ninguno"}</dd>
          <dt>Orígenes de red</dt><dd>{activeFingerprint.manifest.permissions.network_origins.length ? activeFingerprint.manifest.permissions.network_origins.join(", ") : "Ninguno"}</dd>
          <dt>Procesos</dt><dd>{activeFingerprint.manifest.permissions.process_allowlist.length ? activeFingerprint.manifest.permissions.process_allowlist.join(", ") : "Ninguno"}</dd>
          <dt>Credenciales</dt><dd>{activeFingerprint.manifest.permissions.secret_refs.length ? `${activeFingerprint.manifest.permissions.secret_refs.length} referencia${activeFingerprint.manifest.permissions.secret_refs.length === 1 ? "" : "s"} declarada${activeFingerprint.manifest.permissions.secret_refs.length === 1 ? "" : "s"} (los valores no se muestran)` : "Ninguna"}</dd>
        </dl>
        <label className="check"><input type="checkbox" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} />Confirmo iniciar la importación de este paquete.</label>
        <button type="button" disabled={!confirmed} onClick={() => void start()}>Importar adaptador</button>
      </section>}
    </> : <section className="import-review" aria-live="polite" aria-label="Estado de importación">
      <h3>{job ? JOB_LABEL[job.state] : "En cola"}</h3>
      {job?.state === "COMPLETED" && <p>Importación completada. La lista de adaptadores se ha actualizado.</p>}
      {job?.state === "FAILED" && <p role="alert">La importación falló. Revisa el paquete y los registros del trabajo.</p>}
      {job?.state === "CANCELLED" && <p>La importación se canceló.</p>}
      {(job?.state === "QUEUED" || job?.state === "RUNNING") && <p role="status">LARM verifica, extrae y publica el paquete de forma supervisada…</p>}
      {(job?.state === "COMPLETED" || job?.state === "FAILED" || job?.state === "CANCELLED") && <button type="button" className="secondary" onClick={reset}>Importar otro paquete</button>}
    </section>}
    {error && <p role="alert" className="state-unknown">{error}</p>}
  </section>;
}
