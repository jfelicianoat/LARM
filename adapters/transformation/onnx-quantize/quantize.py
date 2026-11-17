"""Script de cuantización que ejecuta el runtime gestionado python-olive.

El worker del paquete (stdlib) lanza este script como subproceso con el
intérprete sellado del runtime (OLIVE_RUNTIME/Scripts/python.exe); aquí
corren olive/torch/onnx. El resultado es un JSON en la última línea de
stdout: {status, sha256, size_bytes, warnings}; sin otra salida.
"""
import hashlib
import json
import os
import sys
from pathlib import Path

def main() -> int:
    if len(sys.argv) != 4:
        print(json.dumps({"status": "error", "reason": "arguments_invalid"}), flush=True)
        return 2
    input_path, output_path, precision = sys.argv[1], sys.argv[2], sys.argv[3]
    if precision != "INT8":
        print(json.dumps({"status": "error", "reason": "quant_unsupported"}), flush=True)
        return 2

    # Import de olive tras validar los argumentos: los errores de entorno
    # salen como JSON de fallo controlado, no como traceback del worker.
    import olive.workflows as workflows

    output_dir = str(Path(output_path).parent)
    run_config = {
        "input_model": {"type": "OnnxModel", "config": {"model_path": input_path}},
        "systems": {"local_system": {"type": "LocalSystem", "config": {"accelerators": [
            {"device": "cpu", "execution_providers": ["CPUExecutionProvider"]}]}}},
        "evaluators": {},
        "passes": {"quant": {"type": "OnnxDynamicQuantization", "config": {"precision": "int8"}}},
        "target": "local_system",
        "output_dir": output_dir,
    }
    try:
        workflows.run(run_config)
    except Exception as error:  # noqa: BLE001 — el fallo viaja controlado al worker
        print(json.dumps({"status": "error", "reason": "olive_run_failed",
                          "detail": str(error)[:400]}), flush=True)
        return 2

    # Olive escribe el modelo cuantizado (y su footprint/cache) bajo
    # output_dir — que es el staging del worker.
    produced = [path for path in Path(output_dir).rglob("*.onnx")]
    if not produced:
        print(json.dumps({"status": "error", "reason": "olive_output_missing"}), flush=True)
        return 2
    produced.sort(key=lambda path: path.stat().st_mtime, reverse=True)
    model_path = Path(produced[0])
    data = model_path.read_bytes()
    output_path_obj = Path(output_path)
    if model_path.resolve() != output_path_obj.resolve():
        output_path_obj.write_bytes(data)
    # El runtime deja su footprint/cache junto al modelo: el staging queda
    # sólo con el modelo cuantizado.
    import shutil
    for extra in Path(output_dir).iterdir():
        if extra == output_path_obj:
            continue
        if extra.is_dir():
            shutil.rmtree(extra, ignore_errors=True)
        else:
            extra.unlink(missing_ok=True)
    print(json.dumps({
        "status": "ok",
        "sha256": hashlib.sha256(data).hexdigest(),
        "size_bytes": str(os.path.getsize(output_path_obj)),
        "model_name": model_path.name,
    }), flush=True)
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
