import { useCallback, useEffect, useId, useRef, useState, type FormEvent } from "react";
import { ApiError, type ApiClient } from "./api/client";
import { useToasts } from "./components";
import { exactBytes, formatBytes } from "./hardware";

/** `domain.schema.json#/$defs/storage_location` (P014). */
export interface StorageLocation {
  id: string;
  row_version: number;
  metadata: {
    volume_budget?: { usable_bytes: string; shared_with: string[] };
    performance_class_source?: "observed" | "hint";
  };
  name: string;
  root_path: string;
  canonical_path: string;
  volume_guid: string | null;
  drive_letter: string | null;
  unc_path: string | null;
  filesystem: string;
  storage_type: string;
  capacity_bytes: string;
  free_bytes: string;
  reserved_bytes: string;
  priority: number;
  auto_placement: boolean;
  state: "ONLINE" | "OFFLINE" | "READ_ONLY" | "LOW_SPACE" | "DEGRADED" | "MISSING" | "ERROR" | "UNKNOWN";
  last_seen_at: string | null;
  volume_key: string;
  performance_class: "NVME" | "SSD" | "HDD" | "NETWORK" | "REMOVABLE" | "UNKNOWN";
}

interface Page {
  items: StorageLocation[];
  next_cursor: string | null;
}

type Draft = {
  name: string;
  root_path: string;
  reserved_gib: string;
  priority: string;
  auto_placement: boolean;
  performance_class: StorageLocation["performance_class"];
};

const STATE_TEXT: Record<StorageLocation["state"], string> = {
  ONLINE: "Disponible",
  OFFLINE: "Sin conexión (el disco no está)",
  READ_ONLY: "Solo lectura",
  LOW_SPACE: "Poco espacio",
  DEGRADED: "Degradada",
  MISSING: "Falta la carpeta",
  ERROR: "Error",
  UNKNOWN: "Desconocido",
};

const CLASS_TEXT: Record<StorageLocation["performance_class"], string> = {
  NVME: "NVMe",
  SSD: "SSD",
  HDD: "Disco duro (HDD)",
  NETWORK: "Red",
  REMOVABLE: "Extraíble",
  UNKNOWN: "Desconocida",
};

const GIB = 1024n ** 3n;

/** GiB escritos por la persona (coma o punto, hasta 3 decimales) → bytes exactos. */
export function gibToBytes(text: string): string | null {
  const clean = text.trim().replace(",", ".");
  const m = /^(\d{1,12})(?:\.(\d{1,3}))?$/.exec(clean);
  if (!m) return null;
  const whole = BigInt(m[1]);
  const frac = BigInt((m[2] ?? "").padEnd(3, "0"));
  return (whole * GIB + (frac * GIB) / 1000n).toString();
}

/** Bytes → GiB para el formulario (3 decimales como mucho). */
export function bytesToGib(text: string): string {
  const bytes = BigInt(text);
  const thousandths = (bytes * 1000n + GIB / 2n) / GIB;
  const whole = thousandths / 1000n;
  const frac = (thousandths % 1000n).toString().padStart(3, "0").replace(/0+$/, "");
  return frac ? `${whole},${frac}` : whole.toString();
}

const EMPTY: Draft = { name: "", root_path: "", reserved_gib: "0", priority: "0", auto_placement: false, performance_class: "UNKNOWN" };

function toDraft(l: StorageLocation): Draft {
  return {
    name: l.name,
    root_path: l.root_path,
    reserved_gib: bytesToGib(l.reserved_bytes),
    priority: String(l.priority),
    auto_placement: l.auto_placement,
    performance_class: l.performance_class,
  };
}

function asApiError(e: unknown): ApiError {
  return e instanceof ApiError ? e : new ApiError("UNEXPECTED_RESPONSE", String(e), 0, false, null, null);
}

/** Mensaje claro para los rechazos del servidor (la ruta se valida al guardar). */
const REASONS: Record<string, string> = {
  root_not_found: "La carpeta no existe o no está accesible.",
  root_not_directory: "La ruta es un fichero, no una carpeta.",
  root_not_writable: "LARM no puede escribir en esa carpeta.",
  root_not_readable: "LARM no puede leer esa carpeta.",
  reparse_point_rejected: "La ruta pasa por un enlace o una junction; elige la carpeta real.",
  root_path_invalid: "La ruta no es válida: usa una ruta completa (C:\\…) o de red (\\\\servidor\\recurso\\…).",
  root_overlap: "La carpeta está dentro de otra ubicación, o la contiene.",
  location_has_artifacts: "La ubicación tiene modelos: muévelos antes de quitarla.",
  location_in_use: "La ubicación la usan runtimes o descargas.",
  root_change_requires_migration: "No se puede cambiar la carpeta de una ubicación con modelos: muévelos.",
  volume_identity_changed: "Esa carpeta está ahora en otro disco; los modelos siguen en el original.",
  reservation_exceeds_capacity: "La reserva supera la capacidad del disco. Introduce una cantidad menor.",
  stale_revision: "Alguien cambió esta ubicación mientras la editabas. Vuelve a abrirla.",
};

function explain(e: ApiError): string {
  return (e.reason && REASONS[e.reason]) ?? e.message;
}

function LocationForm({ initial, title, onSubmit, onCancel }: {
  initial: Draft;
  title: string;
  onSubmit: (draft: Draft, reservedBytes: string) => Promise<void>;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState<Draft>(initial);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const id = useId();
  const reserved = gibToBytes(draft.reserved_gib);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!draft.name.trim() || !draft.root_path.trim()) {
      setError("El nombre y la carpeta son obligatorios.");
      return;
    }
    if (reserved === null || !/^\d+$/.test(draft.priority)) {
      setError("La reserva debe ser un número de GiB y la prioridad un entero ≥ 0.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await onSubmit(draft, reserved);
    } catch (e) {
      setError(explain(asApiError(e)));
    } finally {
      setBusy(false);
    }
  };
  const field = (name: keyof Draft) => `${id}-${name}`;
  return (
    <form className="panel form" onSubmit={(e) => void submit(e)} aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{title}</h2>
      <label htmlFor={field("name")}>Nombre</label>
      <input id={field("name")} value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} />
      <label htmlFor={field("root_path")}>Carpeta</label>
      <input
        id={field("root_path")}
        className="mono"
        value={draft.root_path}
        placeholder={"D:\\ModelosIA o \\\\NAS\\IA"}
        onChange={(e) => setDraft({ ...draft, root_path: e.target.value })}
      />
      <p className="detail">LARM comprueba que la carpeta existe, que puede escribir en ella y que no es un enlace antes de guardarla.</p>
      <label htmlFor={field("reserved_gib")}>Espacio que no se debe usar (GiB)</label>
      <input id={field("reserved_gib")} inputMode="decimal" value={draft.reserved_gib} onChange={(e) => setDraft({ ...draft, reserved_gib: e.target.value })} />
      <p className="detail" data-testid="reserved-exact">{reserved === null ? "Número no válido" : exactBytes(reserved)}</p>
      <label htmlFor={field("priority")}>Prioridad (mayor = preferida)</label>
      <input id={field("priority")} inputMode="numeric" value={draft.priority} onChange={(e) => setDraft({ ...draft, priority: e.target.value })} />
      <label htmlFor={field("performance_class")}>Tipo de disco si LARM no lo detecta</label>
      <select
        id={field("performance_class")}
        value={draft.performance_class}
        onChange={(e) => setDraft({ ...draft, performance_class: e.target.value as Draft["performance_class"] })}
      >
        {Object.entries(CLASS_TEXT).map(([value, text]) => <option key={value} value={value}>{text}</option>)}
      </select>
      <label className="check">
        <input type="checkbox" checked={draft.auto_placement} onChange={(e) => setDraft({ ...draft, auto_placement: e.target.checked })} />
        Permitir que LARM elija esta ubicación automáticamente
      </label>
      {error && <p role="alert" className="state-unknown">{error}</p>}
      <div className="actions">
        <button type="submit" disabled={busy}>{busy ? "Comprobando y guardando…" : "Guardar"}</button>
        <button type="button" className="secondary" onClick={onCancel} disabled={busy}>Cancelar</button>
      </div>
    </form>
  );
}

function LocationCard({ l, all, onEdit, onProbe, onDelete }: {
  l: StorageLocation;
  all: StorageLocation[];
  onEdit: () => void;
  onProbe: () => void;
  onDelete: () => void;
}) {
  const budget = l.metadata.volume_budget;
  const shared = (budget?.shared_with ?? []).map((id) => all.find((o) => o.id === id)?.name ?? id);
  return (
    <article className="device" aria-label={`Ubicación ${l.name}`} data-state={l.state} data-resource-id={l.id} tabIndex={-1}>
      <h3>{l.name}</h3>
      <dl className="facts">
        <dt>Estado</dt>
        <dd className={l.state === "ONLINE" ? "status-verified" : "status-unknown"}>{STATE_TEXT[l.state]}</dd>
        <dt>Carpeta</dt>
        <dd className="mono">{l.root_path}</dd>
        <dt>Espacio utilizable</dt>
        <dd data-testid="usable">
          {budget ? formatBytes(budget.usable_bytes) : "—"} de {formatBytes(l.free_bytes)} libres
          <span className="source">
            {budget ? exactBytes(budget.usable_bytes) : ""} · reserva {formatBytes(l.reserved_bytes)} · capacidad {formatBytes(l.capacity_bytes)}
          </span>
          {shared.length > 0 && (
            <span className="source">Comparte el disco con {shared.join(", ")}: el espacio y la reserva son de todo el disco.</span>
          )}
        </dd>
        <dt>Disco</dt>
        <dd>
          {CLASS_TEXT[l.performance_class]}
          {l.metadata.performance_class_source === "hint" && <span className="detail"> (indicado por ti)</span>} · {l.filesystem}
          {l.drive_letter && ` · ${l.drive_letter}`}
        </dd>
        <dt>Identificador del volumen</dt>
        <dd className="mono">{l.volume_guid ?? l.unc_path ?? "Desconocido: no se usará para colocar modelos automáticamente"}</dd>
        <dt>Colocación automática</dt>
        <dd>{l.auto_placement ? `Sí · prioridad ${l.priority}` : "No"}</dd>
        <dt>Visto por última vez</dt>
        <dd>{l.last_seen_at ? new Date(l.last_seen_at).toLocaleString("es-ES") : "Nunca"}</dd>
      </dl>
      <div className="actions">
        <button type="button" onClick={onEdit}>Editar</button>
        <button type="button" className="secondary" onClick={onProbe}>Analizar</button>
        <button type="button" className="secondary" onClick={onDelete}>Quitar</button>
      </div>
    </article>
  );
}

const POLL_MS = 1000;

export function StoragePage({ client, focusResourceId }: { client: ApiClient; focusResourceId?: string }) {
  const [items, setItems] = useState<StorageLocation[] | null>(null);
  const [loadError, setLoadError] = useState<ApiError | null>(null);
  const [mode, setMode] = useState<{ kind: "list" } | { kind: "create" } | { kind: "edit"; location: StorageLocation } | { kind: "delete"; location: StorageLocation }>({ kind: "list" });
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const { notify } = useToasts();
  const alive = useRef(true);

  const load = useCallback(async () => {
    setLoadError(null);
    try {
      const all: StorageLocation[] = [];
      let cursor: string | null = null;
      for (let i = 0; i < 20; i += 1) {
        const query: string = cursor ? `?limit=200&cursor=${encodeURIComponent(cursor)}` : "?limit=200";
        const page: Page = await client.get<Page>(`/api/v1/storage-locations${query}`);
        all.push(...page.items);
        cursor = page.next_cursor;
        if (!cursor) break;
      }
      if (alive.current) setItems(all);
    } catch (e) {
      if (alive.current) setLoadError(asApiError(e));
    }
  }, [client]);

  useEffect(() => {
    alive.current = true;
    void load();
    return () => {
      alive.current = false;
    };
  }, [load]);

  useEffect(() => {
    if (!focusResourceId || !items) return;
    const target = document.querySelector<HTMLElement>(`[data-resource-id="${focusResourceId}"]`);
    target?.focus({ preventScroll: true });
    target?.scrollIntoView({ block: "center" });
  }, [focusResourceId, items]);

  const returnToList = () => {
    setDeleteError(null);
    setMode({ kind: "list" });
    void load();
  };

  const save = async (draft: Draft, reserved: string, existing?: StorageLocation) => {
    const body = {
      name: draft.name.trim(),
      root_path: draft.root_path.trim(),
      reserved_bytes: reserved,
      priority: Number(draft.priority),
      auto_placement: draft.auto_placement,
      performance_class: draft.performance_class,
    };
    if (existing) {
      await client.request({ method: "PATCH", path: `/api/v1/storage-locations/${existing.id}`, body, headers: { "if-match": String(existing.row_version) } });
      notify("info", `Ubicación «${body.name}» guardada.`);
    } else {
      await client.request({ method: "POST", path: "/api/v1/storage-locations", body, headers: { "idempotency-key": `loc-${crypto.randomUUID()}` } });
      notify("info", `Ubicación «${body.name}» añadida.`);
    }
    setMode({ kind: "list" });
    await load();
  };

  const probe = async (l: StorageLocation) => {
    try {
      const receipt = await client.request<{ job_id: string; state: string }>({
        method: "POST",
        path: `/api/v1/storage-locations/${l.id}/probe`,
        body: { resource_id: null, profile_id: null },
        headers: { "idempotency-key": `probe-${crypto.randomUUID()}` },
      });
      let state = receipt.state;
      while (alive.current && (state === "QUEUED" || state === "RUNNING")) {
        await new Promise((r) => window.setTimeout(r, POLL_MS));
        state = (await client.get<{ state: string }>(`/api/v1/jobs/${receipt.job_id}`)).state;
      }
      notify(state === "COMPLETED" ? "info" : "error", state === "COMPLETED" ? `«${l.name}» analizada.` : `No se pudo analizar «${l.name}».`);
      await load();
    } catch (e) {
      notify("error", explain(asApiError(e)));
    }
  };

  const remove = async (l: StorageLocation) => {
    setDeleteError(null);
    try {
      await client.request({ method: "DELETE", path: `/api/v1/storage-locations/${l.id}`, headers: { "if-match": String(l.row_version) } });
      notify("info", `Ubicación «${l.name}» quitada. Los ficheros de la carpeta no se han tocado.`);
      setMode({ kind: "list" });
      await load();
    } catch (e) {
      setDeleteError(explain(asApiError(e)));
    }
  };

  if (loadError) {
    return (
      <section role="alert" className="panel panel-error">
        <h2>No se pudieron cargar las ubicaciones</h2>
        <p>{loadError.message}</p>
        <div className="actions">
          <button type="button" onClick={() => void load()}>Reintentar</button>
        </div>
      </section>
    );
  }
  if (items === null) {
    return (
      <section className="panel" aria-busy="true">
        <p>Cargando ubicaciones…</p>
      </section>
    );
  }
  if (mode.kind === "create") {
    return <LocationForm initial={EMPTY} title="Nueva ubicación" onSubmit={(d, r) => save(d, r)} onCancel={returnToList} />;
  }
  if (mode.kind === "edit") {
    return (
      <LocationForm
        initial={toDraft(mode.location)}
        title={`Editar «${mode.location.name}»`}
        onSubmit={(d, r) => save(d, r, mode.location)}
        onCancel={returnToList}
      />
    );
  }
  return (
    <div className="stack">
      {mode.kind === "delete" && (
        <section role="alertdialog" aria-labelledby="borrar-titulo" className="panel panel-error">
          <h2 id="borrar-titulo">¿Quitar la ubicación «{mode.location.name}»?</h2>
          <p>LARM dejará de usarla. No se borra ningún fichero de la carpeta. Si tiene modelos, primero hay que moverlos.</p>
          {deleteError && <p role="alert" className="state-unknown">{deleteError}</p>}
          <div className="actions">
            <button type="button" onClick={() => void remove(mode.location)}>Quitar</button>
            <button type="button" className="secondary" onClick={returnToList}>Cancelar</button>
          </div>
        </section>
      )}
      <section className="panel" aria-labelledby="ubicaciones-titulo">
        <h2 id="ubicaciones-titulo">Ubicaciones</h2>
        {items.length === 0 ? (
          <p>Todavía no hay ninguna ubicación. Añade una carpeta donde LARM pueda guardar modelos.</p>
        ) : (
          <div className="devices">
            {items.map((l) => (
              <LocationCard
                key={l.id}
                l={l}
                all={items}
                onEdit={() => setMode({ kind: "edit", location: l })}
                onProbe={() => void probe(l)}
                onDelete={() => setMode({ kind: "delete", location: l })}
              />
            ))}
          </div>
        )}
        <div className="actions">
          <button type="button" onClick={() => setMode({ kind: "create" })}>Añadir ubicación</button>
          <button type="button" className="secondary" onClick={() => void load()}>Actualizar ubicaciones</button>
        </div>
      </section>
    </div>
  );
}
