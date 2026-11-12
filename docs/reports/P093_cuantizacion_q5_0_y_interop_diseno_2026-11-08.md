# P093 — cuantización real Q5_0 y corrección de interop del empaquetado Q4_0: diseño

Fecha: 2026-11-08. Quinta fase de conversores. Dos objetivos: (1) añadir Q5_0 (ggml type 6) al enum de cuantización, completando la familia de bloques simples {Q4_0, Q5_0, Q8_0}; (2) **corregir un defecto de interoperabilidad descubierto en Q4_0 al contrastar con la referencia real de ggml** (`ggml-quants.c`, llama.cpp master).

## Defecto de interop en Q4_0 (corregir en esta fase)

Nuestra implementación de Q4_0 empaquetaba los cuantizados en pares SECUENCIALES (`qs[k]` = elementos `2k` bajo + `2k+1` alto). La referencia real de ggml empaqueta pares de **mitades opuestas**: para `j` en `[0,16)`, `qs[j] = xi(x[j]) | (xi(x[j+16]) << 4)` — los elementos `j` y `j+16` comparten byte. La aritmética (escala con signo `d=max/-8`, truncado del cast C sobre `f32(v·id)+8.5`, clamp 15) es idéntica; sólo el empaquetado dentro del bloque difiere. Consecuencia: los contenedores Q4_0 publicados no serían legibles por llama.cpp real. Corrección: adoptar el empaquetado de mitades de ggml en el paquete, el oráculo focal, el espejo Rust y los goldens (regenerados). La validación congelada anterior (attempt3) queda como registro histórico del comportamiento previo.

## Aritmética Q5_0 (contrato determinista, contra la referencia real de ggml)

Bloque de QK5_0=32 elementos, 22 bytes: `d` (f16 RNE) + `qh[4]` (32 bits de 5º bit, LE) + `qs[16]`.

1. `max` = valor con signo de mayor magnitud (empate: el primero, comparación estricta). `d = f32(f64(max)/-16.0)`; `id = d==0 ? 0 : f32(1.0/f64(d))`.
2. `xi = min(31, trunc(f32(f32(v·id) + 16.5)))` — un solo redondeo por operación f32; el truncado es el cast C. Por construcción `f32(v·id)+16.5 ∈ [0.5, 32.5]` → `xi ∈ [0..32]` → clamp sólo por arriba a 31.
3. **Empaquetado de mitades** (igual que Q4_0 corregido): para `j` en `[0,16)`, `qs[j] = (xi(x[j]) & 0x0F) | ((xi(x[j+16]) & 0x0F) << 4)`.
4. **Bits altos en uint32**: `qh` little-endian de 4 bytes; elemento `j` → bit `j`; elemento `j+16` → bit `j+16`.
5. Dequantización: `v ≈ d·((xi_q - 16))` con `xi_q = low|high<<4` reconstruido; error ≤ |d|/2 + épsilon f16 (cota |v−d·(xi−16)|≤|d| en el oráculo focal).
6. Misma regla por tensor y guards de Q8_0/Q4_0 (no-finito cerrado, escala f16 no representable cerrada).

## Integración

- **Paquete GGUF v1.1.0**: `quantize_q5_0_block` nuevo, `quantize_q4_0_block` corregido a mitades, enum `["Q8_0","Q4_0","Q5_0"]`, despacho por formato en `stream`/`layout`.
- **Suite del host**: espejo Rust corregido (q4_0) y nuevo (q5_0), goldens regenerados para Q4_0 y Q5_0 (`QUANT4_GOLDEN_GGUF_HEX` cambia; `QUANT5_GOLDEN_GGUF_HEX` nuevo), caso positivo de suite Q5_0.
- **Aceptación pública**: tercer ciclo (Q5_0) en el mismo test — counts [3,4,0,0], tres variantes publicadas.
- **Auditor**: re-derivación independiente de los tres formatos (98.304 bloques por formato).
- **Wrapper congelado**: mismas 34 comprobaciones; asertos actualizados a tres formatos.
