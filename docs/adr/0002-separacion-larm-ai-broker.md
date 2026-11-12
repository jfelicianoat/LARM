# ADR-0002 — Separación LARM / AI Broker

**Estado**: aceptado · **Fecha**: 2026-10-10 · **Prompt**: P002

## Contexto
El AI Broker (otro repositorio, en el PC IA) conserva routing y gateway de inferencia; LARM administra capacidad local. La spec prohíbe crear chat, RAG, router de negocio ni evaluador funcional dentro de LARM.

## Decisión
- LARM expone contratos (P101) y leases (P100); el Broker consulta LocalRuntimeResolver (P107). LARM nunca enruta conversaciones de negocio.
- Los snapshots de Broker/Agora/Lab/Drift en `docs/contracts/external/` son obligatorios en integraciones; LARM no los reimplementa.
- Los cambios en repos externos (P107/P108/P118) se ejecutan como parches del repositorio del Broker con su contexto en `Contexto_Broker_P107_v3`.

## Consecuencias
- Cualquier cambio que introduzca routing de negocio en LARM viola la frontera y se rechaza en revisión de arquitectura.
