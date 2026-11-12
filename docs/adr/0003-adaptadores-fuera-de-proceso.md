# ADR-0003 — Adaptadores out-of-process

**Estado**: aceptado · **Fecha**: 2026-10-10 · **Prompt**: P002

## Contexto
Los 20 runtimes y las fuentes son binarios/servicios heterogéneos; un crash o hang de un runtime no puede tumbar el daemon (contrato 03).

## Decisión
- Los adaptadores se ejecutan fuera de proceso (worker/hijo o servicio externo ya presente en el host); `larm-adapter-host` orquesta con timeout, cancelación, detección de crash y reinicio según contrato.
- El protocolo adaptador v1 (P020) es la única interfaz; no se enlazan librerías nativas de runtimes dentro del proceso del daemon.

## Consecuencias
- Cada interacción con un runtime paga una frontera de proceso; aceptado por aislamiento.
- Los timeouts/crash son caminos de primera clase con tests de fault injection.
