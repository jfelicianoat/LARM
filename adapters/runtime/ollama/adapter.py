"""Ollama worker: physical store inspection; remaining methods fail closed."""
import json
from pathlib import Path

from larm_adapter_sdk import DomainError
from store import Store, open_readonly, parse_json


class Adapter:
    def __init__(self, package, work, manifest, contracts):
        self.package, self.work = Path(package).resolve(strict=True), Path(work).resolve(strict=True)
        self.manifest, self.contracts = manifest, contracts
        permissions = manifest["permissions"]
        if (set(permissions["read_roots"]) != {"ollama-package", "ollama-work", "ollama-model-store"}
                or any(permissions[key] for key in ("write_roots", "network_origins", "process_allowlist", "secret_refs"))
                or manifest["capabilities"] or manifest["installation_strategies"]):
            raise DomainError("FORBIDDEN", "El worker requiere permisos de sólo lectura", "ollama_worker_permissions_invalid")
        with open_readonly(self.work / "profile.json", self.work) as stream:
            profile = parse_json(stream.read((1 << 20) + 1))
        if set(profile) != {"model_store_root"} or not isinstance(profile["model_store_root"], str):
            raise DomainError("VALIDATION_FAILED", "Perfil de almacén inválido", "ollama_store_profile_invalid")
        self.store = Store(profile["model_store_root"])

    def call(self, method, params, ctx):
        ctx.check_cancelled()
        if method == "runtime.inspect_model":
            self.contracts.request(method, params)
            return self.store.inspect(params["root_path"], ctx, params["format_hint"])
        raise DomainError("UNSUPPORTED_CAPABILITY", "Método de Ollama todavía no implementado", "ollama_method_not_implemented")

    def shutdown(self):
        # No owned service, persistent file handles, state or model mutation.
        pass
