# P093 — decisión del operador: cuantización ONNX (P065) espera el runtime Python gestionado

Fecha: 2026-11-13. Registro de la decisión de alcance del contrato 00.

## La decisión

Ante la disyuntiva planteada al operador — implementar una cuantización int8 weights-only en stdlib dentro del conversor ONNX de producto frente a esperar el runtime Python gestionado para el flujo Olive real — el operador eligió: **esperar el runtime Python para Olive real**.

## Consecuencias fijadas

1. La fase P065 (cuantización ONNX vía Olive) queda **BLOQUEADA** por el runtime Python gestionado.
2. No se implementa la alternativa stdlib ni se registra en el catálogo de operaciones.
3. El conversor ONNX de producto v1.0.0 mantiene su alcance vigente: copia verbatim de initializers (F32/F16/BF16/I64/I32/I16/I8) sin recuantizar.
4. El enum de cuantización del conversor GGUF (Q8_0/Q4_0/Q5_0/Q4_K/Q5_K/Q6_K) no se amplía hacia ONNX.
5. Cualquier propuesta futura de cuantización ONNX distinta de Olive requiere nueva decisión del operador.

## Estado de las fases del plan

Todas las fases restantes del plan están bloqueadas por sus dependencias externas:

| Fase | Dependencia externa que falta |
|---|---|
| P065 cuantización ONNX (Olive) | Runtime Python gestionado |
| P060/P099 runtimes reales | Builds de Windows de terceros |
| P044 Descubrir aceptación | Fuentes upstream reales |
| P110-112 Benchmarks | Benchmark real cualificado |

El estado integral del workspace está en verde ([gate de consolidación](P093_gate_consolidacion_2026-11-12.md), [estabilización](P093_flaky_workspace_2026-11-12.md)): 142 suites OK / 1631 pruebas Rust, UI 157/157, contratos a 3.14.0.

## Con lo ya construido

Las catorce fases anteriores dejan: los seis formatos de cuantización GGUF con interop verificada contra llama.cpp, el conversor ONNX verbatim, el linaje estructural de cuantización, la promoción P120 con exclusividad de DEFAULT, la revisión contractual 3.14.0, la UI completa (transformar/cancelar/promocionar/descubrir/inicio) y el gate workspace estabilizado. El reanudar P065 cuando el runtime Python gestionado esté disponible partirá del diseño `P093_product_converters_design_2026-11-07.md` y de esta decisión.
