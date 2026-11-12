# Revalidación de las incidencias de LARM — 16 de octubre de 2026

Las siete incidencias de la auditoría del 15 de octubre quedan corregidas dentro de la cobertura ejecutada. La fase de correcciones pasa sus comprobaciones de código, pruebas reales y comprobaciones de empaquetado. El desarrollo de las fases posteriores continúa pendiente; este informe no acredita REAL_PASS de módulos o dependencias que no lo tengan.

[Resultados y hashes de las transcripciones](AUDIT_FIXES_2026-10-16.checks.json) · [estado del proyecto](../implementation/STATE.md) · [compilación y credenciales](../operations/desktop-build.md)

## Cambios y prueba de cada incidencia

| Incidencia | Comportamiento corregido | Comprobación |
|---|---|---|
| INC-01: reserva imposible | Alta y edición rechazan una reserva superior a la capacidad conocida con 400 `reservation_exceeds_capacity`. Conservan fila, revisión, eventos y presupuesto. La reserva igual a capacidad sigue siendo válida. | API y SQLite reales; 13 pasos de regresión de almacenamiento, incluidas rutas UNC, junction, carpetas ausentes, concurrencia y limpieza. |
| INC-02: datos antiguos en pantalla | Cancelar un formulario recarga las ubicaciones. Botón «Actualizar ubicaciones». El rechazo conserva los campos editados. | Pruebas de interfaz con cambio realizado por otro actor, vuelta desde el formulario y actualización manual. |
| INC-03: desbordamiento | Las columnas y formularios pueden contraerse; los datos pasan a una columna y las tablas se ajustan en ventanas estrechas. Se corrigió también el desbordamiento de Hardware descubierto al revisar la solución. | Frontend compilado en Chromium, con observaciones reales como fixtures: Almacenamiento a 800 px, formulario a 375 px y Hardware a 800/375 px. `scrollWidth == clientWidth` en todos los casos. |
| INC-04: método incorrecto | Ruta conocida con método inválido devuelve 405 con `Allow` y el envelope de error. HEAD conserva la autorización de GET. | Nueve pruebas de la aplicación HTTP y `POST /health/live` contra el daemon real. Se conserva el contrato existente del límite de cuerpo. |
| INC-05: escritorio con interfaz antigua | El frontend genera un manifiesto SHA-256 de fuentes y assets. El empaquetado verifica ambos y publica su huella en `--ui-build-info`. | Ejecutable y frontend coinciden: 24 fuentes y 4 assets. Dos compilaciones negativas rechazan un asset alterado y un fuente añadido; se restauran los archivos y se reconstruye el ejecutable válido. |
| INC-06: huellas provisionales del Broker | Catálogo y leases consultan el último perfil persistido. Publican hardware y drivers observados con referencia a la revisión del perfil. Un conjunto incompleto de drivers permanece UNAVAILABLE. | Catálogo real de Ollama frente al perfil de hardware real: huella idéntica y los dos drivers observados. Prueba adicional de cambio de perfil en el mismo puente, coherencia de la lease y valores desconocidos. |
| INC-07: segunda ventana en pánico | Bloqueo exclusivo por usuario adquirido antes del WebView. Segunda instancia: salida 0 con mensaje claro; al cerrar la primera se libera el bloqueo. | Dos procesos reales, el segundo con flags distintos de WebView2; la primera ventana permanece viva. Prueba de exclusión y recuperación del bloqueo. |

## Validación ejecutada

- Las doce puertas comunes de `tools/check.ps1`: formato, Clippy con errores ante avisos, suite de Rust completa, arquitectura, integridad del paquete, comprobaciones del shell Tauri y lint, tipos, pruebas y build del frontend. Todas PASS; 31 pruebas de interfaz.
- Regresiones específicas de API y del puente, 13 pasos de almacenamiento real y seis pasos sobre API, hardware, catálogo y doble instancia.
- Ventana nativa real sin daemon y con daemon: WebView → transporte Tauri → pipe de bootstrap → API; salida limpia. Se revocan los principales de prueba.
- Compilaciones negativas del escritorio y comparación de la huella del ejecutable reconstruido con la del frontend.
- Coherencia de las 45 dependencias usadas y validación de las evidencias de prompts.
- P000 revalidado: 15 PASS, cero FAIL y una prueba bloqueada. El acceso autenticado al Broker ya pasa.

Las transcripciones completas, sus comandos y SHA-256 están en la evidencia. La primera ejecución de una prueba del puente dentro del entorno restringido no pudo conectarse a su servidor de loopback; la ejecución autorizada fuera de esa restricción pasó. También se corrigieron dos problemas del procedimiento: solicitar el sondeo real antes de comparar el catálogo, y no intentar revocar un principal de escritorio inexistente durante el enrolamiento de credenciales.

Para la revisión visual no se obtuvo la captura mediante el control de la ventana nativa: la solicitud de acceso de esa herramienta caducó. Se validó el layout con **los mismos JS/CSS compilados**, en una vista aislada sin credenciales y con observaciones del daemon como fixtures. Las pruebas funcionales de la ventana nativa sí se ejecutaron; la vista aislada no se presenta como prueba de integración nativa.

[Almacenamiento a 800 px](AUDIT_FIXES_2026-10-16_runs/almacenamiento-800.png) · [formulario a 375 px](AUDIT_FIXES_2026-10-16_runs/formulario-375.png) · [Hardware a 800 px](AUDIT_FIXES_2026-10-16_runs/hardware-800.png)

## Credenciales y alcance

Los tokens facilitados se enrolaron mediante la API P009 y se recuperaron desde Windows Credential Manager. El Broker real responde 200 a `auth/check`, con `authenticated=true` y `auth_required=true`. No se siguen redirecciones al verificarlo. La revisión de 1233 archivos de código, datos operativos, documentación, builds y logs no encontró ninguno de los dos tokens, ni en UTF-8 ni en UTF-16LE.

Las referencias quedan en la carpeta ignorada `.local`, sin secretos. Agora tiene su credencial guardada; su integración funcional sigue pendiente de P118. No se modificaron los proyectos ni la configuración de producción del Broker o Agora. Las pruebas usan datos aislados y no cargan ni descargan los modelos del usuario. Se detuvo únicamente el daemon residual de la auditoría anterior, después de verificar su ejecutable y su data-root de prueba, para poder recompilar.

No hay migraciones nuevas. El próximo incremento del índice es **P024**, instalación por versiones y rollback de adaptadores. Persisten los bloqueos declarados para las pruebas que requieren el PC principal, otro volumen, un segundo usuario de Windows o el fixture de habla de ASR. Los estados BLOCKED_REAL correspondientes se conservan.
