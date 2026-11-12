import { useEffect, useId, useRef, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import type { StorageLocation } from "./storage";

interface Page<T> { items: T[]; next_cursor: string | null }
interface JobReceipt { job_id: string; status_url: string; state: "QUEUED" | "RUNNING" | "COMPLETED" | "FAILED" | "CANCELLED" }
interface JobState extends JobReceipt {
  progress: Record<string, unknown>;
  result: Record<string, unknown> | null;
  error: { message: string; code: string } | null;
}
interface ImportPreviewFile {
  relative_path: string;
  kind: string;
  role: string;
  required: boolean;
  size_bytes: string;
  facts: Record<string, string>;
}
interface ImportPreviewVariant {
  dir: string;
  name: string;
  format: string;
  quantization: string | null;
  architecture: string | null;
  size_bytes: string;
  files: ImportPreviewFile[];
}
interface ImportScanPreview {
  files_seen: number;
  recognized_files: number;
  bytes_seen: string;
  recognized_bytes: string;
  variants: ImportPreviewVariant[];
  ignored: Array<{ relative_path: string; reason: string }>;
  skipped: Array<{ relative_path: string; reason: string }>;
}
interface ReviewedPreview { sourcePath: string; scan: ImportScanPreview }

function explain(error: unknown): string {
  if (error instanceof ApiError && error.isOffline) return "Se perdió la conexión con LARM. Comprueba la conexión e inténtalo de nuevo.";
  if (error instanceof ApiError && error.status === 403) return "Tu cuenta no tiene permiso para revisar o importar modelos.";
  return "No se pudo completar la operación. Comprueba la ruta y que el disco esté disponible.";
}

function resultSummary(result: Record<string, unknown> | null): string {
  if (!result) return "LARM terminó la importación; consulta el registro del trabajo para ver su resultado.";
  const variants = Array.isArray(result.variants) ? result.variants.length : null;
  const files = typeof result.hashed === "number" ? result.hashed : null;
  const ignored = Array.isArray(result.ignored) ? result.ignored.length : null;
  const skipped = Array.isArray(result.skipped) ? result.skipped.length : null;
  const details = [variants === null ? null : `${variants} variante${variants === 1 ? "" : "s"} detectada${variants === 1 ? "" : "s"}`, files === null ? null : `${files} archivo${files === 1 ? "" : "s"} verificado${files === 1 ? "" : "s"}`, ignored === null ? null : `${ignored} ignorado${ignored === 1 ? "" : "s"}`, skipped === null ? null : `${skipped} omitido${skipped === 1 ? "" : "s"}`].filter(Boolean);
  return details.length ? details.join(" · ") : "La importación terminó; consulta el registro del trabajo para ver su resultado.";
}

export function ModelImport({ client, onCompleted }: { client: ApiClient; onCompleted: () => void }) {
  const id = useId();
  const [locations, setLocations] = useState<StorageLocation[]>([]);
  const [loadingLocations, setLoadingLocations] = useState(true);
  const [sourcePath, setSourcePath] = useState("");
  const [targetId, setTargetId] = useState("");
  const [mode, setMode] = useState<"ADOPT" | "COPY">("ADOPT");
  const [format, setFormat] = useState("");
  const [licenseId, setLicenseId] = useState("");
  const [preview, setPreview] = useState<ReviewedPreview | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [receipt, setReceipt] = useState<JobReceipt | null>(null);
  const [job, setJob] = useState<JobState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const notified = useRef(false);
  const scanGeneration = useRef(0);
  const targets = locations.filter((location) => location.state === "ONLINE");

  useEffect(() => {
    let active = true;
    void client.get<Page<StorageLocation>>("/api/v1/storage-locations?limit=200").then((page) => {
      if (!active) return;
      setLocations(page.items);
      setTargetId(page.items.find((item) => item.state === "ONLINE")?.id ?? "");
    }).catch((e: unknown) => { if (active) setError(explain(e)); })
      .finally(() => { if (active) setLoadingLocations(false); });
    return () => { active = false; };
  }, [client]);

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
      } catch (e) {
        if (!active) return;
        setError(explain(e));
      }
      if (active) timer = window.setTimeout(() => void poll(), 1000);
    };
    void poll();
    return () => { active = false; if (timer !== undefined) window.clearTimeout(timer); };
  }, [client, onCompleted, receipt]);

  const resetConfirmation = () => setConfirmed(false);
  const changeSource = (value: string) => {
    scanGeneration.current += 1;
    setSourcePath(value);
    setPreview(null);
    setPreviewing(false);
    setFormat("");
    setConfirmed(false);
    setError(null);
  };

  const scan = async () => {
    const source = sourcePath.trim();
    if (!source || previewing || receipt) return;
    const generation = ++scanGeneration.current;
    setPreview(null);
    setPreviewing(true);
    setConfirmed(false);
    setError(null);
    try {
      const result = await client.request<ImportScanPreview>({
        method: "POST", path: "/api/v1/models/import/preview",
        body: { source_path: source },
      });
      if (scanGeneration.current === generation) {
        setPreview({ sourcePath: source, scan: result });
        setFormat((current) => result.variants.some((variant) => variant.format.toLowerCase() === current.toLowerCase()) ? current : "");
      }
    } catch (e) {
      if (scanGeneration.current === generation) setError(explain(e));
    } finally {
      if (scanGeneration.current === generation) setPreviewing(false);
    }
  };

  const start = async () => {
    const source = sourcePath.trim();
    const currentPreview = preview?.sourcePath === source ? preview.scan : null;
    const hasSelectedVariants = currentPreview?.variants.some((variant) => !format || variant.format.toLowerCase() === format.toLowerCase()) ?? false;
    if (!source || !targetId || !currentPreview || !hasSelectedVariants || !confirmed || receipt) return;
    setError(null);
    try {
      const started = await client.request<JobReceipt>({
        method: "POST", path: "/api/v1/models/import",
        headers: { "Idempotency-Key": `model-import-${crypto.randomUUID()}` },
        body: { source_path: source, storage_location_id: targetId, mode, format: format.trim() || null, license_id: licenseId.trim() || null },
      });
      setReceipt(started); setJob(null);
    } catch (e) { setError(explain(e)); }
  };

  const selectedTarget = targets.find((location) => location.id === targetId);
  const activePreview = preview?.sourcePath === sourcePath.trim() ? preview.scan : null;
  const availableFormats = [...new Set(activePreview?.variants.map((variant) => variant.format) ?? [])].sort((left, right) => left.localeCompare(right));
  const selectedVariants = activePreview?.variants.filter((variant) => !format || variant.format.toLowerCase() === format.toLowerCase()) ?? [];
  const variantCount = selectedVariants.length;
  const confirmationText = `Confirmo iniciar la importación de ${variantCount} variante${variantCount === 1 ? "" : "s"} en el destino elegido.`;

  return <section className="panel model-import" aria-labelledby={`${id}-heading`}>
    <div className="model-section-heading"><div><p className="eyebrow">Importación local</p><h2 id={`${id}-heading`}>Importar carpeta</h2></div></div>
    {!receipt ? <>
      <p>Primero puedes revisar la carpeta en modo de solo lectura. LARM no registrará ni copiará archivos hasta que confirmes la importación.</p>
      <p>Al confirmar, LARM creará una fuente local para esta carpeta o reutilizará la existente y conservará ese vínculo en cada modelo importado.</p>
      <label className="model-import-field" htmlFor={`${id}-source`}>Ruta de la carpeta local
        <input id={`${id}-source`} value={sourcePath} onChange={(event) => changeSource(event.target.value)} placeholder="C:\Modelos\mi-modelo" autoComplete="off" />
      </label>
      <label className="model-import-field" htmlFor={`${id}-target`}>Ubicación de LARM
        <select id={`${id}-target`} value={targetId} disabled={loadingLocations} onChange={(event) => { setTargetId(event.target.value); resetConfirmation(); }}>
          <option value="">Elige una ubicación online</option>
          {targets.map((location) => <option key={location.id} value={location.id}>{location.name}{location.drive_letter ? ` · ${location.drive_letter}` : ""}</option>)}
        </select>
      </label>
      {!loadingLocations && locations.length > 0 && !targets.length && <p role="alert" className="state-unknown">No hay ubicaciones online disponibles.</p>}
      <label className="model-import-field" htmlFor={`${id}-mode`}>Cómo importar
        <select id={`${id}-mode`} value={mode} onChange={(event) => { setMode(event.target.value as "ADOPT" | "COPY"); resetConfirmation(); }}>
          <option value="ADOPT">Adoptar (los archivos permanecen en su ubicación)</option>
          <option value="COPY">Copiar a la ubicación gestionada por LARM</option>
        </select>
      </label>
      {mode === "ADOPT" && <p className="detail">Adoptar registra los archivos sin moverlos ni modificarlos. La carpeta debe estar dentro de la ubicación elegida.</p>}
      {mode === "COPY" && <p className="detail">Copiar calcula y verifica hashes y guarda una copia administrada por LARM en el destino.</p>}
      <div className="model-import-optional-fields">
        <label htmlFor={`${id}-license`}>Licencia (opcional)<input id={`${id}-license`} value={licenseId} onChange={(event) => { setLicenseId(event.target.value); resetConfirmation(); }} placeholder="Identificador de licencia" /></label>
      </div>

      <button type="button" disabled={!sourcePath.trim() || previewing} onClick={() => void scan()}>{previewing ? "Revisando carpeta…" : activePreview ? "Volver a explorar" : "Previsualizar carpeta"}</button>
      {activePreview && <section className="import-review" aria-labelledby={`${id}-preview-heading`}>
        <h3 id={`${id}-preview-heading`}>Vista previa de solo lectura</h3>
        <dl className="facts">
          <dt>Variantes detectadas</dt><dd>{activePreview.variants.length}</dd>
          <dt>Variantes seleccionadas</dt><dd>{variantCount}</dd>
          <dt>Archivos legibles</dt><dd>{activePreview.files_seen} · {activePreview.bytes_seen} bytes medidos</dd>
          <dt>Archivos reconocidos</dt><dd>{activePreview.recognized_files} · {activePreview.recognized_bytes} bytes únicos</dd>
          <dt>Ignorados</dt><dd>{activePreview.ignored.length}</dd>
          <dt>Omitidos</dt><dd>{activePreview.skipped.length} · tamaño no incluido</dd>
        </dl>
        {activePreview.variants.length > 0 && <label className="model-import-field" htmlFor={`${id}-format`}>Formato a importar (opcional)
          <select id={`${id}-format`} value={format} onChange={(event) => { setFormat(event.target.value); resetConfirmation(); }}>
            <option value="">Todos los formatos</option>
            {availableFormats.map((availableFormat) => <option key={availableFormat} value={availableFormat}>{availableFormat}</option>)}
          </select>
        </label>}
        {activePreview.variants.length === 0 ? <p role="status">No se detectaron variantes importables en esta carpeta.</p> : <ul className="import-preview-list">
          {activePreview.variants.map((variant) => <li key={`${variant.dir}/${variant.name}`}>
            <strong>{variant.name}</strong> · {variant.format} · {variant.size_bytes} bytes
            <span>{!format || variant.format.toLowerCase() === format.toLowerCase() ? " · Se importará" : " · Excluida por el filtro"}</span>
            {variant.quantization && <span> · {variant.quantization}</span>}
            {variant.architecture && <span> · {variant.architecture}</span>}
            <ul>{variant.files.map((file) => <li key={file.relative_path}>
              <span className="mono">{file.relative_path}</span> · {file.size_bytes} bytes · {file.required ? "requerido" : "opcional"}
            </li>)}</ul>
          </li>)}
        </ul>}
        {activePreview.ignored.length > 0 && <details><summary>Archivos sin modelo reconocido ({activePreview.ignored.length})</summary><ul>
          {activePreview.ignored.map((file) => <li key={file.relative_path}><span className="mono">{file.relative_path}</span> · {file.reason}</li>)}
        </ul></details>}
        {activePreview.skipped.length > 0 && <details><summary>Elementos omitidos ({activePreview.skipped.length})</summary><ul>
          {activePreview.skipped.map((file) => <li key={file.relative_path}><span className="mono">{file.relative_path}</span> · {file.reason}</li>)}
        </ul></details>}
        <h4>Acción que se confirmará</h4>
        <dl className="facts"><dt>Carpeta</dt><dd className="mono">{sourcePath.trim()}</dd><dt>Destino</dt><dd>{selectedTarget?.name ?? "Ubicación no disponible"}</dd><dt>Acción</dt><dd>{mode === "ADOPT" ? "Registrar sin mover archivos" : "Copiar y registrar como gestionado por LARM"}</dd><dt>Formato</dt><dd>{format || "Todos"}</dd><dt>Licencia declarada</dt><dd>{licenseId.trim() || "Sin especificar"}</dd></dl>
        <p className="detail">Al iniciar, LARM volverá a recorrer y verificar la carpeta. El escaneo previo no fija ni autoriza esos cambios.</p>
        <label className="check"><input type="checkbox" checked={confirmed} disabled={variantCount === 0 || !targetId} onChange={(event) => setConfirmed(event.target.checked)} />{confirmationText}</label>
        <button type="button" disabled={!confirmed || previewing || !targetId} onClick={() => void start()}>Iniciar importación</button>
      </section>}
      {error && <p role="alert" className="state-unknown">{error}</p>}
    </> : <section className="import-progress" aria-live="polite">
      <h3>{job?.state === "COMPLETED" ? "Importación completada" : job?.state === "FAILED" ? "Importación fallida" : job?.state === "CANCELLED" ? "Importación cancelada" : "Importación en curso"}</h3>
      <p>Trabajo {receipt.job_id} · estado {job?.state ?? receipt.state}.</p>
      {job?.state === "COMPLETED" && <p role="status" className="state-verified">{resultSummary(job.result)}</p>}
      {job?.state === "FAILED" && <p role="alert" className="state-unknown">{job.error?.message ?? "El trabajo falló. Revisa el detalle del trabajo antes de volver a intentarlo."}</p>}
      {job?.state === "CANCELLED" && <p role="status">El trabajo se canceló. Comprueba qué archivos llegaron a registrarse antes de repetirlo.</p>}
      {error && <p role="alert" className="state-unknown">{error} Se volverá a consultar el trabajo.</p>}
    </section>}
  </section>;
}
