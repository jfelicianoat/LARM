# ADR-0001 — Windows-first

**Estado**: aceptado · **Fecha**: 2026-10-10 · **Prompt**: P002

## Contexto
LARM administra capacidad de IA local en el PC IA con Windows; la spec (secciones 3, 4 y 127) exige profilers de Windows (GPU/NPU, volumen GUID, servicios) y excluye la portabilidad multiplataforma como objetivo.

## Decisión
- Objetivo primario: Windows 11 (build 26200+ verificado en P000). El IO asume Windows (rutas, volúmenes GUID, Credential Manager).
- El código específico de plataforma vive en `larm-hardware`/`larm-storage`; el dominio (`larm-domain`) permanece puro y sin dependencias de plataforma.
- WSL2/contenedores se exponen como execution hosts abstraídos (P081), no como portabilidad del producto.

## Consecuencias
- No se mantienen backends alternativos (macOS/Linux); los cambios que los añaden son cambio de alcance y se rechazan.
- Las pruebas reales de cada prompt corren sobre Windows/PC IA real.
