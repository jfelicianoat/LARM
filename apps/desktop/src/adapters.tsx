import { useCallback, useEffect, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import type { Page } from "./api/runtimes";
import { AdapterImport } from "./adapter-import";
import { AdapterExport } from "./adapter-export";
import { AdapterRegistryPanel } from "./adapter-registry";

type Trust = "UNTRUSTED" | "VERIFIED" | "REVOKED" | string;
interface AdapterManifest {
  protocol_version?: unknown;
  signature?: unknown;
  permissions?: unknown;
}
export interface AdapterInstallation {
  id: string;
  adapter_id: string;
  version: string;
  family: string;
  package_hash: string;
  publisher: string;
  trust: Trust;
  manifest: AdapterManifest;
  state: string;
  row_version: number;
  metadata: Record<string, unknown>;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === "string") : [];
}

function trustLabel(value: Trust): string {
  if (value === "VERIFIED") return "Verificada";
  if (value === "REVOKED") return "Revocada";
  if (value === "UNTRUSTED") return "No verificada";
  return "Desconocida";
}

function stateLabel(value: string): string {
  const labels: Record<string, string> = {
    STAGED: "En revisión", VERIFIED: "Verificada", INSTALLED: "Instalada",
    VALIDATED: "Validada", RETIRED: "Retirada", FAILED: "Fallida",
  };
  return labels[value] ?? "Desconocido";
}

function protocolLabel(value: unknown): string {
  if (typeof value !== "string" || value.length === 0) return "Desconocida";
  return value === "1.0" ? `${value} · compatible` : `${value} · incompatible`;
}

function signatureLabel(manifest: AdapterManifest, trust: Trust): string {
  if (manifest.signature === null) return trust === "VERIFIED" ? "Sin firma declarada · confianza verificada" : "Sin firma";
  if (!record(manifest.signature)) return "Desconocida";
  if (trust === "REVOKED") return "Firma revocada";
  if (trust === "VERIFIED") return "Verificada por LARM";
  return "Firma declarada · pendiente de verificación";
}

function explain(error: unknown): string {
  if (!(error instanceof ApiError)) return "No se pudo cargar el catálogo. Vuelve a intentarlo.";
  if (error.isOffline) return "Se perdió la conexión con el daemon de LARM.";
  if (error.status === 403) return "Tu cuenta no tiene permiso para consultar los adaptadores.";
  return "LARM no pudo cargar los adaptadores. Puedes volver a intentarlo.";
}

function PermissionList({ title, values }: { title: string; values: string[] }) {
  return <div>
    <dt>{title}</dt>
    <dd>{values.length ? <ul>{values.map((value, index) => <li key={`${value}-${index}`} className="mono">{value}</li>)}</ul> : "Ninguno declarado"}</dd>
  </div>;
}

function AdapterDetail({ adapter }: { adapter: AdapterInstallation }) {
  const protocol = adapter.manifest.protocol_version;
  const incompatible = typeof protocol === "string" && protocol !== "1.0";
  const signature = signatureLabel(adapter.manifest, adapter.trust);
  const permissions = record(adapter.manifest.permissions) ? adapter.manifest.permissions : {};
  const credentialRefs = strings(permissions.secret_refs);
  return <section className="panel stack" aria-label={`Detalle de ${adapter.adapter_id}`}>
    <h3>Detalle del adaptador</h3>
    {incompatible && <p role="alert" className="notice">Protocolo incompatible: LARM admite la versión 1.0 y este paquete declara {String(protocol)}. No se ofrecerá como conector de fuentes.</p>}
    {signature === "Sin firma" && <p role="alert" className="notice">Paquete sin firma. Revisa el origen y los permisos antes de confiar en él.</p>}
    <dl className="facts">
      <dt>Publicador</dt><dd>{adapter.publisher || "Desconocido"}</dd>
      <dt>Firma</dt><dd>{signature}</dd>
      <dt>Confianza</dt><dd>{trustLabel(adapter.trust)}</dd>
      <dt>Estado</dt><dd>{stateLabel(adapter.state)}</dd>
      <dt>Versión del protocolo</dt><dd>{protocolLabel(protocol)}</dd>
      <dt>SHA-256 del paquete</dt><dd className="mono">{adapter.package_hash || "Desconocido"}</dd>
    </dl>
    <h4>Permisos declarados</h4>
    <dl className="facts permissions">
      <PermissionList title="Lectura de carpetas" values={strings(permissions.read_roots)} />
      <PermissionList title="Escritura de carpetas" values={strings(permissions.write_roots)} />
      <PermissionList title="Orígenes de red" values={strings(permissions.network_origins)} />
      <PermissionList title="Procesos permitidos" values={strings(permissions.process_allowlist)} />
      <dt>Referencias de credenciales</dt><dd>{credentialRefs.length ? `${credentialRefs.length} declarada${credentialRefs.length === 1 ? "" : "s"} (los valores no se muestran)` : "Ninguna declarada"}</dd>
    </dl>
  </section>;
}

export function AdaptersPage({ client }: { client: ApiClient }) {
  const [items, setItems] = useState<AdapterInstallation[]>([]);
  const [selected, setSelected] = useState<AdapterInstallation | null>(null);
  const [detailTarget, setDetailTarget] = useState<AdapterInstallation | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [detailLoading, setDetailLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [detailError, setDetailError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const refreshAfterImport = useCallback(() => setRevision((value) => value + 1), []);

  const read = useCallback(async (next: string | null) => {
    const params = new URLSearchParams({ limit: "200" });
    if (next) params.set("cursor", next);
    return client.get<Page<AdapterInstallation>>(`/api/v1/adapters?${params}`);
  }, [client]);

  useEffect(() => {
    let active = true;
    setLoading(true); setError(null); setItems([]); setCursor(null);
    void read(null).then((page) => {
      if (active) { setItems(page.items); setCursor(page.next_cursor); }
    }).catch((cause: unknown) => { if (active) setError(explain(cause)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [read, revision]);

  const loadDetail = async (item: AdapterInstallation) => {
    setSelected(null); setDetailTarget(item); setDetailLoading(true); setDetailError(null);
    try {
      const detail = await client.get<AdapterInstallation>(`/api/v1/adapters/${item.id}`);
      setSelected(detail);
    } catch (cause) { setDetailError(explain(cause)); }
    finally { setDetailLoading(false); }
  };

  const loadMore = async () => {
    if (!cursor || loading) return;
    setLoading(true); setError(null);
    try {
      const page = await read(cursor);
      setItems((current) => [...current, ...page.items]); setCursor(page.next_cursor);
    } catch (cause) { setError(explain(cause)); }
    finally { setLoading(false); }
  };

  return <div className="stack">
    <p>Consulta el publicador, la firma, la confianza, el protocolo y los permisos de cada adaptador instalado.</p>
    <div className="actions">
      <button type="button" className="secondary" disabled={loading} onClick={() => setRevision((value) => value + 1)}>Actualizar adaptadores</button>
    </div>
    <AdapterRegistryPanel client={client} />
    <AdapterImport client={client} onCompleted={refreshAfterImport} />
    {loading && <p role="status">Cargando adaptadores…</p>}
    {error && <div role="alert" className="state-unknown"><p>{error}</p><button type="button" className="secondary" onClick={() => setRevision((value) => value + 1)}>Reintentar</button></div>}
    {!loading && !error && !items.length && <p>Todavía no hay adaptadores instalados.</p>}
    {items.map((item) => <article className="device" key={item.id} aria-label={`Adaptador ${item.adapter_id}`}>
      <h2>{item.adapter_id}</h2>
      <dl className="facts">
        <dt>Familia</dt><dd>{item.family}</dd>
        <dt>Versión</dt><dd>{item.version}</dd>
        <dt>Publicador</dt><dd>{item.publisher || "Desconocido"}</dd>
        <dt>Estado</dt><dd>{stateLabel(item.state)}</dd>
        <dt>Confianza</dt><dd>{trustLabel(item.trust)}</dd>
      </dl>
      {item.manifest?.signature === null && item.trust !== "VERIFIED" && <p className="notice">Paquete sin firma declarada.</p>}
      <button type="button" className="secondary" disabled={detailLoading} onClick={() => void loadDetail(item)}>Ver detalles de {item.adapter_id}</button>
      <AdapterExport client={client} adapterId={item.id} adapterName={`${item.adapter_id} ${item.version}`} />
    </article>)}
    {cursor && <button type="button" className="secondary" disabled={loading} onClick={() => void loadMore()}>Cargar más adaptadores</button>}
    {detailLoading && <p role="status">Cargando detalle del adaptador…</p>}
    {detailError && <div role="alert" className="state-unknown"><p>{detailError}</p>
      {detailTarget && <button type="button" className="secondary" disabled={detailLoading} onClick={() => void loadDetail(detailTarget)}>Reintentar detalle</button>}
    </div>}
    {selected && <AdapterDetail adapter={selected} />}
  </div>;
}
