# KAGGLE source adapter (P049)

El CLI oficial ofrece comandos de modelos, pero este perfil espera credenciales resueltas por P045 y una calificación aislada de llamadas, paginación y planes antes de anunciar soporte.

Official reference: https://github.com/Kaggle/kaggle-cli/blob/main/docs/models.md

The adapter owns a distinct generated manifest and profile. It never calls a provider API unless that origin is declared in the generated manifest. Supported methods: none.
