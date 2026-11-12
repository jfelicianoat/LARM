# Compilar y comprobar el escritorio

Para construir el ejecutable con la interfaz actual, desde la raíz del repositorio:

```powershell
npm run tauri --workspace @larm/desktop -- build --debug --no-bundle
```

Tauri ejecuta `npm run build` antes de empaquetar. El build del frontend genera `apps/desktop/dist/ui-build.json`, con SHA-256 de todos los fuentes y de los assets que se van a incrustar. El build de Rust comprueba ambas listas y rechaza los archivos ausentes, añadidos o cambiados con `frontend_desactualizado`. No utiliza fechas de modificación. El modo `tauri dev` conserva su servidor de desarrollo.

El ejecutable empaquetado informa de la huella de su manifiesto sin abrir una ventana:

```powershell
apps/desktop/src-tauri/target/debug/larm-desktop.exe --ui-build-info
.venv-tools/Scripts/python.exe tools/ui_bundle_real.py
```

`tools/ui_bundle_real.py --fault` prueba la compilación negativa con un asset alterado y con un fuente añadido temporalmente; restaura ambos y reconstruye un ejecutable válido. Las ventanas de prueba deben estar cerradas antes de recompilar, porque Windows bloquea los ejecutables abiertos.

El escritorio adquiere un bloqueo exclusivo por usuario antes de inicializar el WebView. Una segunda instancia sale con código 0 y «LARM ya está abierto. Usa la ventana existente». Windows libera el bloqueo al cerrar o terminar el proceso; el archivo del bloqueo puede permanecer sin impedir el siguiente arranque.

## Credenciales de integración para el desarrollo

`tools/enroll_integrations.py` acepta por entrada oculta los dos tokens facilitados para AI Broker y Agora y los guarda a través de la API de credenciales de LARM. Los secretos quedan en Windows Credential Manager (`larm/integration/ai-broker` y `larm/integration/agora`, usuario `operator`). El directorio ignorado `.local/integration-data` guarda la base de referencias; `.local/integrations.json` contiene sólo referencias, sin tokens. Ejecutarlo de nuevo conserva esas referencias.

`tools/check_integration_credentials.py` recupera exclusivamente esas dos entradas y verifica `GET http://127.0.0.1:8765/api/v1/auth/check`, con el token del Broker en cabecera y sin seguir redirecciones. No imprime los secretos. La conexión e integración funcional con Agora sigue correspondiendo a P118; guardar su token no acredita esa integración.
