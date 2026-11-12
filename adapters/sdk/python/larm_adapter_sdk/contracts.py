"""Carga los contratos fijados del paquete; nunca descarga schemas de la red."""
import json
import hashlib
from pathlib import Path

from jsonschema import Draft202012Validator, FormatChecker, validators, ValidationError
from referencing import Registry, Resource


class ContractViolation(ValueError):
    """El documento no cumple el contrato público; no incluye el dato rechazado."""


def _maximum_decimal(_validator, limit, instance, _schema):
    if isinstance(instance, str) and (not instance.isascii() or not instance.isdigit() or (len(instance), instance) > (len(limit), limit)):
        yield ValidationError("Decimal fuera del límite publicado")


LarmValidator = validators.extend(Draft202012Validator, {"x-maximum-decimal": _maximum_decimal})


class Contracts:
    def __init__(self, docs: Path):
        self.docs = Path(docs)
        registry = Registry()
        self.documents = {}
        for name in ("larm.schema.json", "domain.schema.json"):
            document = json.loads((self.docs / "contracts/schemas" / name).read_text(encoding="utf-8"))
            self.documents[name] = document
            resource = Resource.from_contents(document)
            registry = registry.with_resource(name, resource)
            if "$id" in document:
                registry = registry.with_resource(document["$id"], resource)
        self.registry = registry
        table = json.loads((self.docs / "contracts/adapter-methods.json").read_text(encoding="utf-8"))
        self.methods = {item["method"]: item for item in table["methods"]}
        self.common = table["mandatory_common"]
        self.frames = {name: json.loads((self.docs / "adapters/schemas" / f"frame.{name}.schema.json").read_text(encoding="utf-8")) for name in ("request", "response", "progress")}
        self.validators = {}
        self.method_validators = self._method_contracts(table)

    @staticmethod
    def _local_refs(value):
        if isinstance(value, list):
            for child in value:
                Contracts._local_refs(child)
        elif isinstance(value, dict):
            if "x-maximum-decimal" in value and (not isinstance(value["x-maximum-decimal"], str) or not value["x-maximum-decimal"].isascii() or not value["x-maximum-decimal"].isdigit()):
                raise ContractViolation("Límite decimal del contrato inválido")
            if set(value) & {"$id", "$anchor", "$dynamicRef", "$dynamicAnchor", "$recursiveRef"} or ("$ref" in value and (not isinstance(value["$ref"], str) or not value["$ref"].startswith("#/$defs/"))):
                raise ContractViolation("Referencia externa o scope no permitido")
            for child in value.values():
                Contracts._local_refs(child)

    def _method_contracts(self, table):
        folder = self.docs / "adapters/method-schemas"
        try:
            index = json.loads((folder / "index.json").read_text(encoding="utf-8"))
            sources = {"adapter-methods.json": self.docs / "contracts/adapter-methods.json", **{name: self.docs / "contracts/schemas" / name for name in self.documents}}
            if index["schema_version"] != "1.0" or index["sources"] != {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in sources.items()}:
                raise ContractViolation("Índice de contratos no corresponde a las fuentes")
            if len(index["methods"]) != len(table["methods"]) or {entry["method"] for entry in index["methods"]} != set(self.methods):
                raise ContractViolation("Tabla de contratos incompleta o duplicada")
            validators = {}
            for entry in index["methods"]:
                method = entry["method"]
                canonical = self.methods[method]
                if entry["family"] != canonical["family"] or entry["required"] != canonical["required"]:
                    raise ContractViolation("Binding de método distinto del contrato")
                for direction in ("request", "response"):
                    item = entry[direction]
                    name = f"{method}.{direction}.schema.json"
                    if item["file"] != name or item["canonical_ref"] != canonical[direction + "_schema"]:
                        raise ContractViolation("Binding de schema distinto del contrato")
                    raw = (folder / name).read_bytes()
                    if hashlib.sha256(raw).hexdigest() != item["sha256"]:
                        raise ContractViolation("Hash de schema no coincide")
                    schema = json.loads(raw)
                    self._local_refs(schema)
                    Draft202012Validator.check_schema(schema)
                    def deny_retrieval(_uri):
                        raise ContractViolation("Resolución externa deshabilitada")
                    validators[(method, direction)] = LarmValidator(schema, registry=Registry(retrieve=deny_retrieval), format_checker=FormatChecker())
            return validators
        except (OSError, ValueError, KeyError, TypeError) as error:
            raise ContractViolation("Contratos generados ausentes o inválidos") from error

    def validate(self, reference: str, value):
        if reference not in self.validators:
            self.validators[reference] = LarmValidator({"$ref": reference}, registry=self.registry, format_checker=FormatChecker())
        try:
            valid = self.validators[reference].is_valid(value)
        except Exception as error:
            raise ContractViolation("Referencia de schema no resuelta localmente") from error
        if not valid:
            raise ContractViolation("Documento fuera del schema publicado")
        return value

    def frame(self, kind, value):
        if not LarmValidator(self.frames[kind], format_checker=FormatChecker()).is_valid(value):
            raise ContractViolation("Frame fuera del contrato")
        return value

    def request(self, method, value):
        return self._method_value(method, "request", value)

    def response(self, method, value):
        return self._method_value(method, "response", value)

    def _method_value(self, method, direction, value):
        try:
            valid = self.method_validators[(method, direction)].is_valid(value)
        except Exception as error:
            raise ContractViolation("Contrato no disponible localmente") from error
        if not valid:
            raise ContractViolation("Documento fuera del schema publicado")
        return value

    def manifest(self, value):
        self.validate("larm.schema.json#/$defs/AdapterManifest", value)
        required = set(self.common) | {method for method, item in self.methods.items() if item["family"] == value["family"] and item["required"]}
        declared = value["methods"]
        if len(set(declared)) != len(declared) or not required.issubset(declared):
            raise ContractViolation("Faltan métodos obligatorios o hay duplicados")
        if any(method not in self.methods or self.methods[method]["family"] not in ("all", value["family"]) for method in declared):
            raise ContractViolation("Método desconocido o de otra familia")
        return value
