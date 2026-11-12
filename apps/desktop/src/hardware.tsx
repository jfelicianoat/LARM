import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, type ApiClient } from "./api/client";

/** `domain.schema.json#/$defs/hardware_profile` → `snapshot.devices[]`. */
export interface Device {
  kind: "GPU" | "NPU";
  vendor: string | null;
  device_id: string | null;
  luid: string | null;
  name: string | null;
  driver_version: string | null;
  dedicated_memory_bytes: string | null;
  shared_memory_bytes: string | null;
  unified_pool_id: string | null;
  backends: string[];
  evidence_refs: string[];
}

/** Fila de `metadata.accelerators` (P011). */
export interface Accelerator {
  api: string;
  status: "VERIFIED" | "UNKNOWN" | "ABSENT";
  devices: string[];
  version: string | null;
  components: { path: string; version: string | null; sha256: string | null }[];
  evidence: string;
  reason: string | null;
}

/** `domain.schema.json#/$defs/hardware_profile` (lo que esta pantalla usa). */
export interface HardwareProfile {
  id: string;
  row_version: number;
  hardware_fingerprint: string;
  probed_at: string;
  evidence_refs: string[];
  metadata: {
    profiler?: { name: string; version: string };
    sources?: Record<string, string>;
    pagefile?: { total_bytes: string | null };
    coverage?: { probed: string[]; pending: string[] };
    accelerators?: Accelerator[];
    storage?: StorageObservation | { error: string; detail: string };
    fingerprint?: {
      version: number;
      previous_profile_id: string | null;
      previous_fingerprint: string | null;
      changes: { field: string; before: unknown; after: unknown }[];
    };
    unknown?: { field: string; reason: string; detail: string }[];
  };
  snapshot: {
    os: { edition: string | null; build: string | null; architecture: string | null };
    cpu: {
      vendor: string | null;
      model: string | null;
      physical_cores: number | null;
      logical_cores: number | null;
      instruction_sets: string[];
    };
    ram: { physical_bytes: string | null; usable_bytes: string | null; pool_id: string };
    devices: Device[];
    dependencies: { name: string; version: string | null; hash: string | null }[];
    observed_metrics: { available_ram_bytes: string | null; cpu_usage_percent: number | null };
  };
}

interface Page {
  items: HardwareProfile[];
  next_cursor: string | null;
  snapshot_at: string;
}

interface JobReceipt {
  job_id: string;
  status_url: string;
  state: "QUEUED" | "RUNNING" | "COMPLETED" | "FAILED";
}

interface JobState {
  job_id: string;
  state: JobReceipt["state"];
  error: { code: string; message: string; details?: { reason?: string } } | null;
}

const UNITS = ["bytes", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

/** Unidades IEC con un decimal (es-ES); los bytes llegan como string decimal. */
export function formatBytes(text: string): string {
  const bytes = BigInt(text);
  let unit = 0;
  let scale = 1n;
  while (unit < UNITS.length - 1 && bytes >= scale * 1024n) {
    scale *= 1024n;
    unit += 1;
  }
  if (unit === 0) return `${bytes.toString()} bytes`;
  const tenths = (bytes * 10n + scale / 2n) / scale;
  const value = Number(tenths) / 10;
  return `${value.toLocaleString("es-ES", { minimumFractionDigits: 1, maximumFractionDigits: 1 })} ${UNITS[unit]}`;
}

/** Bytes exactos con separador de miles para el detalle. */
export function exactBytes(text: string): string {
  return `${BigInt(text).toLocaleString("es-ES")} bytes`;
}

const REASONS: Record<string, string> = {
  wmi_timeout: "WMI no respondió a tiempo",
  wmi_unavailable: "WMI no está disponible",
  wmi_failed: "WMI devolvió un error",
  wmi_unparseable: "WMI devolvió un dato ilegible",
  wmi_inconsistent: "WMI dio un dato incoherente",
  registry_unreadable: "No se pudo leer el registro",
  os_api_unavailable: "Windows no lo informó",
  native_timeout: "El sondeo de dispositivos no respondió a tiempo",
  native_unavailable: "El sondeo de dispositivos no se pudo lanzar",
  native_failed: "El sondeo de dispositivos falló",
  native_unparseable: "El sondeo de dispositivos devolvió datos ilegibles",
  native_version: "El sondeo de dispositivos es de otra versión",
  tool_missing: "Falta la herramienta que comprueba el dispositivo",
  tool_timeout: "La herramienta de comprobación no respondió a tiempo",
  tool_failed: "La herramienta de comprobación falló",
  tool_unavailable: "La herramienta de comprobación no se pudo lanzar",
  no_device: "No hay ningún dispositivo de este equipo que la use",
  not_verified: "Instalada, pero LARM todavía no sabe comprobarla",
  probe_failed: "La prueba de dispositivo falló",
  not_loadable: "Instalada fuera de las carpetas seguras",
  ambiguous_binding: "Funciona, pero no se sabe en qué GPU",
};

/** Nombre legible de cada API de la spec §22. */
const API_NAMES: Record<string, string> = {
  vulkan: "Vulkan",
  cuda: "CUDA",
  rocm: "ROCm (HIP)",
  directml: "DirectML",
  winml: "Windows ML",
  opencl: "OpenCL",
  sycl: "SYCL",
  vitisai: "Vitis AI",
  migraphx: "MIGraphX",
  openvino: "OpenVINO",
  qnn: "QNN",
  tensorrt: "TensorRT",
};

const STATUS_TEXT: Record<Accelerator["status"], string> = {
  VERIFIED: "Funciona (verificada)",
  UNKNOWN: "Sin verificar",
  ABSENT: "No instalada",
};

/** Fabricantes por identificador PCI. */
const VENDORS: Record<string, string> = { "1002": "AMD", "1022": "AMD", "10de": "NVIDIA", "8086": "Intel", "1414": "Microsoft", "5143": "Qualcomm" };

const MAX_PAGES = 20;
const POLL_MS = 1000;

/** Todos los perfiles (del más antiguo al más reciente) siguiendo el cursor. */
async function loadProfiles(client: ApiClient): Promise<HardwareProfile[]> {
  const all: HardwareProfile[] = [];
  let cursor: string | null = null;
  for (let i = 0; i < MAX_PAGES; i += 1) {
    const query: string = cursor ? `?limit=200&cursor=${encodeURIComponent(cursor)}` : "?limit=200";
    const page: Page = await client.get<Page>(`/api/v1/hardware${query}`);
    all.push(...page.items);
    cursor = page.next_cursor;
    if (!cursor) break;
  }
  return all;
}

type State =
  | { kind: "loading" }
  | { kind: "error"; error: ApiError }
  | { kind: "ready"; profiles: HardwareProfile[] };

type Refresh =
  | { kind: "idle" }
  | { kind: "running"; jobId: string | null }
  | { kind: "done" }
  | { kind: "failed"; message: string };

function asApiError(e: unknown): ApiError {
  return e instanceof ApiError ? e : new ApiError("UNEXPECTED_RESPONSE", String(e), 0, false, null, null);
}

/** Un dato con su estado: observado (con fuente) o desconocido (con motivo). */
function Fact({ label, value, field, profile, detail }: {
  label: string;
  value: string | null;
  field: string;
  profile: HardwareProfile;
  detail?: string;
}) {
  const unknown = profile.metadata.unknown?.find((u) => u.field === field);
  const source = Object.entries(profile.metadata.sources ?? {}).find(([k]) => field === k || field.startsWith(`${k}.`))?.[1];
  return (
    <>
      <dt>{label}</dt>
      <dd data-field={field}>
        {value === null ? (
          <span className="state-unknown">
            Desconocido{unknown ? ` — ${REASONS[unknown.reason] ?? unknown.reason}` : ""}
          </span>
        ) : (
          <>
            <span>{value}</span>
            {detail && <span className="detail"> ({detail})</span>}
            <span className="state-verified"> · Verificado</span>
          </>
        )}
        {source && <span className="source mono">Fuente: {source}</span>}
      </dd>
    </>
  );
}

function bytesFact(text: string | null): [string | null, string | undefined] {
  return text === null ? [null, undefined] : [formatBytes(text), exactBytes(text)];
}

function DeviceCard({ device }: { device: Device }) {
  const vendor = device.vendor ? (VENDORS[device.vendor] ?? `PCI ${device.vendor}`) : "Fabricante desconocido";
  const title = device.name ?? "Dispositivo sin nombre";
  return (
    <article className="device" aria-label={`${device.kind} ${title}`}>
      <h3>
        <span className="kind">{device.kind}</span> {title}
      </h3>
      <dl className="facts">
        <dt>Fabricante</dt>
        <dd>{vendor}</dd>
        <dt>Driver</dt>
        <dd>{device.driver_version ?? <span className="state-unknown">Desconocido</span>}</dd>
        {device.dedicated_memory_bytes !== null && (
          <>
            <dt>Memoria dedicada</dt>
            <dd>
              {formatBytes(device.dedicated_memory_bytes)} <span className="detail">({exactBytes(device.dedicated_memory_bytes)})</span>
            </dd>
          </>
        )}
        {device.shared_memory_bytes !== null && (
          <>
            <dt>Memoria compartida</dt>
            <dd>
              {formatBytes(device.shared_memory_bytes)} <span className="detail">({exactBytes(device.shared_memory_bytes)})</span>
              {device.unified_pool_id && <span className="source">Sale de la RAM del sistema (memoria unificada): no se cuenta dos veces.</span>}
            </dd>
          </>
        )}
        <dt>APIs que funcionan</dt>
        <dd data-field="backends">
          {device.backends.length === 0 ? (
            <span>Ninguna verificada</span>
          ) : (
            <ul className="chips" aria-label={`APIs verificadas en ${title}`}>
              {device.backends.map((b) => <li key={b}>{API_NAMES[b] ?? b}</li>)}
            </ul>
          )}
        </dd>
        <dt>Identificador</dt>
        <dd className="mono">{device.device_id ?? "desconocido"}</dd>
      </dl>
    </article>
  );
}

/** Volumen observado (P012, `domain::storage::StorageVolume`). */
export interface StorageVolume {
  identity: {
    volume_key: string | null;
    volume_guid: string | null;
    unc_path: string | null;
    mount_points: string[];
    drive_letter: string | null;
  };
  label: string | null;
  filesystem: string | null;
  performance_class: "NVME" | "SSD" | "HDD" | "NETWORK" | "REMOVABLE" | "UNKNOWN";
  capacity_bytes: string | null;
  free_bytes: string | null;
  read_only: boolean;
  online: boolean;
}

export interface StorageObservation {
  volumes: StorageVolume[];
  unknown: { field: string; reason: string; detail: string }[];
  observed_at: string;
}

const CLASS_TEXT: Record<StorageVolume["performance_class"], string> = {
  NVME: "NVMe",
  SSD: "SSD",
  HDD: "Disco duro (HDD)",
  NETWORK: "Red",
  REMOVABLE: "Extraíble",
  UNKNOWN: "Desconocida",
};

function VolumeRow({ v }: { v: StorageVolume }) {
  const where = v.identity.unc_path ?? v.identity.drive_letter ?? (v.identity.mount_points[0] || "Sin letra");
  return (
    <tr data-volume={v.identity.volume_key ?? "desconocido"}>
      <th scope="row">
        {where}
        {v.label && <span className="detail"> · {v.label}</span>}
        <span className="source mono">{v.identity.volume_guid ?? v.identity.volume_key ?? "Identidad desconocida"}</span>
      </th>
      <td className={v.online ? "status-verified" : "status-unknown"}>
        {v.online ? (v.read_only ? "Solo lectura" : "Disponible") : "No disponible"}
      </td>
      <td>{CLASS_TEXT[v.performance_class]}</td>
      <td className="mono">{v.filesystem ?? "—"}</td>
      <td>
        {v.capacity_bytes && v.free_bytes ? (
          <>
            {formatBytes(v.free_bytes)} libres de {formatBytes(v.capacity_bytes)}
            <span className="source">{exactBytes(v.free_bytes)} libres</span>
          </>
        ) : (
          <span className="state-unknown">Desconocido</span>
        )}
      </td>
    </tr>
  );
}

function StorageSection({ storage }: { storage: HardwareProfile["metadata"]["storage"] }) {
  if (!storage) return null;
  if ("error" in storage) {
    return (
      <section className="panel" aria-labelledby="hw-almacenamiento">
        <h2 id="hw-almacenamiento">Almacenamiento</h2>
        <p className="state-unknown" role="note">
          No se pudieron enumerar los volúmenes ({storage.error}). Que no aparezcan no significa que no existan.
        </p>
      </section>
    );
  }
  return (
    <section className="panel" aria-labelledby="hw-almacenamiento">
      <h2 id="hw-almacenamiento">Almacenamiento</h2>
      <table className="matrix">
        <caption>Cada volumen se identifica por su GUID (o su ruta de red), no por la letra: si la letra cambia, LARM lo sigue reconociendo.</caption>
        <thead>
          <tr>
            <th scope="col">Volumen</th>
            <th scope="col">Estado</th>
            <th scope="col">Clase</th>
            <th scope="col">Sistema de ficheros</th>
            <th scope="col">Espacio</th>
          </tr>
        </thead>
        <tbody>
          {storage.volumes.map((v) => <VolumeRow key={v.identity.volume_key ?? v.identity.mount_points.join()} v={v} />)}
        </tbody>
      </table>
    </section>
  );
}

function AcceleratorTable({ rows }: { rows: Accelerator[] }) {
  return (
    <table className="matrix">
      <caption>Cada API se marca como «Funciona» solo si LARM la ha probado en un dispositivo real.</caption>
      <thead>
        <tr>
          <th scope="col">API</th>
          <th scope="col">Estado</th>
          <th scope="col">Versión</th>
          <th scope="col">Detalle</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((r) => (
          <tr key={r.api} data-api={r.api} data-status={r.status}>
            <th scope="row">{API_NAMES[r.api] ?? r.api}</th>
            <td className={`status-${r.status.toLowerCase()}`}>{STATUS_TEXT[r.status]}</td>
            <td className="mono">{r.version ?? "—"}</td>
            <td>
              {r.reason && <strong>{REASONS[r.reason] ?? r.reason}. </strong>}
              <span className="detail">{r.evidence}</span>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function HardwarePage({ client }: { client: ApiClient }) {
  const [state, setState] = useState<State>({ kind: "loading" });
  const [refresh, setRefresh] = useState<Refresh>({ kind: "idle" });
  const alive = useRef(true);
  const load = useCallback(async () => {
    setState({ kind: "loading" });
    try {
      const profiles = await loadProfiles(client);
      if (alive.current) setState({ kind: "ready", profiles });
    } catch (e) {
      if (alive.current) setState({ kind: "error", error: asApiError(e) });
    }
  }, [client]);
  useEffect(() => {
    alive.current = true;
    void load();
    return () => {
      alive.current = false;
    };
  }, [load]);

  const startRefresh = useCallback(async () => {
    setRefresh({ kind: "running", jobId: null });
    try {
      const receipt = await client.request<JobReceipt>({
        method: "POST",
        path: "/api/v1/hardware/refresh",
        body: { resource_id: null, profile_id: null },
        headers: { "idempotency-key": `hw-refresh-${crypto.randomUUID()}` },
      });
      if (alive.current) setRefresh({ kind: "running", jobId: receipt.job_id });
      let job: JobState = { job_id: receipt.job_id, state: receipt.state, error: null };
      while (alive.current && (job.state === "QUEUED" || job.state === "RUNNING")) {
        await new Promise((r) => window.setTimeout(r, POLL_MS));
        job = await client.get<JobState>(`/api/v1/jobs/${receipt.job_id}`);
      }
      if (!alive.current) return;
      if (job.state === "FAILED") {
        setRefresh({ kind: "failed", message: job.error?.message ?? "El sondeo falló" });
        return;
      }
      setRefresh({ kind: "done" });
      await load();
    } catch (e) {
      const error = asApiError(e);
      if (!alive.current) return;
      setRefresh({
        kind: "failed",
        message: error.code === "FORBIDDEN" ? "Volver a sondear necesita permisos de administrador." : error.message,
      });
    }
  }, [client, load]);

  if (state.kind === "loading") {
    return (
      <section className="panel" aria-busy="true">
        <p>Cargando el perfil de hardware…</p>
      </section>
    );
  }
  if (state.kind === "error") {
    return (
      <section role="alert" className="panel panel-error">
        <h2>No se pudo cargar el perfil de hardware</h2>
        <p>{state.error.message}</p>
        <p className="mono">{state.error.code}{state.error.reason ? ` (${state.error.reason})` : ""}</p>
        <div className="actions">
          <button type="button" onClick={() => void load()}>Reintentar</button>
        </div>
      </section>
    );
  }
  const refreshBar = (
    <div className="actions">
      <button type="button" onClick={() => void startRefresh()} disabled={refresh.kind === "running"}>
        {refresh.kind === "running" ? "Sondeando el equipo…" : "Volver a sondear"}
      </button>
      <span className="detail">Solo lee el equipo: no instala ni cambia drivers.</span>
      <span role="status" aria-live="polite" className="refresh-status">
        {refresh.kind === "done" && "Sondeo terminado y guardado."}
      </span>
      {refresh.kind === "failed" && (
        <span role="alert" className="state-unknown">{refresh.message}</span>
      )}
    </div>
  );
  const current = state.profiles.at(-1);
  if (!current) {
    return (
      <section className="panel">
        <h2>Todavía no hay ningún perfil</h2>
        <p>El daemon sondea el equipo al arrancar. Si acaba de iniciarse, el perfil aparecerá en unos segundos.</p>
        <div className="actions">
          <button type="button" className="secondary" onClick={() => void load()}>Comprobar de nuevo</button>
        </div>
        {refreshBar}
      </section>
    );
  }
  const { os, cpu, ram, devices, dependencies, observed_metrics: live } = current.snapshot;
  const pagefile = current.metadata.pagefile?.total_bytes ?? null;
  const pending = current.metadata.coverage?.pending ?? [];
  const accelerators = current.metadata.accelerators ?? [];
  const deviceUnknown = current.metadata.unknown?.find((u) => u.field === "devices");
  const probedAt = new Date(current.probed_at);
  const [installed, installedExact] = bytesFact(ram.physical_bytes);
  const [usable, usableExact] = bytesFact(ram.usable_bytes);
  const [available, availableExact] = bytesFact(live.available_ram_bytes);
  const [page, pageExact] = bytesFact(pagefile);
  const cores = (n: number | null) => (n === null ? null : n.toLocaleString("es-ES"));

  return (
    <div className="stack">
      <section className="panel" aria-labelledby="hw-resumen">
        <h2 id="hw-resumen">Último sondeo</h2>
        <dl className="facts">
          <dt>Fecha</dt>
          <dd>
            <time dateTime={current.probed_at}>{probedAt.toLocaleString("es-ES")}</time>
            <span className="detail mono"> ({current.probed_at} UTC)</span>
          </dd>
          <dt>Huella de hardware</dt>
          <dd className="mono" data-testid="hw-fingerprint">
            {current.hardware_fingerprint}
            {current.metadata.fingerprint && <span className="source">Versión {current.metadata.fingerprint.version} del cálculo</span>}
          </dd>
          {current.metadata.fingerprint && (
            <>
              <dt>Cambios</dt>
              <dd data-testid="hw-changes">
                {current.metadata.fingerprint.previous_profile_id === null ? (
                  <span>Primera configuración registrada.</span>
                ) : current.metadata.fingerprint.changes.length === 0 ? (
                  <span>Ninguno respecto a la configuración anterior.</span>
                ) : (
                  <ul className="components" aria-label="Cambios respecto a la configuración anterior">
                    {current.metadata.fingerprint.changes.map((c) => (
                      <li key={c.field}>
                        <span className="mono">{c.field}</span>: {c.before === null || c.before === undefined ? "—" : JSON.stringify(c.before)} →{" "}
                        {c.after === null || c.after === undefined ? "—" : JSON.stringify(c.after)}
                      </li>
                    ))}
                  </ul>
                )}
              </dd>
            </>
          )}
          <dt>Perfil</dt>
          <dd>
            Revisión {current.row_version} · {state.profiles.length === 1 ? "1 configuración registrada" : `${state.profiles.length} configuraciones registradas`}
          </dd>
          {current.metadata.profiler && (
            <>
              <dt>Profiler</dt>
              <dd className="mono">{current.metadata.profiler.name} {current.metadata.profiler.version}</dd>
            </>
          )}
        </dl>
        {refreshBar}
      </section>

      <section className="panel" aria-labelledby="hw-sistema">
        <h2 id="hw-sistema">Sistema</h2>
        <dl className="facts">
          <Fact label="Edición" value={os.edition} field="os.edition" profile={current} />
          <Fact label="Build" value={os.build} field="os.build" profile={current} />
          <Fact label="Arquitectura" value={os.architecture} field="os.architecture" profile={current} />
        </dl>
      </section>

      <section className="panel" aria-labelledby="hw-cpu">
        <h2 id="hw-cpu">Procesador</h2>
        <dl className="facts">
          <Fact label="Fabricante" value={cpu.vendor} field="cpu.vendor" profile={current} />
          <Fact label="Modelo" value={cpu.model} field="cpu.model" profile={current} />
          <Fact label="Núcleos físicos" value={cores(cpu.physical_cores)} field="cpu.physical_cores" profile={current} />
          <Fact label="Hilos (núcleos lógicos)" value={cores(cpu.logical_cores)} field="cpu.logical_cores" profile={current} />
          <dt>Extensiones</dt>
          <dd data-field="cpu.instruction_sets">
            {cpu.instruction_sets.length === 0 ? (
              <span>Ninguna detectada</span>
            ) : (
              <ul className="chips" aria-label="Extensiones del juego de instrucciones">
                {cpu.instruction_sets.map((isa) => <li key={isa} className="mono">{isa}</li>)}
              </ul>
            )}
            <span className="source mono">Fuente: {current.metadata.sources?.["cpu.instruction_sets"] ?? "CPUID"}</span>
          </dd>
        </dl>
      </section>

      <section className="panel" aria-labelledby="hw-memoria">
        <h2 id="hw-memoria">Memoria</h2>
        <dl className="facts">
          <Fact label="RAM instalada" value={installed} detail={installedExact} field="ram.physical_bytes" profile={current} />
          <Fact label="RAM visible para Windows" value={usable} detail={usableExact} field="ram.usable_bytes" profile={current} />
          <Fact label="Archivo de paginación" value={page} detail={pageExact} field="pagefile" profile={current} />
          <dt>RAM libre ahora</dt>
          <dd data-field="observed_metrics.available_ram_bytes">
            {available === null ? <span className="state-unknown">Desconocido</span> : <span>{available} <span className="detail">({availableExact})</span></span>}
            <span className="source">Medida en el momento del sondeo; no forma parte de la huella.</span>
          </dd>
        </dl>
        {ram.physical_bytes !== null && ram.usable_bytes !== null && BigInt(ram.physical_bytes) > BigInt(ram.usable_bytes) && (
          <p className="note">
            Windows ve menos RAM de la instalada: la diferencia ({formatBytes((BigInt(ram.physical_bytes) - BigInt(ram.usable_bytes)).toString())}) está reservada
            para otros dispositivos, como la memoria de vídeo de una gráfica integrada. LARM no la cuenta como RAM disponible.
          </p>
        )}
      </section>

      {pending.includes("devices") ? (
        <section className="panel" aria-labelledby="hw-pendiente">
          <h2 id="hw-pendiente">GPU, NPU, drivers y aceleradores</h2>
          <p>
            <strong>Todavía no se sondean.</strong> Que no aparezcan aquí no significa que el equipo no los tenga: esta versión solo
            detecta sistema, procesador y memoria.
          </p>
        </section>
      ) : (
        <>
          <section className="panel" aria-labelledby="hw-dispositivos">
            <h2 id="hw-dispositivos">GPU y NPU</h2>
            {deviceUnknown && (
              <p className="state-unknown" role="note">
                No se pudieron enumerar los dispositivos: {REASONS[deviceUnknown.reason] ?? deviceUnknown.reason}. Que no aparezcan no significa que no existan.
              </p>
            )}
            {devices.length === 0 && !deviceUnknown && <p>No hay ninguna GPU ni NPU física en este equipo.</p>}
            <div className="devices">
              {devices.map((d) => <DeviceCard key={`${d.kind}-${d.device_id}`} device={d} />)}
            </div>
          </section>

          <section className="panel" aria-labelledby="hw-apis">
            <h2 id="hw-apis">APIs de aceleración</h2>
            <AcceleratorTable rows={accelerators} />
          </section>

          <StorageSection storage={current.metadata.storage} />

          {dependencies.length > 0 && (
            <section className="panel" aria-labelledby="hw-componentes">
              <h2 id="hw-componentes">Componentes de runtime</h2>
              <p className="detail">Forman parte de la huella: si cambia uno (por ejemplo, con una actualización), cambia la huella.</p>
              <ul className="components">
                {dependencies.map((d) => (
                  <li key={`${d.name}-${d.hash}`}>
                    <span className="mono">{d.name}</span> {d.version ?? "versión desconocida"}
                    {d.hash && <span className="detail mono"> · sha256 {d.hash.slice(0, 16)}…</span>}
                  </li>
                ))}
              </ul>
            </section>
          )}
        </>
      )}
    </div>
  );
}
