# P093 — cuantización real Q5_0 y corrección de interop del empaquetado Q4_0

Fecha: 2026-11-08. Quinta fase de conversores según el [diseño fijado](P093_cuantizacion_q5_0_y_interop_diseno_2026-11-08.md): el enum de cuantización pasa a `["Q8_0","Q4_0","Q5_0"]` (familia completa de bloques simples de ggml) y se corrige un **defecto de interoperabilidad real** descubierto al contrastar el Q4_0 propio con la referencia oficial de ggml (`ggml-quants.c` de llama.cpp).

## Defecto de interop descubierto y corregido (Q4_0)

Nuestro Q4_0 empaquetaba pares SECUENCIALES (elementos 2k/2k+1 por byte). La referencia real de ggml empaqueta pares de MITADES: el byte `j` une los elementos `j` (nibble bajo) y `j+16` (nibble alto) del bloque. La aritmética de escala y truncado era correcta; el orden de bytes dentro del bloque no — los contenedores publicados no serían legibles por llama.cpp real. La conformidad cruzada Python↔Rust no lo podía destapar (ambos lados reproducían el MISMO contrato equivocado); lo reveló la contrastación externa con el código de referencia. Corregido en paquete, oráculo focal, espejo Rust y goldens (regenerados).

## Implementación Q5_0

- **Paquete GGUF v1.1.0** (`gguf.py`): `quantize_q5_0_block` — type 6, 22 bytes por bloque (escala f16 con signo `d=f32(max/-16)` + uint32 LE de bits altos `qh` + 16 bytes de nibbles con empaquetado de mitades). Aritmética del diseño: `xi = min(31, trunc(f32(f32(v·id)+16.5)))`; el 5º bit del elemento `k` va al bit `k` de `qh`. Misma regla por tensor y guards (no-finito cerrado, escala f16 no representable cerrada).
- **Oráculo focal** (7 pruebas): referencia Q5_0 independiente + contenedor publicado byte-exacto + cota semántica |v−d·(xi−16)|≤|d| reconstruyendo `xi` desde nibbles y bits altos; referencias Q4_0/Q8_0 re-validadas con el empaquetado corregido.
- **Suite del host**: `quantize_q5_0_block` en Rust, `expected_gguf_quantized` parametrizado a tres formatos, goldens regenerados (Q4_0 cambia de bytes; Q5_0 nuevo — 5 constantes en `gguf_golden.rs`), caso positivo de suite Q5_0 (plan → execute → contenedor byte-exacto + types + file_type 7). 60 pruebas del crate.
- **Aceptación pública TRIPLE**: tres ciclos completos (Q8_0 → Q4_0 → Q5_0) en la misma sesión de daemon — counts [3,4,0,0], variantes de 3.34 MB / 1.77 MB / 2.16 MB publicadas por CAS, cada contenedor byte-exacto contra su emisión espejo.

## Validación congelada

[attempt4](P093_runs/public-gguf-quant-20261108/attempt4/verification-synopsis.json) — **516 pruebas PASS / 0 FAIL / 0 ignoradas, 34/34 comprobaciones**, auditor independiente PASS que re-deriva en Python **294.912 bloques** (98.304 por formato, formatos verificados {2,6,8}) sobre **7.275.816 bytes** de contenedores cuantizados retenidos, bloque a bloque contra referencias independientes de los tres formatos — incluyendo el empaquetado de mitades corregido y los bits altos uint32 de Q5_0. La fase pasó a la primera en la validación congelada (pre-vuelos de fmt/clippy/arquitectura/copias y validación del auditor contra evidencia previa).

## Límites conservados

Super-bloques K (Q4_K/Q5_K con escalas y mínimos de 8/6 bits), cuantización ONNX/int8 (Olive/P065), aceptación de runtimes y UI siguen pendientes. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
