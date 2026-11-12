import { useId, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import { formatBytes } from "./hardware";

/** Candidato de búsqueda federada (subconjunto mostrado al operador). */
export interface SearchCandidate {
  model: { id: string; name: string; capabilities: string[] };
  source_id: string;
  source_priority: number;
  variant_id: string;
  author: string | null;
  capabilities: string[];
  format: string | null;
  quantization: string | null;
  architecture: string | null;
  parameter_count: string | null;
  size_bytes: string | null;
  size_confidence: string;
  licenses: string[];
  technical_fit: number | null;
  trust_score: number | null;
}

interface Page<T> { items: T[]; next_cursor: string | null }

interface SearchWarning {
  source_id: string;
  reason: string;
  limitations: string[];
}

interface SearchResponse extends Page<SearchCandidate> {
  metadata?: { search?: { warnings?: SearchWarning[] } };
}

type Compatibility = "ALL" | "COMPATIBLE" | "INCOMPATIBLE" | "UNKNOWN";
const COMPATIBILITY: Compatibility[] = ["ALL", "COMPATIBLE", "INCOMPATIBLE", "UNKNOWN"];

/** Construye el DTO `Search` completo del contrato (todos los campos required;
 *  array vacío omite el filtro, `q` vacío omite el texto). */
export function buildSearchRequest(options: {
  q: string;
  formats: string[];
  quantizations: string[];
  compatibility: Compatibility;
  limit: number;
  cursor: string | null;
}): Record<string, unknown> {
  return {
    q: options.q.trim(),
    capabilities: [],
    source_ids: [],
    compatibility: options.compatibility,
    architectures: [],
    formats: options.formats,
    quantizations: options.quantizations,
    licenses: [],
    runtime_ids: [],
    parameter_count: { minimum: null, maximum: null },
    size_bytes: { minimum: null, maximum: null },
    trust_score: { minimum: null, maximum: null },
    certification_statuses: [],
    only_executable: false,
    only_recommended: false,
    show_incompatible: true,
    cursor: options.cursor,
    limit: options.limit,
  };
}

function explain(error: unknown): string {
  if (error instanceof ApiError && error.isOffline) return "Se perdió la conexión con LARM. Comprueba la conexión e inténtalo de nuevo.";
  if (error instanceof ApiError && error.status === 403) return "Tu cuenta no tiene permiso para buscar modelos.";
  return "La búsqueda no se pudo completar. Comprueba la conexión y los filtros.";
}

/** Pantalla Descubrir: búsqueda federada de modelos en las fuentes cualificadas. */
export function DiscoverPage({ client, navigate }: { client: ApiClient; navigate: (target: string, id?: string) => void }) {
  const id = useId();
  const [q, setQ] = useState("");
  const [format, setFormat] = useState("");
  const [quantization, setQuantization] = useState("");
  const [compatibility, setCompatibility] = useState<Compatibility>("ALL");
  const [results, setResults] = useState<Page<SearchCandidate> | null>(null);
  const [warnings, setWarnings] = useState<SearchWarning[]>([]);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [searchedOnce, setSearchedOnce] = useState(false);

  const search = async (cursor: string | null) => {
    setError(null);
    setSearching(true);
    try {
      const reply = await client.request<SearchResponse>({
        method: "POST",
        path: "/api/v1/models/search",
        body: buildSearchRequest({ q, formats: format ? [format] : [], quantizations: quantization ? [quantization] : [], compatibility, limit: 50, cursor }),
      });
      setResults(cursor ? { items: [...(results?.items ?? []), ...reply.items], next_cursor: reply.next_cursor } : reply);
      setWarnings(reply.metadata?.search?.warnings ?? []);
      setSearchedOnce(true);
    } catch (e) {
      setError(explain(e));
    } finally {
      setSearching(false);
    }
  };

  const sizeLabel = (candidate: SearchCandidate): string | null => {
    if (!candidate.size_bytes) return null;
    try { return formatBytes(candidate.size_bytes); } catch { return candidate.size_bytes; }
  };

  return <section className="panel discover" aria-labelledby={`discover-heading-${id}`}>
    <h2 id={`discover-heading-${id}`}>Buscar modelos en las fuentes</h2>
    <div className="form-grid">
      <label htmlFor={`${id}-q`}>Texto</label>
      <input id={`${id}-q`} value={q} onChange={(event) => setQ(event.target.value)} placeholder="Nombre del modelo" />
      <label htmlFor={`${id}-format`}>Formato</label>
      <input id={`${id}-format`} value={format} onChange={(event) => setFormat(event.target.value)} placeholder="GGUF" />
      <label htmlFor={`${id}-quant`}>Cuantización</label>
      <input id={`${id}-quant`} value={quantization} onChange={(event) => setQuantization(event.target.value)} placeholder="Q8_0" />
      <label htmlFor={`${id}-compat`}>Compatibilidad</label>
      <select id={`${id}-compat`} value={compatibility} onChange={(event) => setCompatibility(event.target.value as Compatibility)}>
        {COMPATIBILITY.map((value) => <option key={value} value={value}>{value}</option>)}
      </select>
    </div>
    <div className="actions">
      <button type="button" onClick={() => void search(null)} disabled={searching}>
        {searching ? "Buscando…" : "Buscar"}
      </button>
    </div>
    {error && <p role="alert" className="state-unknown">{error}</p>}
    {warnings.map((warning) => <p key={warning.source_id} role="status" className="detail">
      Advertencia de la fuente {warning.source_id}: {warning.reason}
    </p>)}
    {searchedOnce && results && <>
      {results.items.length === 0 && <p>No hay resultados para esta búsqueda.</p>}
      <ul className="discover-results">
        {results.items.map((candidate) => <li key={candidate.variant_id}>
          <article className="discover-result">
            <div>
              <strong>{candidate.model.name}</strong>
              <span className="detail"> · {candidate.source_id} (prioridad {candidate.source_priority})</span>
              {candidate.author && <span className="detail"> · {candidate.author}</span>}
            </div>
            <div className="detail">
              {candidate.format ?? "Formato desconocido"}
              {candidate.quantization ? ` · ${candidate.quantization}` : ""}
              {candidate.architecture ? ` · ${candidate.architecture}` : ""}
              {sizeLabel(candidate) ? ` · ${sizeLabel(candidate)}` : ""}
              {candidate.technical_fit !== null ? ` · ajuste técnico ${candidate.technical_fit}%` : ""}
              {candidate.trust_score !== null ? ` · confianza ${candidate.trust_score}%` : ""}
            </div>
            {candidate.variant_id && <button type="button" className="link" onClick={() => navigate("mis-modelos", candidate.variant_id)}>
              Ver en Mis modelos
            </button>}
          </article>
        </li>)}
      </ul>
      {results.next_cursor && <button type="button" className="secondary" onClick={() => void search(results.next_cursor)} disabled={searching}>
        {searching ? "Cargando…" : "Cargar más resultados"}
      </button>}
    </>}
  </section>;
}
