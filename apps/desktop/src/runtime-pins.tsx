import { useEffect, useRef, useState } from "react";
import { ApiClient, ApiError } from "./api/client";
import { launchProfiles, type LaunchProfile } from "./api/launchProfiles";
import { allPages, type RuntimeVersion, type RuntimeVersionPin } from "./api/runtimes";

interface VariantOption {
  id: string;
  status: string;
  lifecycle: string;
}

const PINNABLE = new Set(["VALIDATED", "DEFAULT", "RETAINED"]);

function explainPin(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 403) return "Tu cuenta no tiene permiso para cambiar los pins de runtime.";
    if (error.status === 409) return "El runtime o la compatibilidad cambiaron. Recarga el detalle antes de volver a fijar la versión.";
    if (error.status === 404) return "No se encontró el modelo, el perfil o la versión seleccionada.";
    if (error.reason === "runtime_pin_compatibility_not_proven") return "No hay una compatibilidad vigente y aprobada para esa combinación. No se guardó el pin.";
  }
  return "No se pudo actualizar el pin. Comprueba el estado y vuelve a intentarlo.";
}

function shortId(value: string): string { return value.slice(0, 8); }

export function RuntimePinManager({ client, versions, refresh, onChanged }: {
  client: ApiClient;
  versions: RuntimeVersion[];
  refresh: number;
  onChanged: () => void;
}) {
  const [variants, setVariants] = useState<VariantOption[]>([]);
  const [profiles, setProfiles] = useState<LaunchProfile[]>([]);
  const [pins, setPins] = useState<RuntimeVersionPin[]>([]);
  const [variantId, setVariantId] = useState("");
  const [profileId, setProfileId] = useState("");
  const [versionId, setVersionId] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const pinAttempt = useRef<{ body: string; key: string } | null>(null);
  const unpinAttempt = useRef<{ body: string; key: string } | null>(null);
  const eligibleVersions = versions.filter((version) => PINNABLE.has(version.lifecycle));

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    void Promise.all([
      allPages<VariantOption>(client, "/api/v1/variants", () => active),
      launchProfiles(client),
      allPages<RuntimeVersionPin>(client, "/api/v1/runtime-pins", () => active),
    ]).then(([nextVariants, nextProfiles, nextPins]) => {
      if (!active) return;
      setVariants(nextVariants);
      setProfiles(nextProfiles);
      setPins(nextPins);
      setVariantId((current) => current && nextVariants.some((item) => item.id === current) ? current : nextVariants[0]?.id ?? "");
      setProfileId((current) => current && nextProfiles.some((item) => item.id === current) ? current : nextProfiles[0]?.id ?? "");
      setVersionId((current) => current && versions.some((item) => item.id === current && PINNABLE.has(item.lifecycle))
        ? current
        : versions.find((item) => PINNABLE.has(item.lifecycle))?.id ?? "");
    }).catch(() => {
      if (active) setError("No se pudieron cargar los modelos, perfiles y pins del runtime.");
    }).finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [client, refresh, versions]);

  const currentPin = pins.find((pin) => pin.variant_id === variantId && pin.launch_profile_id === profileId);
  const pinnedVersion = currentPin ? versions.find((version) => version.id === currentPin.runtime_version_id) : undefined;
  const selectedVersion = eligibleVersions.find((version) => version.id === versionId);

  const pin = async () => {
    if (!selectedVersion || !variantId || !profileId || busy) return;
    const bodyValue = { variant_id: variantId, launch_profile_id: profileId };
    const body = `${JSON.stringify(bodyValue)}|${versionId}`;
    if (pinAttempt.current?.body !== body) pinAttempt.current = { body, key: crypto.randomUUID() };
    setBusy(true); setError(null); setMessage(null);
    try {
      const receipt = await client.request<{ resource_id: string; row_version: number; event_id: string }>({
        method: "POST", path: `/api/v1/runtime-versions/${encodeURIComponent(versionId)}/pin`, body: bodyValue,
        headers: { "Idempotency-Key": pinAttempt.current.key },
      });
      if (!receipt || typeof receipt.resource_id !== "string" || !Number.isInteger(receipt.row_version) || typeof receipt.event_id !== "string") throw new Error("runtime_pin_receipt_invalid");
      pinAttempt.current = null;
      setMessage("Fijación guardada. Las próximas leases de esta combinación usarán esta versión.");
      onChanged();
    } catch (cause) {
      setError(cause instanceof Error && cause.message === "runtime_pin_receipt_invalid" ? "El daemon no devolvió un recibo válido; consulta los pins antes de repetir." : explainPin(cause));
    } finally { setBusy(false); }
  };

  const unpin = async () => {
    if (!currentPin || busy) return;
    const body = JSON.stringify({ variant_id: variantId, launch_profile_id: profileId });
    if (unpinAttempt.current?.body !== body) unpinAttempt.current = { body, key: crypto.randomUUID() };
    setBusy(true); setError(null); setMessage(null);
    try {
      await client.request<unknown>({
        method: "DELETE", path: `/api/v1/runtime-pins/${encodeURIComponent(variantId)}/${encodeURIComponent(profileId)}`,
        headers: { "Idempotency-Key": unpinAttempt.current.key },
      });
      unpinAttempt.current = null;
      setMessage("Pin retirado para las próximas leases. Las leases existentes conservan su versión.");
      onChanged();
    } catch (cause) {
      setError(explainPin(cause));
    } finally { setBusy(false); }
  };

  return <section className="runtime-pin-manager" aria-label="Fijación de versión por modelo y perfil">
    <h3>Fijar versión a un modelo y perfil</h3>
    <p>El pin afecta a nuevas leases. Una lease activa conserva la versión con la que se creó.</p>
    {loading && <p role="status">Cargando modelos, perfiles y pins…</p>}
    {error && <p role="alert" className="state-unknown">{error}</p>}
    {!loading && !error && <>
      {(!variants.length || !profiles.length) && <p className="state-unknown">Registra al menos un modelo y un perfil de lanzamiento antes de crear un pin.</p>}
      {!eligibleVersions.length && <p className="state-unknown">No hay versiones validadas disponibles para fijar.</p>}
      {!!variants.length && <label htmlFor="runtime-pin-variant">Modelo</label>}
      {!!variants.length && <select id="runtime-pin-variant" value={variantId} disabled={busy} onChange={(event) => { setVariantId(event.target.value); setMessage(null); setError(null); }}>
        {variants.map((variant) => <option key={variant.id} value={variant.id}>Modelo {shortId(variant.id)} · {variant.status} · {variant.lifecycle}</option>)}
      </select>}
      {!!profiles.length && <label htmlFor="runtime-pin-profile">Perfil de lanzamiento</label>}
      {!!profiles.length && <select id="runtime-pin-profile" value={profileId} disabled={busy} onChange={(event) => { setProfileId(event.target.value); setMessage(null); setError(null); }}>
        {profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.name} · {shortId(profile.id)}</option>)}
      </select>}
      {!!eligibleVersions.length && <label htmlFor="runtime-pin-version">Versión que fijar</label>}
      {!!eligibleVersions.length && <select id="runtime-pin-version" value={versionId} disabled={busy} onChange={(event) => { setVersionId(event.target.value); setMessage(null); setError(null); }}>
        {eligibleVersions.map((version) => <option key={version.id} value={version.id}>{version.version_string} · {version.lifecycle}</option>)}
      </select>}
      {currentPin && <p role="status">Pin actual: {pinnedVersion?.version_string ?? `versión ${shortId(currentPin.runtime_version_id)}`}.</p>}
      {!currentPin && variantId && profileId && <p>Esta combinación usa la versión predeterminada del runtime.</p>}
      {message && <p role="status">{message}</p>}
      <div className="actions">
        <button type="button" disabled={busy || loading || !selectedVersion || !variantId || !profileId || currentPin?.runtime_version_id === versionId} onClick={() => void pin()}>
          {busy ? "Guardando…" : currentPin ? "Cambiar versión fijada" : "Fijar esta versión"}
        </button>
        {currentPin && <button type="button" className="secondary" disabled={busy || loading} onClick={() => void unpin()}>
          Retirar pin para nuevas leases
        </button>}
      </div>
      <p className="detail">El servidor comprueba que la versión siga instalada, validada y compatible con el modelo y el perfil.</p>
    </>}
  </section>;
}
