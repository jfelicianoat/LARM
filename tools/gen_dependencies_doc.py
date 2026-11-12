#!/usr/bin/env python3
"""Genera docs/architecture/dependencies.md a partir de docs/contracts/crate-graph.json.

  gen_dependencies_doc.py          reescribe el documento
  gen_dependencies_doc.py --check  exit 1 si el documento no coincide con el contrato

El contrato es la fuente; este script nunca lo modifica.
"""
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
GRAPH = REPO / "docs/contracts/crate-graph.json"
DOC = REPO / "docs/architecture/dependencies.md"


def render() -> str:
    c = json.loads(GRAPH.read_text(encoding="utf-8"))
    allowed = c["allowed_dependencies"]
    dependents = {name: sorted(k for k, v in allowed.items() if name in v) for name in allowed}
    rows = "\n".join(
        f"| `{name}` | {', '.join(f'`{d}`' for d in deps) or '—'} | {', '.join(f'`{d}`' for d in dependents[name]) or '—'} |"
        for name, deps in allowed.items()
    )
    return f"""# Fronteras de dependencias entre crates

Generado por `tools/gen_dependencies_doc.py` desde [crate-graph.json](../contracts/crate-graph.json),
copia íntegra del contrato del paquete v3. No editar a mano: cambiar el grafo exige nueva revisión del paquete.
Verificación: `tools/check_architecture.ps1` contra `cargo metadata` del workspace y del shell Tauri.

## Grafo permitido

Cada crate solo puede depender (normal, dev o build) de los indicados. Lo no listado está prohibido.

| Crate | Puede depender de | Lo usan |
|---|---|---|
{rows}

## Reglas

- **Raíz de composición**: `{c['composition_root']}` puede depender de todos los crates; ningún crate depende de `apps/*`.
- **Dominio puro**: `larm-domain` no depende de {', '.join(c['forbidden_domain_dependencies'])}. El verificador
  traduce cada categoría a crates concretos (p. ej. IO → tokio, reqwest, tempfile; Windows → windows, winreg)
  y rechaza además `std::fs`, `std::net` y `std::process` en su código.
- **GUI**: {c['gui_boundary']}. `apps/desktop` no depende de ningún crate interno.
- **Puertos**: los traits que implementa un crate que no puede ver `larm-core` (p. ej. `larm-runtime`) viven en
  `larm-domain::ports`; `larm-core` los reexporta para `larm-api`.
"""


def main(argv: list[str]) -> int:
    text = render()
    if argv[:1] == ["--check"]:
        same = DOC.is_file() and DOC.read_text(encoding="utf-8") == text
        print("PASS documento coincide con el contrato" if same else "FAIL docs/architecture/dependencies.md no coincide con crate-graph.json")
        return 0 if same else 1
    DOC.write_text(text, encoding="utf-8")
    print(f"escrito {DOC.relative_to(REPO)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
