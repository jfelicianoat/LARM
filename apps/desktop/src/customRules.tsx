import { useEffect, useId, useMemo, useState, type FormEvent } from "react";
import { ApiClient, ApiError } from "./api/client";
import {
  emptyCustomRules,
  newCustomRule,
  previewCustomRules,
  readCustomRules,
  writeCustomRules,
  type CustomRule,
  type CustomRules,
  type RuleField,
  type RuleOperator,
  type RulePredicate,
  type RuleValue,
  type VersionedCustomRules,
} from "./api/customRules";
import { useToasts } from "./components";

const FIELD_LABEL: Record<RuleField, string> = {
  format: "Formato",
  architecture: "Arquitectura",
  quantization: "Cuantización",
  capability: "Capacidad",
  backend: "Backend",
  os_build: "Versión de Windows",
};
const FIELDS = Object.keys(FIELD_LABEL) as RuleField[];
const EFFECT_LABEL = { require: "Requerir", deny: "Denegar", limit: "Limitar tamaño" } as const;
const OPERATOR_LABEL: Record<RuleOperator, string> = {
  eq: "es igual a",
  in: "está entre",
  contains: "contiene",
  range: "está en el intervalo",
  exists: "está presente",
};

function explain(error: unknown): string {
  if (!(error instanceof ApiError)) return "No se pudieron guardar las reglas. Revisa la conexión e inténtalo de nuevo.";
  if (error.reason === "stale_revision") return "Las reglas cambiaron en otra sesión. Recarga la versión actual antes de guardar.";
  if (error.reason === "custom_rules_invalid" || error.reason === "schema_violation") return "Hay una regla incompleta o incompatible. Revisa los campos y vuelve a intentarlo.";
  if (error.status === 403) return "Tu cuenta no tiene permiso para cambiar las reglas.";
  return error.message;
}

function editRule(rules: CustomRules, id: string, update: (rule: CustomRule) => CustomRule): CustomRules {
  return { ...rules, rules: rules.rules.map((rule) => rule.id === id ? update(rule) : rule) };
}

function parseValue(field: RuleField, operator: RuleOperator, text: string): RuleValue {
  if (operator === "exists") return text === "true";
  if (operator === "range" || operator === "in" || text.trimStart().startsWith("[")) {
    try { return JSON.parse(text) as RuleValue; } catch { return text; }
  }
  if (field === "os_build" && /^\d+$/.test(text)) return Number(text);
  return text;
}

function valueText(value: RuleValue): string {
  return typeof value === "string" ? value : JSON.stringify(value);
}

function previewFacts(values: Record<RuleField, string>): Partial<Record<RuleField, RuleValue>> {
  const facts: Partial<Record<RuleField, RuleValue>> = {};
  for (const field of FIELDS) {
    const text = values[field].trim();
    if (!text) continue;
    if (field === "os_build") {
      if (/^\d+$/.test(text)) facts[field] = Number(text);
    } else if (field === "capability") {
      facts[field] = text.split(",").map((item) => item.trim()).filter(Boolean);
    } else {
      facts[field] = text;
    }
  }
  return facts;
}

function RuleEditor({ rule, onChange, onRemove }: {
  rule: CustomRule;
  onChange: (next: CustomRule) => void;
  onRemove: () => void;
}) {
  const id = useId();
  const updatePredicate = (index: number, update: (predicate: RulePredicate) => RulePredicate) => {
    const predicates = rule.predicates.map((predicate, i) => i === index ? update(predicate) : predicate);
    onChange({ ...rule, predicates });
  };
  return (
    <article className="device rule-card" aria-label={`Regla ${rule.id}`}>
      <div className="rule-card-heading">
        <h3>{rule.id}</h3>
        <button type="button" className="secondary" onClick={onRemove}>Quitar regla</button>
      </div>
      <label htmlFor={`${id}-effect`}>Acción</label>
      <select id={`${id}-effect`} value={rule.effect} onChange={(event) => {
        const effect = event.target.value as CustomRule["effect"];
        onChange({ ...rule, effect, limit_bytes: effect === "limit" ? (rule.limit_bytes ?? "1073741824") : null });
      }}>
        {Object.entries(EFFECT_LABEL).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
      </select>
      {rule.predicates.map((predicate, index) => {
        const prefix = `${id}-predicate-${index}`;
        return (
          <fieldset className="rule-predicate" key={prefix}>
            <legend>Condición {index + 1}</legend>
            <label htmlFor={`${prefix}-field`}>Dato</label>
            <select id={`${prefix}-field`} value={predicate.field} onChange={(event) => updatePredicate(index, (p) => ({ ...p, field: event.target.value as RuleField }))}>
              {FIELDS.map((field) => <option key={field} value={field}>{FIELD_LABEL[field]}</option>)}
            </select>
            <label htmlFor={`${prefix}-operator`}>Condición</label>
            <select id={`${prefix}-operator`} value={predicate.operator} onChange={(event) => updatePredicate(index, (p) => ({ ...p, operator: event.target.value as RuleOperator }))}>
              {Object.entries(OPERATOR_LABEL).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
            </select>
            <label htmlFor={`${prefix}-value`}>Valor</label>
            {predicate.operator === "exists" ? (
              <select id={`${prefix}-value`} value={String(predicate.value)} onChange={(event) => updatePredicate(index, (p) => ({ ...p, value: event.target.value === "true" }))}>
                <option value="true">Sí</option><option value="false">No</option>
              </select>
            ) : (
              <input
                id={`${prefix}-value`}
                value={valueText(predicate.value)}
                aria-describedby={`${prefix}-help`}
                onChange={(event) => updatePredicate(index, (p) => ({ ...p, value: parseValue(p.field, p.operator, event.target.value) }))}
              />
            )}
            <p className="detail" id={`${prefix}-help`}>
              {predicate.operator === "in" || predicate.operator === "range" ? "Escribe una lista JSON; para intervalos usa [mínimo, máximo]." : "Todas las condiciones de una regla deben cumplirse."}
            </p>
            {rule.predicates.length > 1 && <button type="button" className="link" onClick={() => onChange({ ...rule, predicates: rule.predicates.filter((_, i) => i !== index) })}>Quitar condición</button>}
          </fieldset>
        );
      })}
      <button type="button" className="secondary" onClick={() => onChange({ ...rule, predicates: [...rule.predicates, { field: "format", operator: "eq", value: "gguf" }] })}>Añadir condición AND</button>
      {rule.effect === "limit" && <>
        <label htmlFor={`${id}-limit`}>Límite de espacio (bytes)</label>
        <input id={`${id}-limit`} inputMode="numeric" value={rule.limit_bytes ?? ""} onChange={(event) => onChange({ ...rule, limit_bytes: event.target.value })} />
      </>}
      {(rule.effect === "require" || rule.effect === "deny") && <>
        <label htmlFor={`${id}-target`}>Ubicación concreta (opcional)</label>
        <input id={`${id}-target`} className="mono" value={rule.target_storage_location_id ?? ""} placeholder="UUIDv7 de ubicación" onChange={(event) => onChange({ ...rule, target_storage_location_id: event.target.value || null })} />
      </>}
    </article>
  );
}

export function CustomRulesPage({ client }: { client: ApiClient }) {
  const [loaded, setLoaded] = useState<VersionedCustomRules | null>(null);
  const [draft, setDraft] = useState<CustomRules>(emptyCustomRules);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [facts, setFacts] = useState<Record<RuleField, string>>({ format: "gguf", architecture: "", quantization: "", capability: "text.generate", backend: "", os_build: "" });
  const [candidateId, setCandidateId] = useState("");
  const { notify } = useToasts();
  const ids = useId();

  useEffect(() => {
    let active = true;
    setLoading(true); setError(null);
    void readCustomRules(client).then((current) => {
      if (!active) return;
      setLoaded(current); setDraft(current.value);
    }).catch((reason: unknown) => { if (active) setError(explain(reason)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [client, refresh]);

  const preview = useMemo(() => previewCustomRules(draft, previewFacts(facts), candidateId.trim() || null), [draft, facts, candidateId]);
  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (!loaded) return;
    setBusy(true); setError(null);
    try {
      const saved = await writeCustomRules(client, loaded.revision, draft);
      setLoaded(saved); setDraft(saved.value);
      notify("info", "Las reglas de ubicación se han guardado.");
    } catch (reason) {
      setError(explain(reason));
    } finally { setBusy(false); }
  };

  if (loading) return <section className="panel" aria-busy="true"><p>Cargando reglas de ubicación…</p></section>;
  if (!loaded) return <section className="panel"><p role="alert">{error ?? "No se pudieron cargar las reglas."}</p><div className="actions"><button type="button" onClick={() => setRefresh((value) => value + 1)}>Reintentar</button></div></section>;

  return <form className="custom-rules-page" onSubmit={(event) => void save(event)}>
    <section className="panel">
      <h2>Ubicación automática</h2>
      <p>Define condiciones seguras para elegir dónde guardar modelos. Las condiciones de cada regla se combinan con AND; denegar tiene prioridad sobre requerir y limitar.</p>
      <p className="detail">Revisión guardada: <span className="mono">{loaded.revision}</span></p>
      {draft.rules.length === 0 ? <p>No hay reglas. LARM pedirá una ubicación cuando una política personalizada no tenga instrucciones.</p> : <div className="devices">
        {draft.rules.map((rule) => <RuleEditor key={rule.id} rule={rule} onChange={(next) => setDraft((current) => editRule(current, rule.id, () => next))} onRemove={() => setDraft((current) => ({ ...current, rules: current.rules.filter((entry) => entry.id !== rule.id) }))} />)}
      </div>}
      <div className="actions">
        <button type="button" className="secondary" disabled={busy} onClick={() => setDraft((current) => ({ ...current, rules: [...current.rules, newCustomRule()] }))}>Añadir regla</button>
        <button type="submit" disabled={busy}>{busy ? "Guardando…" : "Guardar reglas"}</button>
        <button type="button" className="secondary" disabled={busy} onClick={() => { setDraft(loaded.value); setError(null); }}>Descartar cambios</button>
      </div>
      {error && <p role="alert" className="state-unknown">{error}</p>}
    </section>

    <section className="panel rule-preview" aria-labelledby={`${ids}-preview`}>
      <h2 id={`${ids}-preview`}>Vista previa</h2>
      <p>Prueba las reglas con datos de ejemplo. La decisión final también aplica disponibilidad, espacio y los demás filtros de ubicación.</p>
      <div className="preview-facts">
        {FIELDS.map((field) => <label key={field} htmlFor={`${ids}-${field}`}>{FIELD_LABEL[field]}
          <input id={`${ids}-${field}`} value={facts[field]} inputMode={field === "os_build" ? "numeric" : undefined} onChange={(event) => setFacts((current) => ({ ...current, [field]: event.target.value }))} />
        </label>)}
        <label htmlFor={`${ids}-candidate`}>ID de ubicación a probar (opcional)
          <input id={`${ids}-candidate`} className="mono" value={candidateId} onChange={(event) => setCandidateId(event.target.value)} />
        </label>
      </div>
      <dl className="facts" data-testid="rules-preview">
        <dt>Resultado</dt><dd data-testid="preview-disposition">{{ deny: "Denegada", requirement_not_met: "Requisitos no cumplidos", require: "Ubicación requerida", limit: "Límite aplicado", no_match: "Sin coincidencias", conflict: "Ubicaciones requeridas incompatibles", invalid_candidate: "El ID de ubicación no es válido" }[preview.disposition]}</dd>
        <dt>Reglas coincidentes</dt><dd>{preview.matched_rule_ids.join(", ") || "Ninguna"}</dd>
        {preview.blocking_rule_ids.length > 0 && <><dt>Reglas que bloquean</dt><dd>{preview.blocking_rule_ids.join(", ")}</dd></>}
        {preview.required_storage_location_id && <><dt>Ubicación requerida</dt><dd className="mono">{preview.required_storage_location_id}</dd></>}
        {preview.effective_limit_bytes && <><dt>Límite efectivo</dt><dd className="mono">{preview.effective_limit_bytes} bytes</dd></>}
      </dl>
    </section>
  </form>;
}
