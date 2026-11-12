# Schemas de las 40 firmas — preparación P026

Generados por tools/p026_schemas.py desde adapter-methods.json y ambos schemas
canónicos. Hay 80 documentos: request/response por firma. index.json conserva
familia, required, referencia original y hashes de fuentes/archivos. No editar
estos JSON; write regenera y check exige nombres/bytes exactos.

Cada documento conserva todas las restricciones del tipo original y sólo sus
$defs alcanzables. Las referencias se reescriben a fragmentos locales; no hay
resolución de red. Una referencia externa o semántica de scope no soportada
impide generar, sin fallback permisivo. El validador sigue siendo JSON Schema
Draft 2020-12 con la extensión publicada x-maximum-decimal; no se implementó un
motor alternativo. El índice Rust generado se embebe en el binario. Python exige
hashes/bindings y los mismos 80 schemas; no hay resolución externa ni fallback.

```powershell
.venv-tools/Scripts/python.exe tools/p026_schemas.py write
.venv-tools/Scripts/python.exe tools/p026_schemas.py check
.venv-tools/Scripts/python.exe tools/test_p026_schemas.py
.venv-tools/Scripts/python.exe tools/p026_prepare_evidence.py
```

Las tres pruebas contrastan bytes/hashes, un documento válido para cada una de
las 80 direcciones, mutaciones y fixtures compartidas contra jsonschema con
los contratos originales y resolución independiente. El generador de corpus
no es un worker ni demuestra conducta funcional. Los SDK ya usan los documentos
y comparten 8.544 casos contra el original con la extensión decimal. La preparación
no acredita estimadores/capacidades, cinco workers, promoción ni P026 completa.
