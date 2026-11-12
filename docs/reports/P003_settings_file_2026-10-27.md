# P003/P051 — Ajustes efectivos y fichero de configuración

Estado global: `BLOCKED_REAL`. Incremento focal: **49 pruebas de API/configuración/persistencia y dos pruebas nativas PASS**. Settings TCP pasa 1/1 en 6,54 s; descarga P048 TCP pasa 1/1 en 182,98 s. Observaciones finales: 2026-10-27 a las 14:56:42 y 15:00:20 UTC.

## Fallo reproducido y corrección

El arranque componía defaults < persistido < fichero < entorno < CLI, mientras Settings API omitía el fichero. Con `null` explícito en el fichero y 65.536 bytes/s persistidos, la API mostraba/aplicaba el límite hasta reiniciar; el nuevo proceso volvía a usar `null`. La prueba previa a la corrección conserva **3 PASS/2 FAIL** en [red.log](P003_runs/settings-file-20261027/red.log). Es un fallo de implementación; se conserva la precedencia existente y no se cambia el contrato.

La API recibe ahora la capa del fichero y usa `Settings::compose_with_base`, igual que el arranque. GET, respuesta de PATCH y hooks del límite/umbrales usan el valor efectivo. La solicitud validada continúa guardándose por CAS como base persistida. Una clave fijada en el fichero prevalece, incluido `null`; al retirarla y reiniciar vuelve a aplicarse el valor guardado. H09 sólo provoca reevaluación cuando cambian los umbrales efectivos.

La suite API completa pasa 37/37, incluidas las dos regresiones nuevas: prioridad de las capas, `null`, límites positivos, persistencia de la intención, hook de caudal y ausencia de actualización H09 cuando el fichero conserva sus umbrales. El [resultado posterior](P003_runs/settings-file-20261027/api-all.log) conserva el contraste con el fallo inicial.

## Aceptaciones ejecutadas

La prueba TCP de Settings inicia cinco procesos del daemon con SQLite y principal temporal reales: fichero 8.192, PATCH persistido 65.536 con respuesta efectiva 8.192, reinicio con ese fichero, sustitución por `null`, retirada de la clave y actualización/reinicio a 32.768. Exige revisiones CAS 1→2→3 y exactamente dos eventos `settings.changed` con outbox; revoca el principal al terminar.

La repetición P048 usa un fichero que omite la clave del límite y lo cambia por la API. Además de repetir confirmación, rechazo de corrupción, pausa y dos reinicios hasta SHA/CAS/procedencia, comprueba que ambos reinicios siguen mostrando 65.536 bytes/s y que las lecturas de recuperación están acotadas por ese límite.

El job correcto conserva la pausa a 131.072 bytes. Después de reanudar y terminar el proceso en `RUNNING`, el checkpoint definitivo es 196.608 bytes. El daemon pide `Range: bytes=196608-262143`, conserva el ETag en `If-Range` y recibe 206. Las 14 lecturas tras recuperar están acotadas a 65.536 bytes. Finaliza `COMPLETED`, intento 3, con un evento de recuperación, SHA-256 completo de los 1.048.832 bytes, CAS/procedencia y journal/reserva/outbox cerrados. El cuerpo alterado termina `INTEGRITY_FAILED` no retryable, sin publicación/CAS/procedencia y con su parcial/reserva retirados. Ambos principales temporales quedan revocados.

Evidencia nativa: [Settings](P003_runs/settings-tcp/01a17ddd-1113-74f4-ad3b-a179bed17a22/result.json), [log Settings](P003_runs/settings-file-20261027/native-settings.log), [descarga P048](P048_runs/daemon-tcp/01a17dde-84a6-762a-a6de-ee31a819c933/result.json), [log P048](P003_runs/settings-file-20261027/p048-regression.log), [cualificación 39/16](P048_runs/daemon-tcp/01a17dde-84a6-762a-a6de-ee31a819c933/source-http.json) y [traza HTTP](P048_runs/daemon-tcp/01a17dde-84a6-762a-a6de-ee31a819c933/requests.ndjson).

Los fixtures anteriores de P051 que prueban cambios en vivo también omiten esa clave y fijan el límite inicial mediante PATCH. El caso explícito de fichero se prueba por separado; no se modifica el fichero del operador ni se usan sus credenciales.

## Verificación

- API completa 37/37, configuración 10/10 y persistencia Settings 2/2: 49 PASS. Las dos regresiones iniciales permanecen FAIL en el log previo a la corrección.
- Clippy API/daemon con todos sus targets y `-D warnings`, formato, sintaxis Python y rutas de fixtures PASS. Los avisos del compilador sobre caché/hardlinks no son lints de Clippy.
- `package_copy`: 74/74 copias idénticas al paquete 3.11.1; sin cambio contractual.
- Dos nativas 2/2 y cualificación de P048 39 casos/16 observaciones. Los 37 inputs congelados y ambos ejecutables coinciden con los recibos; [comprobación independiente](P003_runs/settings-file-20261027/increment.checks.json) e [inputs](P003_runs/settings-tcp/01a17ddd-1113-74f4-ad3b-a179bed17a22/implementation.sha256.json).

## Alcance

Estos cambios no prueban todavía el caudal agregado H08 con varias transferencias, el disco lleno físico, ingestión completa P043 ni GUI→daemon. El [ensayo TCP anterior de P048](P048_daemon_tcp_2026-10-27.md) conserva su recibo y sus 103,93 s históricos; los nuevos recibos identifican su propio binario e inputs. No se repiten frontend, workspace, las fases nativas históricas de rangos/integridad P051 ni los runners formales completos. No se afirma `REAL_PASS` de P003/P048/P051/P125.
