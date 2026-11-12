#!/usr/bin/env python3
"""P023 - Mutaciones: cada defecto introducido a propósito tiene que hacer fallar
alguna prueba. Restaura siempre el fichero original. Resultado en
docs/reports/P023_runs/mutations.json."""
import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SDK = "crates/larm-adapter-sdk/src/declarative.rs"
GEN = "crates/larm-adapter-sdk/src/generic.rs"
CORE = "crates/larm-core/src/adapters.rs"
UNIT = [["cargo", "test", "-p", "larm-adapter-sdk", "--lib", "declarative"]]
BUILD = ["cargo", "build", "-p", "larm-adapter-sdk", "--bins", "--examples"]
# Las pruebas del host usan el binario del worker: hay que recompilarlo con la mutación.
HOST = [BUILD, ["cargo", "test", "-p", "larm-adapter-host", "--test", "declarative"]]
API = [["cargo", "test", "-p", "larm-api", "--test", "adapters"]]

STEP_TIMEOUT = 600


def kill_strays() -> None:
    """Binarios de prueba y runtimes falsos que una prueba colgada deja vivos."""
    for image in ("fake_runtime.exe", "declarative-*", "adapters-*"):
        subprocess.run(["taskkill", "/F", "/T", "/IM", image], capture_output=True)


MUTATIONS = [
    ("placeholder desconocido aceptado", SDK,
     "let slot = Placeholder::parse(&name).ok_or_else(|| {",
     "let slot = Placeholder::parse(&name).or(Some(Placeholder::Port)).ok_or_else(|| {", UNIT),
    ("llave suelta aceptada", SDK,
     "            '}' => {\n                return Err(invalid(\n                    \"placeholder_syntax\",",
     "            '}' if false => {\n                return Err(invalid(\n                    \"placeholder_syntax\",", UNIT),
    (".bat/.cmd aceptados", SDK,
     "if !path.to_ascii_lowercase().ends_with(\".exe\") {", "if false {", UNIT),
    ("metacaracteres de shell aceptados en rutas", SDK,
     "\"<>|&;$`\\\"'*?%^{}\".contains(c));\n    if ok {", "\"\".contains(c));\n    if ok {", UNIT),
    ("health no loopback aceptado", SDK,
     "if !matches!(host, \"127.0.0.1\" | \"localhost\" | \"[::1]\") {", "if false {", UNIT),
    ("query con token en la URL aceptada", SDK,
     "if rest.contains(['?', '#', '@']) {", "if false {", UNIT),
    ("unir allowlists amplía permisos", SDK,
     "            if !both(b.name()) {", "            if false && !both(b.name()) {", UNIT),
    ("secreto como valor aceptado", SDK,
     "if SECRET_HINTS.iter().any(|h| upper.contains(h)) {", "if false {", UNIT),
    ("referencia no declarada en secret_refs", SDK,
     "                    if !self\n                        .manifest\n                        .permissions\n                        .secret_refs\n                        .contains(credential_reference)",
     "                    if false && !self\n                        .manifest\n                        .permissions\n                        .secret_refs\n                        .contains(credential_reference)", UNIT),
    ("ruta fuera de read_roots aceptada", SDK,
     "        self.grants()\n            .check(&Access {\n                reads: vec![PathBuf::from(path)],\n                ..Access::default()\n            })\n            .map_err(|e| e.with_technical(format!(\"{field} fuera de read_roots\")))?;",
     "", UNIT),
    ("runtime_arguments que sustituyen MODEL_PATH", SDK,
     "if !params.runtime_arguments.is_empty() {", "if false {", UNIT),
    ("EXACT_TEXT toma la primera línea (un aviso) como versión", SDK,
     "                let t = s.trim();",
     "                let t = s.trim().lines().next().unwrap_or_default().trim();", UNIT),
    ("límite de memoria ignorado", SDK,
     "if params.memory_limit_bytes.is_some() || max_memory_bytes.is_some() {", "if false {", UNIT),
    ("documento fuera del hash del paquete", SDK,
     "manifest.package_sha256 = package_hash(&staging, &[MANIFEST_FILE])?;",
     "manifest.package_sha256 = package_hash(&staging, &[MANIFEST_FILE, DOCUMENT_FILE])?;", HOST),
    ("el runtime hereda el entorno del worker", GEN,
     "        .env_clear()\n        .envs(minimal_env())", "        .envs(minimal_env())", HOST),
    ("credencial sin resolver: se lanza igualmente sin ella", GEN,
     ".envs(resolve_env(&prepared.environment)?)", ".envs(resolve_env(&prepared.environment).unwrap_or_default())", HOST),
    ("los runtimes sobreviven al cierre de sesión del worker", GEN,
     ["        for (_, mut instance) in std::mem::take(&mut *instances) {",
      "        .stdin(Stdio::null())\n        .kill_on_drop(true);"],
     ["        for (_, mut instance) in std::mem::take(&mut BTreeMap::<String, Instance>::new()) {\n            let _ = &mut instances;",
      "        .stdin(Stdio::null());"], HOST),
    ("plan reutilizable", GEN, "            plan.used = true;\n", "", HOST),
    ("plan_hash sin comprobar", GEN, "            if plan.hash != e.plan_hash {", "            if false {", HOST),
    ("health ignora el estado HTTP esperado", GEN,
     "Ok((status, _)) if u32::from(status) != h.expected_status => {",
     "Ok((status, _)) if false && u32::from(status) != h.expected_status => {", HOST),
    ("proceso muerto no es FAILED", GEN,
     "            if matches!(i.child.try_wait(), Ok(Some(_))) {", "            if false {", HOST),
    ("sondeo devuelve estable", GEN, "let items = if q.channel == Channel::Custom {", "let items = if true {", HOST),
    ("alta sin comprobar rutas reales", CORE, "        doc.check_paths()?;\n", "", API),
    ("declarativo nace confiable", CORE, "trust: AdapterTrust::Untrusted,", "trust: AdapterTrust::Verified,", API),
    ("conflicto de versión ignorado", CORE,
     "if self.repo.find(&m.id, &m.version).await?.is_some() {", "if false {", API),
]


def main() -> int:
    results = []
    for name, rel, old, new, cmd in MUTATIONS:
        path = REPO / rel
        original = path.read_bytes().decode("utf-8")
        olds, news = (old, new) if isinstance(old, list) else ([old], [new])
        crlf = "\r\n" in original
        norm = original.replace("\r\n", "\n")
        if any(norm.count(o) != 1 for o in olds):
            results.append({"mutation": name, "file": rel, "result": "ERROR", "detail": "patrón no único"})
            print(f"  [ERROR] {name}: patrón no único")
            continue
        mutated = norm
        for o, n in zip(olds, news):
            mutated = mutated.replace(o, n)
        if crlf:
            mutated = mutated.replace("\n", "\r\n")
        out, code = "", 0
        try:
            path.write_bytes(mutated.encode("utf-8"))
            for step in cmd:
                try:
                    proc = subprocess.run(step, cwd=REPO, capture_output=True, text=True, encoding="utf-8",
                                          errors="replace", timeout=STEP_TIMEOUT)
                except subprocess.TimeoutExpired as exc:
                    # Una prueba colgada tampoco pasa: la mutación queda detectada.
                    out += f"TIMEOUT {STEP_TIMEOUT}s: {' '.join(step)}\n" + str(exc.stdout or "")
                    code = -1
                    kill_strays()
                    break
                out += proc.stdout + proc.stderr
                code = proc.returncode
                if code != 0:
                    break
        finally:
            path.write_bytes(original.encode("utf-8"))
        compiled = "error[E" not in out and "could not compile" not in out and "\nerror: " not in out.split("test result")[0]
        # Detectada = una prueba falla de verdad (o se cuelga), no un exit≠0 cualquiera.
        killed = code != 0 and compiled and (" FAILED" in out or "panicked" in out or "TIMEOUT" in out)
        failing = [l.split()[1] for l in out.splitlines() if l.startswith("test ") and l.rstrip().endswith("FAILED")]
        results.append({"mutation": name, "file": rel, "command": " && ".join(" ".join(c) for c in cmd),
                        "result": "KILLED" if killed else ("NO_COMPILA" if not compiled else "SURVIVED"),
                        "failing_tests": failing})
        print(f"  [{results[-1]['result']}] {name}: {', '.join(failing) or '-'}")
    # El binario del worker vuelve a compilarse con el código original.
    restored = subprocess.run(BUILD, cwd=REPO, capture_output=True)
    if restored.returncode != 0:
        print("  [ERROR] no se pudo recompilar el worker original")
        return 1
    out_dir = REPO / "docs/reports/P023_runs"
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "mutations.json").write_text(json.dumps(results, indent=2, ensure_ascii=False), encoding="utf-8")
    bad = [r for r in results if r["result"] != "KILLED"]
    print(f"{len(results) - len(bad)}/{len(results)} mutaciones detectadas")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    raise SystemExit(main())
