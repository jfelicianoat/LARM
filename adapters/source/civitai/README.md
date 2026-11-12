# CIVITAI source adapter (P049)

API Site v1 pública para metadatos, modelos, versiones y cursor; planes y transferencias quedan deshabilitados hasta integrar P045.

Official reference: https://github.com/civitai/civitai-developer-docs/blob/main/site/reference/models.md

The adapter owns a distinct generated manifest and profile. It never calls a provider API unless that origin is declared in the generated manifest. Supported methods: source.search, source.get_model, source.list_revisions, source.get_manifest, source.estimate_size.
