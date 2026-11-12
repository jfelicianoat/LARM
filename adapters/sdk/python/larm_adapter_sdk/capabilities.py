"""Registro tipado privado de descriptores; no amplía DTOs ni concede cualificación.

El paquete aporta schemas/fixture fijados y el handler aporta mapping/estimador
ejecutables. Una entrada válida aún debe superar las pruebas de conducta P026.
"""
import copy
import hashlib
import json
import re

import rfc8785
from jsonschema import Draft202012Validator, FormatChecker
from referencing import Registry

from .contracts import Contracts, ContractViolation, LarmValidator
from .server import DomainError

LIMIT = 1 << 53
HASH = {"type": "string", "pattern": "^[0-9a-f]{64}$"}
TEXT = {"type": "string", "minLength": 1}
DESCRIPTOR = {"type":"object","additionalProperties":False,"required":["id","version","input_schema","output_schema","input_sha256","output_sha256","mapping","estimator_id","fixture","fixture_sha256"],"properties":{"id":TEXT,"version":TEXT,"input_schema":{"type":"object"},"output_schema":{"type":"object"},"input_sha256":HASH,"output_sha256":HASH,"mapping":TEXT,"estimator_id":TEXT,"fixture":{"type":"object","additionalProperties":False,"required":["input","output"],"properties":{"input":{"type":"object"},"output":{"type":"object"}}},"fixture_sha256":HASH}}


def canonical(value):
    def normalize(item):
        if isinstance(item, bool) or item is None or isinstance(item, (str, float)):
            return item
        if isinstance(item, int):
            if abs(item) > LIMIT:
                raise ValueError("Entero fuera del rango canónico del contrato")
            return float(item) if abs(item) == LIMIT else item
        if isinstance(item, list):
            return [normalize(child) for child in item]
        if isinstance(item, dict):
            if not all(isinstance(key, str) for key in item):
                raise ValueError("Claves JSON deben ser strings")
            return {key: normalize(child) for key, child in item.items()}
        raise ValueError("Tipo no JSON")
    return rfc8785.dumps(normalize(value))


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def failure(code, reason):
    return DomainError(code, "Contrato de capacidad no disponible o inválido", reason)


class CapabilityRegistry:
    def __init__(self, bundle, contracts, estimators, mappings):
        self.contracts, self.estimators = contracts, dict(estimators)
        self._entries, self._validators = {}, {}
        try:
            if set(bundle) != {"schema_version", "entries"} or bundle["schema_version"] != "1.0" or not isinstance(bundle["entries"], list) or not 1 <= len(bundle["entries"]) <= 128:
                raise ValueError("bundle")
            known = json.loads((contracts.docs / "contracts/capability-contracts.json").read_text(encoding="utf-8"))
            aliases = known["aliases_read_only"]
            self.aliases = dict(aliases)
            known_ids = {item["id"] for item in known["contracts"]}
            builtin = json.loads((contracts.docs / "adapters/capability-schemas/index.json").read_text(encoding="utf-8"))
            sources = {"capability-contracts.json":contracts.docs / "contracts/capability-contracts.json", **{name:contracts.docs / "contracts/schemas" / name for name in contracts.documents}}
            if builtin["schema_version"] != "1.0" or builtin["sources"] != {name:hashlib.sha256(path.read_bytes()).hexdigest() for name,path in sources.items()}:
                raise ValueError("builtin sources")
            builtin = {entry["id"]:entry for entry in builtin["capabilities"]}
            if set(builtin) != known_ids:
                raise ValueError("builtin entries")
            for original in bundle["entries"]:
                if not Draft202012Validator(DESCRIPTOR).is_valid(original):
                    raise ValueError("descriptor")
                entry = copy.deepcopy(original)
                identity = entry["id"]
                if not re.fullmatch(r"[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*", identity) or len(identity) > 128 or identity in aliases or (identity not in known_ids and "." not in identity) or not re.fullmatch(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?", entry["version"]):
                    raise ValueError("identity")
                if identity in self._entries:
                    raise failure("CONFLICT", "capability_descriptor_duplicate")
                if identity in builtin and any(entry[key] != builtin[identity][key] for key in ("version", "input_sha256", "output_sha256")):
                    raise failure("UNSUPPORTED_CAPABILITY", "capability_builtin_schema_mismatch")
                if entry["mapping"] not in mappings:
                    raise failure("UNSUPPORTED_CAPABILITY", "capability_mapping_missing")
                if entry["estimator_id"] not in self.estimators or not callable(self.estimators[entry["estimator_id"]]):
                    raise failure("UNSUPPORTED_CAPABILITY", "capability_estimator_missing")
                for direction in ("input", "output"):
                    schema = entry[direction + "_schema"]
                    if len(canonical(schema)) > 1 << 20 or digest(schema) != entry[direction + "_sha256"]:
                        raise failure("INTEGRITY_FAILED", "capability_schema_hash_mismatch")
                    Contracts._local_refs(schema)
                    Draft202012Validator.check_schema(schema)
                    def no_retrieval(_uri):
                        raise ContractViolation("Resolución externa deshabilitada")
                    validator = LarmValidator(schema, registry=Registry(retrieve=no_retrieval), format_checker=FormatChecker())
                    if not validator.is_valid(entry["fixture"][direction]):
                        raise failure("VALIDATION_FAILED", "capability_fixture_invalid")
                    self._validators[(identity, direction)] = validator
                if digest(entry["fixture"]) != entry["fixture_sha256"]:
                    raise failure("INTEGRITY_FAILED", "capability_fixture_hash_mismatch")
                self._entries[identity] = entry
            self.profile_hash = digest({"schema_version":"1.0","entries":[self._entries[name] for name in sorted(self._entries)]})
        except DomainError:
            raise
        except Exception:
            raise failure("VALIDATION_FAILED", "capability_descriptor_invalid") from None

    def entry(self, identity):
        identity = self.aliases.get(identity, identity)
        if identity not in self._entries:
            raise failure("UNSUPPORTED_CAPABILITY", "capability_descriptor_missing")
        return copy.deepcopy(self._entries[identity])

    def check_advertised(self, capabilities):
        if len(set(capabilities)) != len(capabilities) or any(name not in self._entries for name in capabilities):
            raise failure("UNSUPPORTED_CAPABILITY", "capability_descriptor_missing")
        for identity in capabilities:
            self.estimate(identity, self._entries[identity]["fixture"]["input"])

    def validate(self, identity, direction, value):
        identity = self.entry(identity)["id"]
        try:
            valid = self._validators[(identity, direction)].is_valid(value)
        except Exception:
            valid = False
        if not valid:
            raise failure("VALIDATION_FAILED" if direction == "input" else "PROTOCOL_ERROR", f"capability_{direction}_invalid")
        return value

    def estimate(self, identity, value):
        self.validate(identity, "input", value)
        entry = self.entry(identity)
        try:
            result = self.estimators[entry["estimator_id"]](copy.deepcopy(value))
            self.contracts.response("runtime.estimate_memory", result)
            return result
        except DomainError:
            raise
        except Exception:
            raise failure("PROTOCOL_ERROR", "capability_estimator_failed") from None

    def schema_id(self, identity, direction):
        entry = self.entry(identity)
        return f"urn:larm:capability:{entry['id']}:{entry['version']}:{direction}:sha256:{entry[direction + '_sha256']}"
