import { useCallback, useEffect, useRef, useState } from "react";
import { ApiClient, ApiError } from "./api/client";

interface RegistryCheckpoint {
  registry_id: string;
  registry_url: string;
  sequence: number;
  payload_sha256: string;
  issued_at: string;
  expires_at: string;
  accepted_at: string;
  envelope: { payload?: { packages?: unknown[] } };
}

interface RegistryRevocation {
  registry_id: string;
  target_type: "PACKAGE_SHA256" | "PUBLISHER_KEY";
  target: string;
  revoked_at: string;
  reason_code: string;
  source_sequence: number;
}

interface RegistryStatus {
  enabled: boolean;
  registry_url: string | null;
  checkpoint: RegistryCheckpoint | null;
  revocations: RegistryRevocation[];
}

interface RegistrySyncReceipt {
  sequence: number;
  package_count: number;
  revocation_count: number;
  replay: boolean;
}

function explain(error: unknown): string {
  if (error instanceof ApiError && error.isOffline) return "Se perdió la conexión con el daemon de LARM.";
  if (error instanceof ApiError && error.status === 403) return "Tu cuenta no tiene permiso para consultar o sincronizar el registro.";
  return "No se pudo consultar el registro. Comprueba la conexión y vuelve a intentarlo.";
}

function dateLabel(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "Fecha desconocida" : date.toLocaleString("es-ES");
}

function revocationReason(value: string): string {
  const labels: Record<string, string> = {
    COMPROMISED_KEY: "Clave comprometida",
    MALICIOUS_PACKAGE: "Paquete malicioso",
    INVALID_PROVENANCE: "Procedencia no válida",
    OPERATOR_REQUEST: "Solicitud del operador",
    OTHER: "Otro motivo",
  };
  return labels[value] ?? "Motivo desconocido";
}

export function AdapterRegistryPanel({ client }: { client: ApiClient }) {
  const [status, setStatus] = useState<RegistryStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [syncing, setSyncing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [receipt, setReceipt] = useState<RegistrySyncReceipt | null>(null);
  const syncKey = useRef<string | null>(null);
  const revocations = status?.revocations ?? [];

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setStatus(await client.get<RegistryStatus>("/api/v1/adapters/registry"));
    } catch (cause) {
      setError(explain(cause));
    } finally {
      setLoading(false);
    }
  }, [client]);

  useEffect(() => { void load(); }, [load]);

  const synchronize = async () => {
    if (!status?.enabled || syncing) return;
    setSyncing(true);
    setError(null);
    setReceipt(null);
    syncKey.current ??= `adapter-registry-sync-${crypto.randomUUID()}`;
    try {
      const result = await client.request<RegistrySyncReceipt>({
        method: "POST",
        path: "/api/v1/adapters/registry/sync",
        headers: { "Idempotency-Key": syncKey.current },
        body: {},
      });
      syncKey.current = null;
      setReceipt(result);
      setStatus(await client.get<RegistryStatus>("/api/v1/adapters/registry"));
    } catch (cause) {
      setError(explain(cause));
    } finally {
      setSyncing(false);
    }
  };

  return <section className="panel stack adapter-registry" aria-labelledby="adapter-registry-heading">
    <div className="model-section-heading">
      <div><p className="eyebrow">Fuentes firmadas</p><h2 id="adapter-registry-heading">Registro remoto</h2></div>
      <div className="actions">
        <button type="button" className="secondary" disabled={loading || syncing} onClick={() => void load()}>Actualizar estado</button>
        <button type="button" disabled={!status?.enabled || loading || syncing} onClick={() => void synchronize()}>
          {syncing ? "Sincronizando…" : "Sincronizar registro"}
        </button>
      </div>
    </div>
    <p>El índice firmado aporta metadatos y revocaciones. La firma del registro no concede confianza ni cualificación al adaptador.</p>
    {loading && <p role="status">Consultando estado del registro…</p>}
    {error && <div role="alert" className="state-unknown"><p>{error}</p><button type="button" className="secondary" disabled={loading} onClick={() => void load()}>Reintentar</button></div>}
    {!loading && status && <>
      <dl className="facts">
        <dt>Estado</dt><dd>{status.enabled ? "Configurado" : "Deshabilitado"}</dd>
        {status.registry_url && <><dt>Dirección</dt><dd className="mono">{status.registry_url}</dd></>}
        {status.checkpoint && <>
          <dt>Registro</dt><dd>{status.checkpoint.registry_id}</dd>
          <dt>Secuencia</dt><dd>{status.checkpoint.sequence}</dd>
          <dt>Última verificación</dt><dd>{dateLabel(status.checkpoint.accepted_at)}</dd>
          <dt>Vigencia hasta</dt><dd>{dateLabel(status.checkpoint.expires_at)}</dd>
          <dt>Paquetes publicados</dt><dd>{status.checkpoint.envelope.payload?.packages?.length ?? "Desconocido"}</dd>
          <dt>SHA-256 del índice</dt><dd className="mono">{status.checkpoint.payload_sha256}</dd>
        </>}
      </dl>
      {!status.enabled && <p>Configura <code>security.registry_url</code>, la clave pública fijada y los orígenes permitidos en la configuración del daemon para activar la consulta.</p>}
      {status.enabled && !status.checkpoint && <p>Aún no hay un checkpoint confirmado. Sincroniza para consultar el índice firmado.</p>}
      {receipt && <p role="status">{receipt.replay ? "El índice ya estaba confirmado." : `Secuencia ${receipt.sequence} aceptada.`} {receipt.package_count} paquetes · {receipt.revocation_count} revocaciones del índice.</p>}
      <section className="registry-revocations" aria-label="Revocaciones persistidas">
        <h3>Revocaciones persistidas ({revocations.length})</h3>
        {revocations.length ? <ul>
          {revocations.map((revocation) => <li key={`${revocation.registry_id}-${revocation.target_type}-${revocation.target}`}>
            <strong>{revocationReason(revocation.reason_code)}</strong>
            <span> · {revocation.target_type === "PACKAGE_SHA256" ? "Paquete" : "Clave del publicador"} · secuencia {revocation.source_sequence}</span>
            <div className="mono">{revocation.target}</div>
            <small>Revocada: {dateLabel(revocation.revoked_at)}</small>
          </li>)}
        </ul> : <p>No hay revocaciones registradas.</p>}
      </section>
    </>}
  </section>;
}
