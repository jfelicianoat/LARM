import { useEffect, useState } from "react";
import { ApiClient } from "./api/client";
import { STATUS_LABEL, type StatusSnapshot } from "./api/status";
import { formatBytes } from "./hardware";
import { PROMOTION_LABEL } from "./model-promote";

interface Page<T> { items: T[]; next_cursor: string | null }
interface VariantRow { id: string; quantization: string | null; promotion: string }
interface AdapterRow { id: string; family: string; state: string }
interface LocationRow { id: string; name: string; state: string; metadata: { volume_budget?: { usable_bytes: string } } }
interface JobRow { id: string; state: string; adapter_id: string | null }

function countsBy<T>(items: T[], key: (item: T) => string): Array<[string, number]> {
  const counts = new Map<string, number>();
  for (const item of items) counts.set(key(item), (counts.get(key(item)) ?? 0) + 1);
  return [...counts.entries()].sort((a, b) => a[0].localeCompare(b[0]));
}

function LinkCard({ title, href, children }: { title: string; href: string; children: React.ReactNode }) {
  return <section className="panel home-card" aria-labelledby={`home-${href.slice(2)}-heading`}>
    <div className="model-section-heading">
      <h2 id={`home-${href.slice(2)}-heading`}>{title}</h2>
      <a className="link" href={href}>Abrir sección</a>
    </div>
    {children}
  </section>;
}

function FailedCard({ title, message }: { title: string; message: string }) {
  return <section className="panel home-card" aria-label={title}>
    <h2>{title}</h2>
    <p role="alert" className="state-unknown">{message}</p>
  </section>;
}

/**
 * Pantalla de inicio (§106, P120 como dueño funcional): el estado de la
 * sesión compuesto con los datos ya servidos por la API pública. Cada
 * tarjeta carga y falla por su lado.
 */
export function HomePage({ client, snapshot }: { client: ApiClient; snapshot: StatusSnapshot }) {
  const [variants, setVariants] = useState<VariantRow[] | null>(null);
  const [variantsError, setVariantsError] = useState<string | null>(null);
  const [adapters, setAdapters] = useState<AdapterRow[] | null>(null);
  const [adaptersError, setAdaptersError] = useState<string | null>(null);
  const [locations, setLocations] = useState<LocationRow[] | null>(null);
  const [locationsError, setLocationsError] = useState<string | null>(null);
  const [activeJobs, setActiveJobs] = useState<Array<{ kind: "transformación" | "descarga"; job: JobRow }> | null>(null);
  const [jobsError, setJobsError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    void Promise.allSettled([
      client.get<Page<VariantRow>>("/api/v1/variants?limit=200"),
      client.get<Page<AdapterRow>>("/api/v1/adapters?limit=200"),
      client.get<Page<LocationRow>>("/api/v1/storage-locations?limit=200"),
      Promise.all([
        client.get<Page<JobRow>>("/api/v1/transformations?limit=200"),
        client.get<Page<JobRow>>("/api/v1/downloads?limit=200"),
      ]),
    ]).then(([variants, adapters, locations, jobs]) => {
      if (!active) return;
      if (variants.status === "fulfilled") setVariants(variants.value.items);
      else setVariantsError("No se pudieron cargar las variantes.");
      if (adapters.status === "fulfilled") setAdapters(adapters.value.items);
      else setAdaptersError("No se pudieron cargar los adaptadores.");
      if (locations.status === "fulfilled") setLocations(locations.value.items);
      else setLocationsError("No se pudo cargar el almacenamiento.");
      if (jobs.status === "fulfilled") {
        const [transformations, downloads] = jobs.value;
        const activeList: Array<{ kind: "transformación" | "descarga"; job: JobRow }> = [];
        for (const job of transformations.items) {
          if (job.state === "RUNNING" || job.state === "QUEUED") activeList.push({ kind: "transformación", job });
        }
        for (const job of downloads.items) {
          if (job.state === "RUNNING" || job.state === "QUEUED") activeList.push({ kind: "descarga", job });
        }
        setActiveJobs(activeList);
      } else {
        setJobsError("No se pudieron cargar los trabajos en curso.");
      }
    });
    return () => { active = false; };
  }, [client]);

  const usableBytes = locations?.reduce((total, location) => {
    const budget = location.metadata?.volume_budget?.usable_bytes;
    return total + (budget ? BigInt(budget) : BigInt(0));
  }, BigInt(0));
  const usableLabel = usableBytes !== undefined && usableBytes > BigInt(0) ? formatBytes(usableBytes.toString()) : null;

  return <div className="home-page">
    <section className="panel home-card" aria-labelledby="home-status-heading">
      <div className="model-section-heading"><h2 id="home-status-heading">Estado del daemon</h2><a className="link" href="#/diagnostico">Diagnóstico</a></div>
      <p>
        <strong>{STATUS_LABEL[snapshot.status]}</strong>
        {snapshot.checkedAt && <span className="detail"> · última comprobación {snapshot.checkedAt.toLocaleTimeString()}</span>}
      </p>
    </section>

    {variantsError ? <FailedCard title="Modelos" message={variantsError} /> : <LinkCard title="Modelos" href="#/mis-modelos">
      {variants === null ? <p>Cargando…</p> : <>
        <p>{variants.length === 200 ? "Más de " : ""}{variants.length} variante{variants.length === 1 ? "" : "s"} registrada{variants.length === 1 ? "" : "s"}.</p>
        {variants.length > 0 && <>
          <p>Promoción: {countsBy(variants, (item) => item.promotion).map(([state, count]) => `${PROMOTION_LABEL[state] ?? state}: ${count}`).join(" · ")}</p>
          <p>Con cuantización registrada: {variants.filter((item) => item.quantization !== null).length}</p>
        </>}
      </>}
    </LinkCard>}

    {adaptersError ? <FailedCard title="Adaptadores" message={adaptersError} /> : <LinkCard title="Adaptadores" href="#/adaptadores">
      {adapters === null ? <p>Cargando…</p> : <>
        <p>{adapters.length} instalado{adapters.length === 1 ? "" : "s"}.</p>
        {adapters.length > 0 && <p>Por familia: {countsBy(adapters, (item) => item.family).map(([family, count]) => `${family}: ${count}`).join(" · ")}</p>}
        <p>Cualificados (VALIDATED): {adapters.filter((item) => item.state === "VALIDATED").length}</p>
      </>}
    </LinkCard>}

    {locationsError ? <FailedCard title="Almacenamiento" message={locationsError} /> : <LinkCard title="Almacenamiento" href="#/almacenamiento">
      {locations === null ? <p>Cargando…</p> : <>
        <p>{locations.filter((item) => item.state === "ONLINE").length} ubicación{locations.filter((item) => item.state === "ONLINE").length === 1 ? "" : "es"} ONLINE de {locations.length}.</p>
        {usableLabel && <p>Presupuesto usable: {usableLabel}</p>}
      </>}
    </LinkCard>}

    {jobsError ? <FailedCard title="Trabajos en curso" message={jobsError} /> : <LinkCard title="Trabajos en curso" href="#/descargas">
      {activeJobs === null ? <p>Cargando…</p> : activeJobs.length === 0 ? <p>Sin descargas ni transformaciones en curso.</p> : <>
        <ul>{activeJobs.map(({ kind, job }) => <li key={`${kind}-${job.id}`}>{kind === "transformación" ? "Transformación" : "Descarga"} ({job.state}){job.adapter_id ? ` · ${job.adapter_id}` : ""}</li>)}</ul>
      </>}
    </LinkCard>}
  </div>;
}
