"""Paquete autónomo de referencia: copia Python/deps ya fijados, sin instalar."""
import copy
import hashlib
import importlib.metadata
import importlib.util
import json
import platform
import shutil
import sys
import uuid
import zipfile
from datetime import datetime, timezone
from pathlib import Path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def package_hash(root):
    entries = sorted((p.relative_to(root).as_posix().lower(), sha(p)) for p in root.rglob("*") if p.is_file() and p.relative_to(root).as_posix() != "manifest.json")
    return hashlib.sha256("".join(f"{name}\0{value}\n" for name, value in entries).encode()).hexdigest()


STDLIB_IGNORED = frozenset(("site-packages", "__pycache__", "test", "tests", "ensurepip", "idlelib", "turtledemo"))


def stdlib_archive(source, destination):
    """Retain the old Lib payload; store pure-code groups in a deterministic ZIP.

    A group with resources or native files stays on disk in its entirety, so
    its __file__-relative data/native loads keep their original layout.
    No exclusion is added to the original reference interpreter's selection.
    """
    source, destination = Path(source), Path(destination)
    files, directories, pending = [], [], [source]
    while pending:
        directory = pending.pop()
        for path in sorted(directory.iterdir()):
            if path.name in STDLIB_IGNORED:
                continue
            if path.is_symlink() or path.is_junction():
                raise ValueError("La biblioteca Python no admite enlaces")
            if path.is_dir():
                directories.append(path.relative_to(source))
                pending.append(path)
            elif path.is_file():
                files.append(path.relative_to(source))
            else:
                raise ValueError("Entrada Python no regular")
    files.sort()
    retained = {path.parts[0] for path in files if path.suffix.lower() not in {".py", ".pyc"}}
    library = destination / "Lib"
    library.mkdir()
    archive_path = destination / f"python{sys.version_info.major}{sys.version_info.minor}.zip"
    with zipfile.ZipFile(archive_path, "x", compression=zipfile.ZIP_STORED) as archive:
        archive_directories = set()
        for relative in files:
            if relative.parts[0] in retained:
                target = library / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source / relative, target)
                continue
            for parent in relative.parents:
                if parent != Path("."):
                    archive_directories.add(parent.as_posix() + "/")
        for directory in sorted(archive_directories):
            info = zipfile.ZipInfo(directory, (1980, 1, 1, 0, 0, 0))
            info.create_system = 3
            info.external_attr = (0o40755 << 16) | 0x10
            archive.writestr(info, b"")
        for relative in files:
            if relative.parts[0] not in retained:
                info = zipfile.ZipInfo(relative.as_posix(), (1980, 1, 1, 0, 0, 0))
                info.create_system = 3
                info.external_attr = 0o100644 << 16
                archive.writestr(info, (source / relative).read_bytes())
    # Preserve genuinely empty directories from the old filesystem payload.
    for directory in directories:
        if not any(path.is_relative_to(directory) for path in files):
            (library / directory).mkdir(parents=True, exist_ok=True)
    return archive_path


def module_paths(destination, family):
    if family not in {"runtime", "source", "protocol", "transformation", "benchmark"}:
        raise ValueError("Familia Python de referencia no válida")
    destination = Path(destination)
    python_tag = f"python{sys.version_info.major}{sys.version_info.minor}"
    if not (destination / f"{python_tag}.zip").is_file():
        raise ValueError("Falta la biblioteca Python empaquetada")
    (destination / f"{python_tag}._pth").write_text(
        f".\nDLLs\n{python_tag}.zip\nLib\nLib/site-packages\n../sdk\n../{family}\n", encoding="utf-8")


def interpreter(repo, destination):
    lock = json.loads((repo / "environment.lock.json").read_text(encoding="utf-8"))
    if platform.python_version() != lock["python"]["version"]:
        raise ValueError("Python no coincide con el lock")
    base = Path(sys.base_prefix)
    destination.mkdir()
    for path in [base / "python.exe", *base.glob("*.dll")]:
        shutil.copyfile(path, destination / path.name)
    shutil.copytree(base / "DLLs", destination / "DLLs", ignore=shutil.ignore_patterns("__pycache__"))
    stdlib_archive(base / "Lib", destination)
    sites = destination / "Lib/site-packages"
    sites.mkdir()
    modules = ("jsonschema", "referencing", "attrs", "attr", "rpds", "jsonschema_specifications", "typing_extensions", "rfc3339_validator", "rfc8785", "six")
    versions = {}
    for name in modules:
        spec = importlib.util.find_spec(name)
        if spec is None or spec.origin is None:
            raise ValueError("Falta una dependencia fijada del SDK")
        source = Path(spec.origin)
        if source.name == "__init__.py":
            shutil.copytree(source.parent, sites / source.parent.name, ignore=shutil.ignore_patterns("__pycache__", "tests", "test"))
        else:
            shutil.copyfile(source, sites / source.name)
    pinned = dict(line.strip().split("==") for line in (repo / "tools/requirements.txt").read_text(encoding="utf-8").splitlines() if "==" in line)
    for name in ("jsonschema", "referencing", "attrs", "rpds-py", "jsonschema-specifications", "typing_extensions", "rfc3339-validator", "rfc8785", "six"):
        distribution = importlib.metadata.distribution(name)
        if distribution.version != pinned[name]:
            raise ValueError("Dependencia no coincide con el lock de herramientas")
        versions[name] = distribution.version
        metadata = next(f for f in distribution.files if f.name == "METADATA" and ".dist-info" in str(f))
        directory = distribution.locate_file(metadata).parent
        shutil.copytree(directory, sites / directory.name)
    module_paths(destination, "source")
    license_path = base / "LICENSE.txt"
    if not license_path.is_file():
        raise ValueError("Falta la licencia local de Python")
    shutil.copyfile(license_path, destination / "LICENSE.txt")
    return {"python": platform.python_version(), "packages": versions, "scope": "reference adapter, not an AI runtime profile"}


def create(repo, root, work, base_url, artifacts):
    """artifacts=[{name,bytes,capability}]; bytes propios servidos por el harness."""
    root, work = root.resolve(), work.resolve()
    root.mkdir(parents=True, exist_ok=False)
    work.mkdir(parents=True, exist_ok=False)
    (work / "catalog").mkdir()
    (work / "storage").mkdir()
    source = repo / "adapters/source/reference"
    shutil.copytree(source, root / "source", ignore=shutil.ignore_patterns("__pycache__", "package.py"))
    shutil.copyfile(source / "capabilities.json",root / "reference-capabilities.json")
    shutil.copytree(repo / "adapters/sdk/python/larm_adapter_sdk", root / "sdk/larm_adapter_sdk", ignore=shutil.ignore_patterns("__pycache__"))
    for file in ("contracts/schemas/larm.schema.json", "contracts/schemas/domain.schema.json", "contracts/adapter-methods.json", "contracts/capability-contracts.json", "adapters/schemas/frame.request.schema.json", "adapters/schemas/frame.response.schema.json", "adapters/schemas/frame.progress.schema.json"):
        target = root / "docs" / file
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(repo / "docs" / file, target)
    bill = interpreter(repo, root / "bin")
    shutil.copytree(repo / "docs/adapters/method-schemas", root / "docs/adapters/method-schemas", ignore=shutil.ignore_patterns("README.md"))
    shutil.copytree(repo / "docs/adapters/capability-schemas", root / "docs/adapters/capability-schemas", ignore=shutil.ignore_patterns("README.md"))
    (root / "dependencies.lock.json").write_text(json.dumps(bill, indent=2), encoding="utf-8")
    table = json.loads((repo / "docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
    manifest = {"id": "reference.catalog", "version": "1.0.0", "family": "source", "protocol_version": "1.0", "publisher": "LARM controlled reference", "executable": {"relative_path": "bin/python.exe", "args": ["-I", "-B", "-m", "main"]}, "platforms": ["windows"], "architectures": ["x86_64"], "methods": table["mandatory_common"] + [item["method"] for item in table["methods"] if item["family"] == "source" and (item["required"] or item["method"] == "source.download_chunk")], "permissions": {"read_roots": ["reference-package", "reference-catalog", "reference-work"], "write_roots": ["reference-work"], "network_origins": [base_url], "process_allowlist": [], "secret_refs": []}, "capabilities": ["source.search", "source.revisions", "source.resume", "source.sha256", "source.sync"], "package_sha256": "0" * 64, "signature": None, "installation_strategies": []}
    source_id, storage_id = str(uuid.uuid7()), str(uuid.uuid7())
    config = {"source_id": source_id, "storage_location_id": storage_id, "probe_url": base_url + "/probe", "license_required": False, "allow_fixture_execution": True, "search_support": {"search_applicable": True, "supported_filters": ["source.search.q", "source.search.capabilities"], "limitations": ["controlled_reference_only", "core_admission_not_accredited"]}}
    # Binding de instancia fuera del paquete: no altera su hash ni sus permisos.
    (work / "profile.json").write_text(json.dumps({"source_id": source_id, "storage_location_id": storage_id}), encoding="utf-8")
    (root / "reference-source.json").write_text(json.dumps({key: value for key, value in config.items() if key not in ("source_id", "storage_location_id")}, indent=2), encoding="utf-8")
    # Reutiliza la forma publicada de ModelPage y ModelInspection de P043.
    template = json.loads((repo / "tests/fixtures/source/protocol.json").read_text(encoding="utf-8"))
    now = datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    entries = []
    for index, artifact in enumerate(artifacts):
        content = artifact["bytes"]
        checksum = hashlib.sha256(content).hexdigest()
        # Self-authored arbitrary BINARY fixtures have no declared model role.
        descriptor = {"relative_path": artifact["name"], "size_bytes": str(len(content)), "sha256": checksum, "required": True, "role": "other"}
        canonical = [{"relative_path": descriptor["relative_path"], "size_bytes": len(content), "sha256": checksum, "required": True}]
        manifest_hash = hashlib.sha256(json.dumps(canonical, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()
        model = copy.deepcopy(template["responses"]["source.search"]["items"][0])
        revision = copy.deepcopy(template["responses"]["source.list_revisions"]["items"][0])
        inspection = copy.deepcopy(template["responses"]["source.get_model"])
        model.update(id=str(uuid.uuid7()), revision_id=str(uuid.uuid7()), created_at=now, updated_at=now, format="BINARY", capabilities=[artifact["capability"]])
        model["metadata"] = {"display_name": f"Reference model {index}", "vendor_unknown": {"new": [True, None, 3]}}
        revision.update(id=model["revision_id"], distribution_id=str(uuid.uuid7()), created_at=now, updated_at=now, revision_key="content:" + manifest_hash, manifest_hash=manifest_hash, upstream_commit=None)
        inspection.update(format="BINARY", capabilities=model["capabilities"], artifacts=[descriptor], limits={"vendor_unknown": {"context": None}}, evidence_refs=["reference:self-authored-test-data"], license=None)
        entries.append({"upstream_namespace": "reference", "upstream_model_id": f"fixture-{index}", "model": model, "inspection": inspection, "revision": revision, "downloads": [{"relative_path": artifact["name"], "url": base_url + f"/artifact/{index}", "etag": '"' + checksum + '"'}], "original": {"tokenizer": {"unknown": [1, None]}, "architecture": None, "new_vendor_field": {"nested": True}, "api_token": "fixture-secret-never-published"}})
    catalog = {"schema_version": "1.0", "source_id": source_id, "entries": entries}
    (work / "catalog/catalog.json").write_text(json.dumps(catalog, ensure_ascii=False, indent=2), encoding="utf-8")
    manifest["package_sha256"] = package_hash(root)
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    return manifest, config, catalog
