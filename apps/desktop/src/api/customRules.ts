import { ApiError, type ApiClient } from "./client";

export type RuleField = "format" | "architecture" | "quantization" | "capability" | "backend" | "os_build";
export type RuleOperator = "eq" | "in" | "contains" | "range" | "exists";
export type RuleEffect = "require" | "deny" | "limit";
export type RuleValue = string | number | boolean | Array<string | number>;

export interface RulePredicate {
  field: RuleField;
  operator: RuleOperator;
  value: RuleValue;
}

export interface CustomRule {
  id: string;
  predicates: RulePredicate[];
  effect: RuleEffect;
  limit_bytes: string | null;
  target_storage_location_id: string | null;
}

export interface CustomRules {
  schema_version: "1.0";
  rules: CustomRule[];
}

export interface VersionedCustomRules {
  value: CustomRules;
  revision: number;
}

export type RuleFacts = Partial<Record<RuleField, RuleValue>>;
export interface CustomRulesPreview {
  disposition: "deny" | "requirement_not_met" | "require" | "limit" | "no_match" | "conflict" | "invalid_candidate";
  matched_rule_ids: string[];
  blocking_rule_ids: string[];
  required_storage_location_id: string | null;
  effective_limit_bytes: string | null;
}

const PATH = "/api/v1/custom-rules";

function revisionFrom(headers: Record<string, string> | undefined): number {
  const raw = headers?.etag;
  const match = raw && /^"?(0|[1-9][0-9]*)"?$/.exec(raw);
  const revision = match ? Number(match[1]) : Number.NaN;
  if (!Number.isSafeInteger(revision) || revision < 0) {
    throw new ApiError("PROTOCOL_ERROR", "La respuesta de reglas no incluye una revisión válida.", 502, false, null, "custom_rules_etag_invalid");
  }
  return revision;
}

export async function readCustomRules(client: ApiClient): Promise<VersionedCustomRules> {
  const response = await client.requestRaw<CustomRules>({ method: "GET", path: PATH });
  return { value: response.body, revision: revisionFrom(response.headers) };
}

export async function writeCustomRules(client: ApiClient, revision: number, value: CustomRules): Promise<VersionedCustomRules> {
  const response = await client.requestRaw<CustomRules>({
    method: "PATCH",
    path: PATH,
    headers: { "If-Match": String(revision) },
    body: value,
  });
  return { value: response.body, revision: revisionFrom(response.headers) };
}

function equal(left: RuleValue, right: RuleValue): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function matches(predicate: RulePredicate, facts: RuleFacts): boolean {
  const actual = facts[predicate.field];
  if (predicate.operator === "exists") return (actual !== undefined) === predicate.value;
  if (actual === undefined) return false;
  switch (predicate.operator) {
    case "eq": return equal(actual, predicate.value);
    case "in": {
      const allowedValues = predicate.value;
      if (!Array.isArray(allowedValues)) return false;
      const values = Array.isArray(actual) ? actual : [actual];
      return values.some((value) => allowedValues.some((allowed) => equal(value as RuleValue, allowed)));
    }
    case "contains": {
      if (Array.isArray(predicate.value)) {
        return Array.isArray(actual) && predicate.value.every((value) => actual.some((item) => equal(item as RuleValue, value)));
      }
      if (typeof predicate.value !== "string") return Array.isArray(actual) && actual.some((value) => equal(value, predicate.value));
      return (typeof actual === "string" && actual.includes(predicate.value))
        || (Array.isArray(actual) && actual.some((value) => equal(value as RuleValue, predicate.value)));
    }
    case "range": {
      if (!Array.isArray(predicate.value) || predicate.value.length !== 2 || typeof actual !== "number") return false;
      const [minimum, maximum] = predicate.value;
      return Number.isSafeInteger(actual) && typeof minimum === "number" && typeof maximum === "number"
        && Number.isSafeInteger(minimum) && Number.isSafeInteger(maximum) && minimum >= 0 && minimum <= maximum
        && actual >= minimum && actual <= maximum;
    }
  }
}

/** Preview con la misma precedencia que el evaluador de dominio de LARM. */
export function previewCustomRules(value: CustomRules, facts: RuleFacts, candidateId: string | null = null): CustomRulesPreview {
  if (candidateId !== null && !/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(candidateId)) {
    return { disposition: "invalid_candidate", matched_rule_ids: [], blocking_rule_ids: [], required_storage_location_id: null, effective_limit_bytes: null };
  }
  const matchedIds: string[] = [];
  const denials: string[] = [];
  const unmet: string[] = [];
  const requirements: string[] = [];
  const limits: Array<[string, bigint]> = [];
  const requiredTargets = new Set<string>();

  for (const rule of value.rules) {
    const predicatesMatch = rule.predicates.every((predicate) => matches(predicate, facts));
    const targetMatches = rule.target_storage_location_id === null || rule.target_storage_location_id === candidateId;
    if (rule.effect === "require") {
      if (predicatesMatch) {
        if (rule.target_storage_location_id) requiredTargets.add(rule.target_storage_location_id);
        if (!targetMatches) unmet.push(rule.id);
        else { matchedIds.push(rule.id); requirements.push(rule.id); }
      } else {
        unmet.push(rule.id);
      }
    } else if (predicatesMatch && targetMatches && rule.effect === "deny") {
      matchedIds.push(rule.id); denials.push(rule.id);
    } else if (predicatesMatch && targetMatches && rule.effect === "limit" && rule.limit_bytes && /^\d+$/.test(rule.limit_bytes)) {
      matchedIds.push(rule.id); limits.push([rule.id, BigInt(rule.limit_bytes)]);
    }
  }
  const effectiveLimit = limits.map(([, bytes]) => bytes).reduce<bigint | null>((minimum, bytes) => minimum === null || bytes < minimum ? bytes : minimum, null);
  const disposition: CustomRulesPreview["disposition"] = denials.length ? "deny"
    : requiredTargets.size > 1 ? "conflict"
    : unmet.length ? "requirement_not_met"
      : requirements.length ? "require"
        : limits.length ? "limit" : "no_match";
  matchedIds.sort(); denials.sort(); unmet.sort();
  return {
    disposition,
    matched_rule_ids: matchedIds,
    blocking_rule_ids: denials.length ? denials : requiredTargets.size > 1 ? requirements.sort() : unmet,
    required_storage_location_id: requiredTargets.size === 1 ? [...requiredTargets][0] : null,
    effective_limit_bytes: effectiveLimit?.toString() ?? null,
  };
}

export function emptyCustomRules(): CustomRules {
  return { schema_version: "1.0", rules: [] };
}

export function newCustomRule(): CustomRule {
  return {
    id: `rule-${crypto.randomUUID()}`,
    predicates: [{ field: "format", operator: "eq", value: "gguf" }],
    effect: "require",
    limit_bytes: null,
    target_storage_location_id: null,
  };
}
