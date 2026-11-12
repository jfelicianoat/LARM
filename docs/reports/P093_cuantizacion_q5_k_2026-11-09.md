# P093 — cuantización real Q5_K (super-bloques K de 5 bits) en el conversor GGUF de producto

Fecha: 2026-11-09. Séptima fase de conversores según el [diseño fijado](P093_cuantizacion_q5_k_diseno_2026-11-09.md): el enum pasa a `["Q8_0","Q4_0","Q5_0","Q4_K","Q5_K"]`. Q5_K (type 13) reutiliza TODA la maquinaria K ya espejada bit-exacto (`make_qkx2_quants`, `nearest_int`, `get_scale_min_k4`, escalas 6-bit) cambiando sólo los parámetros de búsqueda (nmax=31, rmin=−0.5, nstep=15), los niveles de 5 bits (clamp 0..31 en la re-cuantización final) y el empaquetado: byte de nibbles por mitades + 5º bit del elemento (n+j)/(n+j+32) en los bits 2·chunk/2·chunk+1 de `qh[j]`. Bloque de 176 bytes.

## Implementación

- **Paquete GGUF v1.1.0** (`gguf.py`): `quantize_q5_k_block` + regla de super-bloques compartida con Q4_K (total múltiplo de 256), guards heredados cerrados.
- **Oráculo focal** (9 pruebas): referencia Q5_K independiente + contenedor publicado byte-exacto + cota semántica con reconstrucción completa de L (nibble + bit alto) por sub-bloque.
- **Suite del host**: espejo Rust directo (62 pruebas del crate), golden `QUANT5K_GOLDEN_GGUF_HEX` (fixture K reutilizado), caso positivo de suite Q5_K.
- **Aceptación pública QUINTUPLE**: cinco ciclos completos — counts [5,6,0,0], variantes de 3.34 / 1.77 / 2.16 / 1.77 / 2.16 MB publicadas por CAS, cada contenedor byte-exacto contra su emisión espejo. Negativos: 404 / 422 (Q6_K) / 201.
- **Auditor**: re-derivación de los cinco formatos (98.304×4 simples + 12.288×2 super-bloques K), validada contra la evidencia real antes de la congelada.

## Defectos y calibraciones de la fase

1. **Lista de permitidos del oráculo Rust sin Q5_K**: la regla `quantized_tensor` actualizó la rama de super-bloques pero la guarda inicial seguía rechazando el formato nuevo → contenedor verbatim con file_type 0; destapado por el golden cruzado (misma clase multi-sitio de la fase Q4_K, registrado en memoria).
2. **Techo de timeout del executor de sondas**: la suite con DOS formatos K supera los 10 minutos (Q4_K ~300 s + Q5_K ~290 s por las dobles conversiones de la ruta de emisión única) — el perfil de cualificación quedó cortado por tres caps de 600 s independientes (conformance.rs, qualification.rs, profiles.rs), todos elevados a 1200 s como detectores de cuelgue; el timeout_seconds del perfil pasó a 1200.
3. El negativo de enum inválido sigue siendo `Q6_K` (entra Q5_K).

## Validación congelada

[attempt14](P093_runs/public-gguf-quant-20261108/attempt14/verification-synopsis.json) — **516 pruebas PASS / 0 FAIL / 0 ignoradas, 34/34 comprobaciones**, auditor independiente PASS que re-deriva en Python **319.488 bloques** (98.304 por cada formato simple + 12.288 super-bloques por cada uno de los dos K, formatos verificados {2,6,8,13}) sobre **11.208.856 bytes** de contenedores cuantizados retenidos. Attempts 10–13 conservados como FAIL: attempt10 por el bug del auditor sin stride para type 13 (su arreglo se validó con simulación `attempt10-sim/` antes de relanzar — patrón establecido), attempt11 por la carrera intermitente de re-despacho QUEUED bajo carga, y attempts 12–13 por rpc_timeout en los RPC de 300 s de las fases K (la degradación acumulada de la máquina superó el presupuesto de un solo RPC). **Lección de la fase**: la calibración de timeouts es una CADENA coherente (RPC individual → plan preview → perfil → deadline del test), no timeouts aislados — un solo eslabón corto estrangula toda la suite; la cadena completa quedó en RPC K 600 s, plan preview 600 s, caps 2400 s, deadline del test 2400 s.

## Límites conservados

Q6_K (último super-bloque K), cuantización ONNX/int8 (Olive/P065), aceptación de runtimes reales (bloqueada P060/P099) y UI siguen pendientes. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
