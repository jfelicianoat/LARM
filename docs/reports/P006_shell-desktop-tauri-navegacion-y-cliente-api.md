# P006 — Shell desktop Tauri, navegación y cliente API

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: ver [evidence.json](P006_shell-desktop-tauri-navegacion-y-cliente-api.evidence.json); depende de P005 (BLOCKED_REAL de entorno heredado). Captura: [shell sin conexión](P006_runs/shell_offline_hardware.png).

## Qué se construyó

- **Transporte seguro (contrato 07)**: el frontend no recibe el token ni la URL. El comando nativo `api_request`:
  - obtiene el token del principal `desktop` por el pipe de bootstrap (P009, DACL solo para el SID del usuario);
  - valida la base (solo `http://127.0.0.1`/`[::1]`, el token no puede salir de la máquina), el método (GET/POST/PATCH/DELETE), la ruta (`/api/v1/`, sin `..` ni `//`) y las cabeceras (nunca `Authorization` desde el WebView);
  - ante un 401 renueva el token una vez.
- **Cliente TypeScript**: `ApiError` con la forma del contrato y validación de rutas antes de salir del frontend; un fallo de transporte es `DAEMON_UNREACHABLE`.
- **Estado del daemon** CONNECTING / ONLINE / DEGRADED / OFFLINE, derivado de `health/live` y `health/ready`, con sondeo periódico.
- **14 pantallas** del contrato 08 en su orden, con navegación por hash. Cada pantalla dice qué prompt traerá su contenido; no hay datos simulados en producción. Se ve el estado sin conexión con reintento.
- Error boundary por pantalla, avisos no intrusivos (regiones `status`/`alert`) solo al cambiar la conexión, y **página de diagnóstico** con estado, error, correlación y qué comprobar.
- **Accesibilidad (WCAG 2.2 AA y la del usuario)**: texto base de 18 px, contraste ≥ 7:1, tema claro y oscuro, foco de 4 px, enlace para saltar al contenido, `aria-current`, el foco pasa al título al navegar, estado con texto y no solo color, y `prefers-reduced-motion`.
- La prueba de humo (P001) recorre ahora la cadena entera: WebView → backend → bootstrap → API.

## Pruebas

- 9 de frontend: cliente, estado, shell online/offline, reconexión, foco y humo, con un transporte falso **solo en pruebas**.
- 4 del transporte nativo.
- lint, typecheck, build y build de Tauri.
- **Real**: la ventana real **sin daemon** informa `DAEMON_UNREACHABLE`. **Con el daemon real**: `ping=pong`, `health/live` LIVE y `health/ready` READY a través del backend y el pipe de bootstrap, con el principal `desktop` creado en el momento.
- Revisión visual con Chrome headless del build de producción. Ajustado el anillo de foco de los títulos, que no son interactivos.

## Notas

- Cada conexión al pipe rota el token del escritorio: una segunda ventana de LARM invalida la primera, que se recupera sola al recibir el 401 y volver a pedir token.
- El contenido de las pantallas llega con sus prompts (P120–P126 y los dominios correspondientes).
