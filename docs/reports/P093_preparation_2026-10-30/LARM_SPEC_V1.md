# Local AI Runtime Manager — LARM

## Especificación Técnica Maestra v1.0

**Fecha:** 10 de octubre de 2026  

**Estado:** baseline arquitectónica completa  

**Plataforma primaria:** Windows  

**Ubicación prevista:** PC IA  

**Gateway de inferencia del ecosistema:** AI Broker  

**Orquestación distribuida:** Agora  

**Evaluación:** Local-AI-Lab + Model Drift

---

# 1. Visión

**Local AI Runtime Manager (LARM)** será la infraestructura local encargada de descubrir, adquirir, transformar, almacenar, ejecutar, versionar, actualizar, supervisar y medir técnicamente modelos de inteligencia artificial y los runtimes necesarios para ejecutarlos.

Su objetivo no será limitarse a LLM.

Deberá poder gestionar cualquier categoría de modelo ejecutable localmente, incluyendo:

- generación de texto;

- modelos multimodales;

- System-1 / decision models;

- embeddings;

- rerankers;

- OCR;

- visión;

- detección y segmentación;

- ASR;

- TTS;

- audio;

- generación de imágenes;

- generación de vídeo;

- modelos ONNX genéricos;

- modelos especializados;

- nuevos tipos que aparezcan en el futuro.

El principio central será:

> **LARM administra la capacidad local de inferencia.**

AI Broker mantiene otra responsabilidad:

> **AI Broker decide qué capacidad utilizar y proporciona el gateway de inferencia al resto del ecosistema.**

Por tanto, LARM no será un segundo AI Broker.

---

# 2. Objetivos fundamentales

LARM deberá:

1. Detectar completamente el hardware y software relevante del equipo.

2. Descubrir runtimes compatibles con Windows.

3. Instalar y mantener múltiples versiones de cada runtime.

4. Permitir añadir nuevos runtimes sin modificar el Core.

5. Buscar modelos en múltiples repositorios.

6. Permitir añadir nuevos repositorios sin modificar el Core.

7. Normalizar modelos procedentes de fuentes diferentes.

8. Detectar variantes, formatos y cuantizaciones.

9. Determinar qué modelos puede ejecutar la máquina.

10. Estimar memoria y almacenamiento necesarios.

11. Mostrar el espacio necesario **antes de descargar**.

12. Permitir elegir en qué unidad/carpeta se almacena cada modelo.

13. Gestionar modelos repartidos entre múltiples discos.

14. Permitir mover posteriormente un modelo a otro disco.

15. Mantener registrada siempre su ubicación física.

16. Descargar con pausa, reanudación y recuperación de errores.

17. Transformar modelos entre formatos cuando sea posible.

18. Ejecutar benchmarks técnicos.

19. Ejecutar diferentes versiones del mismo runtime.

20. Detectar regresiones entre versiones.

21. Arrancar/detener modelos bajo demanda.

22. Exponer endpoints de inferencia.

23. Exponer administración mediante API.

24. Exponer administración agéntica mediante MCP.

25. Permitir opcionalmente acceso MCP a modelos concretos.

26. Integrarse con AI Broker.

27. Integrarse con Agora.

28. Integrarse con Local-AI-Lab.

29. Integrarse con Model Drift.

30. Gestionar instalaciones existentes como Lemonade u Ollama.

31. Avisar de modelos nuevos compatibles.

32. Avisar de actualizaciones de modelos y runtimes.

33. Mantener trazabilidad completa y reproducibilidad.

---

# 3. No objetivos

LARM no será:

- un chat;

- un agente generalista;

- un sistema RAG;

- un sistema de memoria;

- un router de negocio;

- un reemplazo de AI Broker;

- un evaluador funcional de calidad;

- un sistema de workflows generales.

Esas funciones permanecen en otros componentes.

---

# 4. Topología del ecosistema

```text

                    PC PRINCIPAL

┌──────────────────────────────────────────────┐

│                                              │

│ ChatyGPT        Athena       otras apps      │

│     │             │              │           │

│     └─────────────┴──────────────┘           │

│                   │                          │

│                   │ LAN                      │

│                   ▼                          │

│                                              │

│ Local-AI-Lab                                │

│ Model Drift                                  │

│ Agora Control Plane                          │

│                                              │

└────────────────────┬─────────────────────────┘

                     │

═════════════════════│══════════════════════════

                     │

                     ▼

                       PC IA

┌──────────────────────────────────────────────┐

│                                              │

│                AI BROKER                     │

│                     │                        │

│                     │ localhost              │

│                     ▼                        │

│         LOCAL AI RUNTIME MANAGER             │

│                                              │

│ Runtime Manager                              │

│ Model Manager                                │

│ Storage Manager                              │

│ Source Discovery                             │

│ Compatibility Engine                         │

│ Hardware Profiler                            │

│ Benchmark Engine                             │

│ Process Supervisor                           │

│ Protocol Gateway                             │

│                                              │

│ llama.cpp / ONNX / WinML / Lemonade / ...   │

│                                              │

│ Agora Runner                                 │

│                                              │

└──────────────────────────────────────────────┘

```

AI Broker y LARM estarán físicamente en el PC IA.

La comunicación entre ambos deberá realizarse normalmente por `localhost`.

---

# 5. Principios arquitectónicos

## 5.1 Windows First

Windows será plataforma de primera clase.

LARM deberá ser capaz de integrar cualquier runtime que pueda ejecutarse:

- nativamente en Windows;

- mediante un entorno Python Windows;

- mediante Windows ML;

- mediante WSL2;

- mediante un contenedor disponible en Windows;

- mediante un host local externo.

La GUI deberá distinguir claramente:

```text

WINDOWS_NATIVE

WINDOWS_PYTHON

WINDOWS_ML

WSL2

CONTAINER

EXTERNAL_LOCAL

```

Un runtime Linux ejecutado mediante WSL no deberá aparecer engañosamente como runtime nativo Windows.

---

# 6. Extensibilidad doble

Existirán dos sistemas independientes de adaptadores:

```text

Runtime Adapter

```

y:

```text

Model Source Adapter

```

Esto significa:

```text

N fuentes de modelos

        │

        ▼

       LARM

        │

        ▼

M runtimes diferentes

```

Ni Hugging Face ni llama.cpp serán dependencias conceptuales del Core.

---

# 7. Modelo conceptual

Se diferenciarán estrictamente:

## Runtime / Engine

Ejecuta las operaciones matemáticas.

Ejemplos:

```text

llama.cpp

ONNX Runtime

ONNX Runtime GenAI

stable-diffusion.cpp

whisper.cpp

OpenVINO Runtime

MLC LLM

PyTorch

CTranslate2

sherpa-onnx

```

## Backend / Execution Provider

Indica sobre qué hardware se ejecuta.

Ejemplos:

```text

CPU

Vulkan

CUDA

ROCm

DirectML

MIGraphX

VitisAI

QNN

OpenVINO

TensorRT

WebGPU

SYCL

OpenCL

```

Windows ML dispone actualmente de proveedores CPU y DirectML, además de proveedores descargables como AMD MIGraphX y VitisAI, NVIDIA TensorRT RTX, Intel OpenVINO, Qualcomm QNN y WebGPU experimental.

## Host

Programa que administra uno o más motores.

Ejemplos:

```text

Lemonade

Ollama

LM Studio

Foundry Local

ComfyUI

KoboldCpp

```

## Modelo

Ejemplos:

```text

Laya

Qwen

Gemma

Whisper

FLUX

modelo ONNX

modelo OCR

modelo TTS

```

## Variante

Una representación concreta:

```text

GGUF Q4_K_M

GGUF Q8_0

ONNX INT4

ONNX FP16

SafeTensors BF16

MLC q4f16

```

---

# 8. Catálogo inicial de runtimes

La arquitectura no dependerá de esta lista.

Servirá únicamente como conjunto inicial de adaptadores oficiales.

## Prioridad alta

```text

llama.cpp

ONNX Runtime

ONNX Runtime GenAI

Windows ML

stable-diffusion.cpp

whisper.cpp

Lemonade

Ollama

MLC LLM

PyTorch / Transformers

Diffusers

sherpa-onnx

OpenVINO Runtime

Foundry Local

```

## Prioridad posterior

```text

CTranslate2

ComfyUI

LM Studio

KoboldCpp

NCNN

MNN

otros compatibles con Windows

```

La aplicación deberá disponer de un **Runtime Adapter Registry** actualizable independientemente del Core.

Así podrán aparecer nuevos adaptadores sin publicar una nueva versión completa de LARM.

---

# 9. Tecnologías verificadas actualmente

`stable-diffusion.cpp` funciona en Windows y admite actualmente CPU, CUDA, Vulkan, Metal, OpenCL y SYCL, además de formatos PyTorch, SafeTensors y GGUF.

MLC LLM soporta Windows/Linux mediante Vulkan y ofrece servidor REST compatible con OpenAI.

ONNX Runtime GenAI dispone de builds Windows y proporciona el bucle generativo necesario para LLM, incluyendo KV cache, sampling y gramáticas para tool calling.

Foundry Local funciona directamente sobre Windows, puede seleccionar variantes CPU/GPU/NPU y dispone actualmente de SDK para .NET, Python, JavaScript y Rust.

OpenVINO Runtime también dispone oficialmente de instalación Windows.

Estas tecnologías constituyen ejemplos; no forman una whitelist cerrada.

---

# 10. Runtime Adapter Architecture

Los runtimes no se integrarán con `if/else` dentro del Core.

Cada uno implementará un contrato.

```text

RuntimeAdapter

```

Operaciones:

```text

probe()

discover_installations()

discover_versions()

plan_install()

install()

update()

rollback()

uninstall()

discover_backends()

discover_models()

validate_installation()

assess_compatibility()

prepare_model()

estimate_runtime_resources()

launch()

stop()

restart()

health()

list_instances()

smoke_test()

benchmark()

get_logs()

```

No todas las operaciones serán obligatorias.

El manifiesto indicará qué capacidades existen.

---

# 11. Runtime Adapter Manifest

Ejemplo:

```yaml

id: llama-cpp

name: llama.cpp

adapter_protocol: 1

type: inference-engine

platforms:

  - windows-x64

formats:

  - gguf

capabilities:

  - text.generate

  - vision.understand

  - embedding.create

  - rerank

backends:

  - cpu

  - vulkan

  - cuda

  - rocm

  - sycl

version_source:

  type: github

  repository: ggml-org/llama.cpp

launch:

  executable: llama-server

protocols:

  - openai

  - anthropic

benchmark:

  executable: llama-bench

```

---

# 12. Adaptadores fuera de proceso

Los plugins externos no se cargarán como librerías arbitrarias dentro del Core.

Preferencia:

```text

LARM

 ↓

JSON-RPC / stdio

 ↓

Adapter Worker

```

o socket local.

Ventajas:

- aislamiento;

- posibilidad de utilizar Python;

- posibilidad de utilizar Rust/C#/Go;

- un crash del adaptador no tumba LARM;

- mayor seguridad;

- actualizaciones independientes.

---

# 13. Adaptadores declarativos

Para runtimes sencillos no será necesario programar.

La GUI permitirá definir:

```text

Nombre

Ejecutable

Argumentos

Comando de versión

Comando de arranque

Comando de parada

Endpoint

Health endpoint

Protocolos

Formatos

Variables de entorno

```

LARM generará un adaptador declarativo.

---

# 14. Model Source Registry

Hugging Face no será la única fuente.

Fuentes iniciales previstas:

```text

Hugging Face

ModelScope

Kaggle Models

Ollama Library

GitHub

OpenXLab

NVIDIA NGC

catálogos de fabricantes

Civitai

Foundry Catalog

Lemonade Registry

HTTP/HTTPS

Git/Git LFS

carpeta local

NAS/LAN

repositorios privados

```

Hugging Face posee API pública y permite obtener metadata de archivos, incluido su tamaño en bytes antes de descargarlos.

ModelScope dispone de API/SDK, búsqueda, descarga selectiva, reanudación, SHA256 y elección de directorio/caché.

Kaggle permite descarga de modelos mediante SDK, CLI o API.

Ollama mantiene su propio catálogo y actualmente contiene LLM, modelos multimodales, embeddings y modelos de decisión/System-1 como Laya.

---

# 15. Model Source Adapter

Contrato:

```text

probe()

authenticate()

logout()

search()

get_model()

get_versions()

get_artifacts()

get_metadata()

get_capabilities()

get_license()

get_files()

get_file_metadata()

estimate_download_size()

download_plan()

download()

check_updates()

verify_integrity()

```

---

# 16. Fuentes privadas

Los adaptadores deberán admitir cuando corresponda:

```text

Bearer Token

API Token

OAuth

Basic Auth

SSH

cookies/tokens específicos

```

Las credenciales no se almacenarán en texto plano en SQLite.

En Windows deberán utilizar:

```text

Windows Credential Manager

y/o DPAPI

```

---

# 17. Modelo canónico

El mismo modelo puede aparecer simultáneamente en múltiples fuentes.

No deberá mostrarse como modelos completamente distintos cuando pueda determinarse que tienen el mismo origen.

Se introducirán:

```text

CanonicalModel

ModelDistribution

ModelVariant

ModelArtifact

```

Ejemplo:

```text

Qwen-X

 │

 ├── Hugging Face

 │      └── GGUF Q5_K_M

 │

 ├── ModelScope

 │      └── SafeTensors BF16

 │

 ├── Vendor

 │      └── ONNX INT4

 │

 └── Ollama

        └── manifest

```

---

# 18. Resolución de identidad

Se utilizarán, cuando estén disponibles:

```text

arquitectura

modelo base

autor

repository metadata

config

commit

SHA256

tensor metadata

tokenizer

lineage

```

La resolución deberá aceptar grados:

```text

IDENTICAL

PROBABLE_SAME_MODEL

DERIVED_MODEL

UNRELATED

UNKNOWN

```

Nunca fusionará automáticamente dos modelos cuando la identidad sea dudosa.

---

# 19. Procedencia

Cada artefacto conservará:

```text

source

repository

original author

uploader

revision

commit

download URL

SHA256

license

gated status

base model

quantizer

conversion lineage

download date

```

La UI diferenciará:

```text

Official publisher

Verified distribution

Community conversion

Unknown origin

```

---

# 20. Capabilities Taxonomy

Las capacidades no serán un enum rígido.

Se utilizarán identificadores extensibles.

Ejemplos:

```text

text.generate

text.classify

decision.evaluate

embedding.create

rerank

vision.understand

vision.ocr

vision.detect

vision.segment

image.generate

image.upscale

video.generate

audio.transcribe

audio.translate

audio.classify

speech.synthesize

tool.call

generic.inference

```

Los adaptadores podrán registrar nuevas capacidades.

---

# 21. Model Bundle

Muchos modelos no son un único fichero.

Por ejemplo:

```text

weights

tokenizer

vision projector

VAE

text encoder

vocoder

processor

config

```

Por tanto existirá:

```text

ModelBundle

```

que agrupará todos los artefactos necesarios para una variante ejecutable.

La compatibilidad se calculará sobre el bundle, no sólo sobre un archivo.

---

# 22. Hardware Profiler

LARM detectará:

## Sistema

```text

Windows edition

Windows build

architecture

```

## CPU

```text

modelo

vendor

cores

threads

instruction sets

```

## Memoria

```text

RAM total

RAM disponible

memoria unificada

pagefile

```

## GPU

```text

vendor

modelo

driver

VRAM

shared memory

APIs disponibles

```

## NPU

```text

vendor

modelo

driver

execution providers

```

## APIs/aceleradores

```text

Vulkan

CUDA

ROCm

DirectML

Windows ML

OpenCL

SYCL

VitisAI

MIGraphX

OpenVINO

QNN

TensorRT

```

## Storage

Para cada volumen:

```text

identidad

tipo

capacidad

espacio libre

filesystem

rendimiento aproximado

```

---

# 23. HardwareFingerprint

Se generará un fingerprint para cada configuración.

Incluirá:

```text

Windows build

CPU

GPU

NPU

RAM

drivers

execution providers

runtime dependencies

```

El cambio de driver podrá producir un fingerprint nuevo.

Esto es necesario porque:

```text

modelo idéntico

+

runtime idéntico

+

driver diferente

```

puede producir resultados diferentes.

---

# 24. Compatibility Engine

La compatibilidad será una relación entre:

```text

ModelVariant

×

RuntimeVersion

×

Backend

×

HardwareFingerprint

×

LaunchProfile

```

Estados:

```text

COMPATIBLE

COMPATIBLE_WITH_LIMITATIONS

EXPERIMENTAL

CONVERSION_REQUIRED

RUNTIME_REQUIRED

BACKEND_REQUIRED

INCOMPATIBLE

UNKNOWN

```

---

# 25. Compatibilidad: hard gates

Antes de calcular puntuaciones deberán cumplirse condiciones obligatorias:

```text

formato soportado

arquitectura soportada

backend disponible

driver válido

memoria mínima

sistema operativo

dependencias

licencia aceptada

artefactos completos

```

Si falla una condición obligatoria:

```text

CompatibilityScore = N/A

```

No se maquillará una incompatibilidad mediante puntuaciones.

---

# 26. Technical Fit Score

Para candidatos compatibles podrá calcularse:

```text

TechnicalFitScore 0-100

```

basado en:

```text

memory headroom

backend suitability

runtime maturity

known benchmark data

storage performance

context requirements

stability evidence

```

Será diferente de:

```text

EvaluationScore

```

procedente de Model Drift.

No se confundirá rendimiento técnico con calidad del modelo.

---

# 27. Storage Manager

El almacenamiento será un subsistema completo.

No existirá una única carpeta global obligatoria.

El usuario podrá registrar múltiples:

```text

StorageLocation

```

Ejemplo:

```text

C:\\AI\\Models

D:\\ModelosIA

E:\\LLM

F:\\Diffusion

\\\\NAS\\AI\\Models

```

Cada ubicación tendrá:

```text

id

display_name

root_path

volume_identity

filesystem

storage_type

capacity

free_space

reserved_space

priority

health

enabled

```

---

# 28. Identidad de unidad

LARM no dependerá exclusivamente de:

```text

D:

```

Cuando sea posible almacenará:

```text

Windows Volume GUID

```

además de:

```text

drive letter

canonical path

```

De este modo podrá reconocer una unidad aunque cambie la letra.

Para ubicaciones de red se utilizará:

```text

UNC path

```

cuando sea posible.

---

# 29. Estados de StorageLocation

```text

ONLINE

LOW_SPACE

READ_ONLY

OFFLINE

MISSING

DEGRADED

ERROR

```

Si se desconecta un disco externo:

```text

Model = OFFLINE

```

no:

```text

Model = DELETED

```

Cuando vuelva a aparecer el volumen, LARM revalidará automáticamente sus modelos.

---

# 30. Selección del destino antes de descargar

Antes de cada descarga la interfaz mostrará:

```text

Modelo:

Qwen-X Q5_K_M

Descarga:

18.4 GB

Tamaño final estimado:

18.8 GB

Espacio temporal máximo:

19.1 GB

Espacio mínimo recomendado:

21 GB

```

Y:

```text

Destino:

D:\\ModelosIA

Libre: 312 GB

E:\\IA

Libre: 1.42 TB

[ Elegir carpeta... ]

```

El usuario podrá seleccionar manualmente.

---

# 31. StorageEstimate

Cada operación de descarga/transformación generará:

```text

StorageEstimate

```

con:

```text

download_bytes

final_artifact_bytes

installed_bytes

derived_cache_bytes

temporary_peak_bytes

required_free_bytes

confidence

```

`confidence`:

```text

EXACT

HIGH

ESTIMATED

UNKNOWN

```

---

# 32. Cálculo del tamaño previo

Orden de prioridad:

```text

1. Metadata oficial de la fuente

2. Manifest

3. API del repositorio

4. HTTP Content-Length

5. Git LFS metadata

6. estimación por formato/parámetros

7. UNKNOWN

```

Por ejemplo, Hugging Face puede proporcionar directamente el tamaño de los archivos mediante metadata del repositorio.

Si no existe información suficiente la interfaz deberá decir:

```text

Tamaño exacto no disponible.

Estimación: 18–23 GB.

```

No inventará precisión.

---

# 33. Espacio de transformaciones

Ejemplo:

```text

SafeTensors original: 30 GB

GGUF temporal:        18 GB

cuantización final:    9 GB

```

Durante el proceso podrían coexistir:

```text

30 + 18 + 9 GB

```

Por tanto el sistema deberá calcular:

```text

PeakDiskUsage

```

antes de iniciar una conversión.

---

# 34. Política tras conversión

El usuario podrá configurar:

```text

Conservar fuente original

Eliminar después de verificar derivado

Preguntar

```

Nunca se borrará automáticamente un original antes de verificar:

```text

hash

integridad

carga del derivado

smoke test

```

---

# 35. Ubicación de artefactos

La base de datos no almacenará únicamente:

```text

model_path

```

Existirá:

```text

ArtifactLocation

```

relacionando:

```text

ModelArtifact

StorageLocation

relative_path

```

Un modelo compuesto podrá tener varios artefactos.

---

# 36. Distribución entre discos

Por defecto todos los componentes críticos de un ModelBundle se almacenarán juntos.

Sin embargo, el modelo de datos permitirá múltiples ubicaciones.

Ejemplo:

```text

weights        → E:

vision encoder → E:

cache          → D:

```

Sólo se permitirá fragmentar un bundle entre discos cuando el Runtime Adapter confirme que es compatible.

---

# 37. Mover modelos

La GUI ofrecerá:

```text

Mover a...

```

Proceso cross-volume:

```text

lock model

 ↓

copy

 ↓

verify hashes

 ↓

update registry

 ↓

test access

 ↓

delete old copy

 ↓

unlock

```

Si falla antes del cambio de registry:

```text

ubicación original sigue siendo válida

```

---

# 38. Modelos activos

Un modelo con:

```text

active RuntimeLease

```

no podrá moverse ni eliminarse.

Opciones:

```text

Esperar

Detener instancia

Cancelar operación

```

---

# 39. Deduplicación

El almacenamiento utilizará hashes de contenido.

Dos modelos que compartan exactamente un archivo podrán referenciar el mismo blob cuando sea seguro.

En NTFS:

```text

hard links

```

podrán utilizarse dentro del mismo volumen.

Entre volúmenes distintos no se asumirá deduplicación física.

---

# 40. Content Addressed Store

Opcionalmente cada StorageLocation podrá funcionar como:

```text

Content Addressed Store

```

con:

```text

SHA256

```

como identidad principal.

Los runtimes podrán recibir una vista compatible:

```text

runtime view

```

mediante enlaces o rutas generadas.

---

# 41. Storage Ownership

Un archivo podrá estar:

```text

LARM_MANAGED

HOST_MANAGED

EXTERNAL

```

Ejemplo:

Un modelo instalado por Lemonade podrá continuar almacenado en la cache propia de Lemonade.

LARM registra:

```text

host-managed

path

size

hash si accesible

```

sin moverlo automáticamente.

---

# 42. Importación de modelos existentes

Función:

```text

Escanear carpeta

```

LARM detectará:

```text

GGUF

ONNX

SafeTensors

PyTorch

MLC

model manifests

Ollama blobs

otros formatos conocidos

```

Intentará identificar el modelo y registrarlo.

Estado:

```text

ADOPTED

```

No será necesario volver a descargarlo.

---

# 43. Limpieza de almacenamiento

Panel:

```text

Storage Analyzer

```

Mostrará:

```text

modelos

copias duplicadas

versiones antiguas

downloads parciales

conversiones temporales

caches

artefactos huérfanos

runtimes antiguos

```

Y estimará:

```text

Espacio recuperable: XXX GB

```

Nunca eliminará elementos automáticamente sin política explícita.

---

# 44. Download Manager

Funciones:

```text

cola

pausa

reanudar

cancelar

prioridades

reintentos

bandwidth limit

parallel downloads

checksum

```

Los downloads usarán:

```text

*.partial

```

hasta completarse.

Tras finalizar:

```text

verify

 ↓

atomic rename

```

cuando el filesystem lo permita.

---

# 45. Preflight de descarga

Antes de comenzar:

```text

source accessible

authentication valid

license accepted

destination online

destination writable

required space available

runtime compatibility known

```

Si falta espacio:

```text

No iniciar descarga.

```

La UI propondrá otras StorageLocations.

---

# 46. Model Transformation Pipeline

LARM deberá poder producir artefactos derivados.

Ejemplos:

```text

SafeTensors

    ↓

GGUF

    ↓

Q5_K_M

```

o:

```text

Hugging Face

    ↓

Olive

    ↓

ONNX

    ↓

INT4

```

Cada transformación producirá:

```text

TransformationRecord

```

con:

```text

source hash

tool

tool version

parameters

output hash

date

logs

```

---

# 47. Provenance derivada

Un GGUF creado localmente conservará la cadena:

```text

Modelo original

  ↓

revision

  ↓

convert_hf_to_gguf version X

  ↓

llama-quantize version Y

  ↓

Q5_K_M

```

Así se podrá reproducir posteriormente.

---

# 48. Runtime Version Manager

Todas las versiones se instalarán side-by-side cuando sea técnicamente viable.

Ejemplo:

```text

llama.cpp

 ├── b8500

 ├── b8544

 └── b8601

```

Nunca se sobrescribirá la última versión estable conocida.

---

# 49. Canales de runtime

```text

STABLE

PRERELEASE

NIGHTLY

COMMIT

CUSTOM

```

El usuario podrá elegir política por runtime.

---

# 50. Actualización de runtime

```text

UPDATE_DISCOVERED

 ↓

download

 ↓

install side-by-side

 ↓

validate

 ↓

smoke test

 ↓

technical benchmark

 ↓

compare

 ↓

promote

```

Si existe regresión:

```text

NO_PROMOTE

```

La versión anterior permanece activa.

---

# 51. Runtime Pinning

Será posible fijar:

```text

Modelo X

 ↓

llama.cpp b8544

```

independientemente de la versión predeterminada.

Esto garantizará reproducibilidad.

---

# 52. Runtime Installation Strategies

Los adaptadores podrán declarar:

```text

PREBUILT_BINARY

GITHUB_RELEASE

SOURCE_BUILD

WINGET

PIP

UV

CONDA

MAMBA

NUGET

WSL

CONTAINER

CUSTOM

```

---

# 53. Python Environment Manager

Los runtimes Python estarán aislados.

Cada entorno tendrá:

```text

Python version

packages

versions

CUDA/ROCm dependencies

path

hash

```

Se podrán utilizar:

```text

uv

venv

micromamba/conda

```

Un runtime no contaminará el Python global.

---

# 54. Runtime Instance

Una ejecución concreta se representará como:

```text

RuntimeInstance

```

Contendrá:

```text

instance_id

runtime

runtime_version

backend

model

launch_profile

PID

start_time

endpoint

protocol

health

memory

```

---

# 55. Runtime Process Supervisor

Estados:

```text

CREATED

STARTING

LOADING

READY

BUSY

DEGRADED

FAILED

STOPPING

STOPPED

CRASHED

```

El Supervisor podrá:

```text

restart

collect logs

detect crash

detect port conflict

detect OOM

```

---

# 56. Launch Profiles

Un modelo podrá tener múltiples configuraciones.

Ejemplo:

```text

ECO

BALANCED

PERFORMANCE

LONG_CONTEXT

CUSTOM

```

Un perfil contiene:

```text

context

threads

batch

GPU layers

backend

memory limit

runtime arguments

environment variables

```

---

# 57. Memory Estimator

Antes de cargar un modelo se estimará:

```text

weights

runtime overhead

KV cache

workspace

vision/audio components

temporary buffers

safety margin

```

En máquinas con memoria unificada se evitará contar RAM y VRAM como memorias independientes cuando no lo sean.

---

# 58. Protocol Layer

Cada RuntimeInstance declarará una o varias interfaces.

Ejemplos:

```text

OPENAI_CHAT

OPENAI_RESPONSES

ANTHROPIC_MESSAGES

OLLAMA

CUSTOM_REST

GRPC

STDIO

MCP

CLI

```

---

# 59. Protocol Sidecar

Cuando un runtime no ofrezca una API conveniente, LARM podrá lanzar:

```text

Protocol Sidecar

```

Ejemplo:

```text

runtime CLI

    ↑

sidecar HTTP

    ↑

AI Broker

```

El Sidecar no realizará routing inteligente.

Sólo normalizará el acceso a **ese modelo concreto**.

---

# 60. Acceso API a modelos

Se mantiene el requisito original:

> Cada modelo gestionado deberá poder exponerse mediante API cuando técnicamente sea posible.

Existirán dos opciones:

```text

Native Runtime Endpoint

```

o:

```text

LARM Protocol Sidecar

```

LARM devuelve el endpoint.

---

# 61. Acceso MCP a modelos

Existirán dos MCP diferentes.

## Administration MCP

Herramientas:

```text

models.search

models.install

models.move

models.delete

runtimes.install

runtimes.update

models.start

models.stop

benchmark.run

hardware.info

```

## Model Access MCP

Opcional.

Permitirá exponer un modelo seleccionado mediante MCP cuando tenga sentido.

No será el mecanismo normal de inferencia interna del ecosistema.

---

# 62. AI Broker sigue siendo el gateway

Para las aplicaciones de la familia:

```text

App

 ↓

AI Broker

 ↓

modelo

```

No:

```text

App

 ↓

LARM

```

salvo herramientas de administración explícitas.

---

# 63. AI Broker Bridge

AI Broker deberá incorporar:

```text

LocalRuntimeResolver

```

Operaciones:

```text

resolve_model()

request_lease()

release_lease()

get_endpoint()

get_capabilities()

```

---

# 64. Runtime Lease

Ejemplo:

```json

{

  "model": "laya",

  "capability": "decision.evaluate",

  "profile": "balanced"

}

```

Respuesta:

```json

{

  "lease_id": "...",

  "instance_id": "...",

  "endpoint": "http://127.0.0.1:18432",

  "protocol": "openai",

  "runtime": "llama.cpp",

  "runtime_version": "...",

  "model_revision": "..."

}

```

Mientras exista la lease LARM no deberá descargar la instancia.

---

# 65. Multimodalidad y AI Broker

AI Broker deberá evolucionar desde un simple catálogo de LLM hacia contratos de capacidad.

Interfaces previstas:

```text

chat

decision

embedding

rerank

vision

ocr

image-generation

video-generation

transcription

speech

generic

```

Las aplicaciones actuales que sólo utilicen chat no necesitarán cambios.

Las nuevas capacidades se incorporarán progresivamente mediante el mismo Broker.

---

# 66. Laya y System-1

Laya será un modelo administrado por LARM.

No una excepción externa.

Ejemplo:

```text

CanonicalModel:

Laya

capability:

decision.evaluate

role:

SYSTEM1

```

Otros modelos System-1 podrán descubrirse desde:

```text

Hugging Face

Ollama

ModelScope

otros

```

---

# 67. Estados System-1

```text

SYSTEM1_CANDIDATE

SYSTEM1_TECHNICALLY_VALID

SYSTEM1_EVALUATED

SYSTEM1_CERTIFIED

SYSTEM1_REJECTED

```

LARM determina:

```text

compatibilidad técnica

```

Local-AI-Lab genera evidencia.

Model Drift determina:

```text

certificación funcional

```

AI Broker decide:

```text

routing

```

---

# 68. Lemonade

Lemonade tendrá adaptador nativo.

La instalación actualmente existente podrá adoptarse:

```text

ADOPTED

```

sin reinstalar.

Lemonade expone actualmente operaciones para instalar, cargar y descargar modelos, listar descargas, gestionar backends, consultar salud, estadísticas y hardware; también puede buscar repositorios en Hugging Face o ModelScope.

Esto permite una integración mucho más robusta que tratarlo simplemente como un endpoint OpenAI.

---

# 69. Lemonade Handshake

Al detectar Lemonade:

```text

1. Detect installation

2. Detect version

3. Discover configuration

4. GET /v1/health

5. GET /v1/docs

6. GET /v1/models

7. inspect recipes

8. inspect backends

9. inspect loaded models

10. smoke-test each eligible combination

```

---

# 70. Certificación Lemonade → AI Broker

Para cada modelo:

```text

DISCOVERED

 ↓

METADATA_VALID

 ↓

BACKEND_AVAILABLE

 ↓

LOAD_PASS

 ↓

HEALTH_PASS

 ↓

PROTOCOL_PASS

 ↓

INFERENCE_PASS

 ↓

STREAMING_PASS

 ↓

TOOLS_PASS (si procede)

 ↓

BROKER_ELIGIBLE

```

Esto permitirá localizar el motivo exacto si vuelve a ocurrir el problema anteriormente observado.

---

# 71. Errores Lemonade distinguibles

Ejemplos:

```text

modelo aparece pero no está instalado

backend ausente

recipe incompatible

load failure

model ID mismatch

endpoint mismatch

OpenAI compatibility parcial

streaming failure

tool calling failure

OOM

backend crash

```

---

# 72. Technical Benchmark Engine

Se ejecutará en el PC IA.

Medirá según capacidad:

## Texto

```text

load time

TTFT

prompt tok/s

generation tok/s

throughput

RAM

GPU/NPU memory

CPU/GPU utilization

```

## Imagen

```text

load time

seconds/image

images/min

memory peak

```

## ASR

```text

real-time factor

latency

memory

```

## TTS

```text

real-time factor

latency

```

Otros adaptadores podrán añadir métricas.

---

# 73. Benchmark reproducible

Todo benchmark deberá estar vinculado a:

```text

ModelArtifact hash

RuntimeVersion

Backend

HardwareFingerprint

LaunchProfile

BenchmarkSuiteVersion

```

---

# 74. Benchmark versus evaluación

No se mezclarán.

```text

Technical Benchmark

       ↓

LARM

```

mide rendimiento.

```text

Evaluation

       ↓

Local-AI-Lab / Model Drift

```

mide comportamiento/calidad.

---

# 75. Benchmark distribuido

Flujo:

```text

PC IA

modelo instalado

 ↓

LARM benchmark

 ↓

TECHNICALLY_READY

 ↓

Agora

════════════════════

PC principal

Local-AI-Lab

 ↓

AI Broker

════════════════════

PC IA

AI Broker

 ↓

modelo

════════════════════

PC principal

resultados

 ↓

Model Drift

 ↓

certificación

```

---

# 76. ExecutionFingerprint

Cada prueba conservará:

```text

model

revision

artifact SHA256

runtime

runtime version

backend

driver

hardware

launch profile

context

endpoint protocol

```

Model Drift podrá así distinguir:

```text

model regression

runtime regression

driver regression

configuration regression

```

---

# 77. Agora

Agora seguirá siendo el sistema de coordinación distribuida.

No se creará otro scheduler.

Eventos iniciales:

```text

MODEL_DISCOVERED

MODEL_UPDATE_AVAILABLE

MODEL_DOWNLOADED

MODEL_INSTALLED

MODEL_MOVED

RUNTIME_UPDATE_AVAILABLE

RUNTIME_INSTALLED

TECH_BENCHMARK_COMPLETED

EVALUATION_REQUESTED

EVALUATION_COMPLETED

MODEL_CERTIFIED

MODEL_REJECTED

RUNTIME_REGRESSION_DETECTED

```

---

# 78. Model Watcher

El usuario podrá crear suscripciones.

Ejemplos:

```text

Nuevos System-1

Nuevos VLM < 20 GB

Nuevos modelos compatibles con este PC

Nuevos modelos de código

Actualizaciones de Laya

Nuevos modelos ONNX optimizados para AMD

```

---

# 79. Source Sync

Cada Source Adapter implementará cuando sea posible:

```text

incremental sync

cursor

ETag

If-Modified-Since

webhook

```

En ausencia de ello:

```text

scheduled polling

```

con backoff.

---

# 80. Notificaciones

Tipos:

```text

NEW_MODEL

NEW_COMPATIBLE_MODEL

MODEL_UPDATE

RUNTIME_UPDATE

LOW_DISK_SPACE

STORAGE_OFFLINE

MODEL_REGRESSION

RUNTIME_REGRESSION

DOWNLOAD_COMPLETED

```

No se descargará automáticamente un modelo nuevo salvo política explícita.

---

# 81. Modelo de datos lógico

Tablas principales:

```text

canonical_model

model_distribution

model_revision

model_variant

model_bundle

model_artifact

artifact_provenance

artifact_location

model_source

model_source_adapter

model_source_account

runtime

runtime_adapter

runtime_version

runtime_installation

runtime_backend

runtime_instance

external_host

hardware_profile

storage_location

compatibility_record

launch_profile

runtime_lease

inference_endpoint

download_job

transformation_job

technical_benchmark

evaluation_reference

certification

source_watch

notification

event

credential_reference

adapter_installation

```

---

# 82. artifact_location

Campos mínimos:

```text

id

artifact_id

storage_location_id

relative_path

file_size

sha256

state

verified_at

```

---

# 83. storage_location

Campos:

```text

id

name

root_path

canonical_path

volume_guid

drive_letter

unc_path

filesystem

storage_type

capacity_bytes

free_bytes

reserved_bytes

priority

auto_placement

state

last_seen_at

```

---

# 84. model_artifact

Campos:

```text

id

variant_id

filename

format

quantization

expected_size

actual_size

sha256

source_url

required

```

---

# 85. runtime_version

No se asumirá SemVer.

Campos:

```text

version_string

commit

build

channel

released_at

```

porque proyectos como llama.cpp pueden utilizar identificadores propios.

---

# 86. Base de datos

SQLite será suficiente para metadata local.

Configuración:

```text

WAL mode

foreign keys

migrations

transactional updates

```

No se almacenarán modelos como blobs dentro de SQLite.

---

# 87. API administrativa

Namespace:

```text

/api/v1

```

Áreas:

```text

/hardware

/storage

/storage/locations

/sources

/sources/search

/models

/models/{id}

/models/{id}/variants

/models/{id}/move

/runtimes

/runtimes/{id}/versions

/runtimes/{id}/instances

/compatibility

/downloads

/transformations

/leases

/benchmarks

/notifications

/events

/adapters

```

---

# 88. Eventos API

Para progreso:

```text

SSE

```

o WebSocket donde sea necesario.

Ejemplos:

```text

download progress

model load

runtime install

benchmark progress

conversion progress

```

---

# 89. API versioning

Cambios incompatibles requerirán:

```text

/api/v2

```

Nunca se romperá silenciosamente AI Broker por una actualización de LARM.

---

# 90. Seguridad de API

Por defecto:

```text

bind 127.0.0.1

```

No:

```text

0.0.0.0

```

El acceso remoto tendrá que activarse explícitamente.

---

# 91. Autenticación local

AI Broker utilizará una credencial local dedicada.

Podrá emplearse:

```text

API token

```

o mecanismo local equivalente.

Las credenciales se guardarán con protección del sistema operativo.

---

# 92. MCP Permissions

Scopes:

```text

read

search

download

install

start

stop

benchmark

move

delete

update

admin

```

Una herramienta MCP no recibirá automáticamente `admin`.

---

# 93. Seguridad de modelos

Especial atención a:

```text

trust_remote_code

custom Python

custom operators

ComfyUI custom nodes

executables

DLLs

scripts

```

Un modelo que requiera ejecutar código externo mostrará:

```text

REMOTE_CODE_REQUIRED

```

y no se ejecutará silenciosamente.

---

# 94. Sandboxing

Siempre que sea viable:

```text

modelo/routine peligroso

 ↓

proceso aislado

```

No dentro del proceso Core.

Los runtimes Python utilizarán entornos separados.

---

# 95. Integridad

Siempre que la fuente proporcione hash:

```text

verify

```

Si no existe:

LARM calculará al menos:

```text

SHA256

```

después de la descarga y conservará ese valor.

---

# 96. Licencias

Antes de descargar se mostrará:

```text

license

gated

commercial restrictions

unknown license

```

LARM no aceptará automáticamente licencias en nombre del usuario cuando el proveedor requiera aceptación explícita.

---

# 97. Trust Score

Separado de CompatibilityScore.

Posibles señales:

```text

official publisher

verified source

signed release

hash available

community uploader

unknown provenance

remote code

```

No afirmará que un modelo es “seguro”; mostrará evidencia y riesgo.

---

# 98. Observabilidad

Logs estructurados:

```text

timestamp

component

runtime

instance

model

severity

event

correlation_id

```

---

# 99. Logs por modelo/instancia

Desde GUI:

```text

Runtime logs

Adapter logs

Download logs

Conversion logs

Broker handshake logs

```

Esto será particularmente útil para Lemonade.

---

# 100. Metrics

Métricas locales:

```text

CPU

RAM

GPU

NPU

disk

requests

latency

errors

runtime crashes

```

Opcionalmente endpoint compatible con Prometheus.

---

# 101. Servicio de fondo

LARM se dividirá en:

```text

LARM Core/Daemon

LARM Desktop GUI

```

Cerrar la GUI no detendrá runtimes gestionados.

---

# 102. Modos de daemon Windows

Dos modos:

## User Background Agent

Predeterminado.

Ventajas:

```text

no necesita servicio privilegiado

acceso natural a carpetas del usuario

mejor acceso a unidades mapeadas

```

## Windows Service

Opcional para funcionamiento unattended.

La arquitectura del Core funcionará en ambos.

---

# 103. Recuperación después de reinicio

Al arrancar:

```text

scan database

 ↓

detect orphan processes

 ↓

detect runtime installations

 ↓

validate StorageLocations

 ↓

reconcile active instances

 ↓

reconcile downloads

```

No deberá asumir que el estado persistido coincide con la realidad.

---

# 104. Crash recovery

Una descarga interrumpida:

```text

PAUSED/INTERRUPTED

```

y podrá reanudarse.

Una transformación no reanudable:

```text

FAILED_RECOVERABLE

```

podrá reiniciarse sin dañar el artefacto original.

---

# 105. Puertos

Los RuntimeInstances recibirán puertos dinámicos de un rango configurable.

LARM verificará:

```text

port free

```

antes de lanzar.

El endpoint nunca se reconstruirá suponiendo un puerto fijo.

---

# 106. Interfaz gráfica principal

Menú propuesto:

```text

Inicio

Descubrir

Mis modelos

Runtimes

Instancias

Hardware

Almacenamiento

Descargas

Benchmarks

Actualizaciones

Fuentes

Adaptadores

Notificaciones

Configuración

```

---

# 107. Inicio

Dashboard:

```text

Modelos instalados

Espacio ocupado

Espacio libre por disco

Runtimes activos

Modelos cargados

RAM disponible

GPU/NPU

Downloads

Actualizaciones

Alertas

```

---

# 108. Descubrir

Buscador federado.

Filtros:

```text

capability

architecture

parameters

format

quantization

runtime

source

license

size

technical compatibility

trust

certification

```

Opciones:

```text

Sólo ejecutables en este PC

Sólo recomendados

Mostrar incompatibles

```

---

# 109. Resultado de búsqueda

Ejemplo:

```text

Modelo X

Fuentes: 4

Variantes: 13

Mejor variante para este PC:

ONNX INT4

12.8 GB

Runtime: ONNX Runtime GenAI

Backend: NPU/GPU

Technical Fit: 97

[Detalles]

[Descargar]

```

---

# 110. Comparación de variantes

```text

             ONNX INT4   GGUF Q5    GGUF Q8

Disco         12 GB       18 GB      29 GB

Runtime       ORT         llama      llama

Backend       NPU/GPU     Vulkan     Vulkan

RAM estim.    ...

Benchmark     ...

Evaluación    ...

```

---

# 111. Pantalla de descarga

Antes de confirmar:

```text

Descarga            18.4 GB

Instalado           18.8 GB

Pico temporal        19.1 GB

Espacio recomendado  21 GB

Destino:

E:\\ModelosIA

Libre:

1.42 TB

```

---

# 112. Pantalla Almacenamiento

Ejemplo:

```text

C:\\AI

95 GB libres

SYSTEM

D:\\Modelos

210 GB libres

LOW SPACE

E:\\IA

1.42 TB libres

PREFERRED

\\\\NAS\\IA

4.2 TB libres

NETWORK

```

Acciones:

```text

Establecer predeterminado

Cambiar reserva

Analizar

Mover modelos

Desactivar

```

---

# 113. Auto-placement

Políticas:

```text

ASK_ALWAYS

PREFERRED_LOCATION

MOST_FREE_SPACE

FASTEST_STORAGE

BY_MODEL_TYPE

CUSTOM_RULES

```

Ejemplo:

```text

LLM        → E:

Diffusion  → F:

Audio      → D:

```

---

# 114. Reserva de espacio

Cada StorageLocation podrá mantener:

```text

reserved_bytes

```

Ejemplo:

```text

No permitir que E: baje de 100 GB libres.

```

Auto-placement respetará esa reserva.

---

# 115. Storage Performance Class

Clasificación:

```text

NVME

SSD

HDD

NETWORK

REMOVABLE

UNKNOWN

```

Podrá influir en recomendaciones.

Un HDD enorme puede ser adecuado para almacenamiento frío pero no para modelos utilizados continuamente.

---

# 116. Ciclo de vida del modelo

```text

DISCOVERED

 ↓

SELECTED

 ↓

DOWNLOAD_PLANNED

 ↓

DOWNLOADING

 ↓

DOWNLOADED

 ↓

VERIFIED

 ↓

PREPARED

 ↓

TECHNICALLY_VALIDATED

 ↓

AVAILABLE

 ↓

RUNNING

```

Estados adicionales:

```text

OFFLINE

CORRUPT

UPDATE_AVAILABLE

FAILED

REJECTED

```

---

# 117. Ciclo de vida runtime

```text

DISCOVERED

 ↓

INSTALL_PLANNED

 ↓

INSTALLING

 ↓

INSTALLED

 ↓

VALIDATED

 ↓

ACTIVE

```

o:

```text

DEGRADED

BROKEN

ROLLED_BACK

```

---

# 118. Instalaciones externas

Estados:

```text

MANAGED

ADOPTED

EXTERNAL

```

Lemonade empezará previsiblemente como:

```text

ADOPTED

```

---

# 119. Actualización de modelos

Una revisión nueva nunca sobrescribirá inmediatamente la versión anterior.

```text

revision A

revision B

```

podrán coexistir hasta validación.

---

# 120. Model promotion

Una nueva variante podrá ser:

```text

CANDIDATE

TECHNICALLY_APPROVED

EVALUATION_APPROVED

DEFAULT

```

AI Broker sólo utilizará automáticamente las categorías permitidas por su política.

---

# 121. Integración Local-AI-Lab

Local-AI-Lab podrá solicitar mediante AI Broker:

```text

modelo

variante

perfil

```

y recibir junto con el resultado:

```text

ExecutionFingerprint

```

No tendrá que saber en qué carpeta física está el modelo.

---

# 122. Integración Model Drift

Model Drift deberá conservar:

```text

model revision

runtime version

backend

fingerprint

```

para poder detectar regresiones correctamente.

---

# 123. Integración Agora

Agora no gestionará directamente archivos o runtimes.

Coordinará trabajos:

```text

benchmark

evaluation

certification

revalidation

```

---

# 124. Integración de otras aplicaciones

ChatyGPT y las aplicaciones que ya utilicen AI Broker para inferencia no necesitarán cambios para seguir haciendo lo mismo.

Athena sólo necesitará acceso MCP a LARM cuando quiera:

```text

buscar

instalar

actualizar

mover

benchmark

```

un modelo.

---

# 125. Recomendación de modelo

Existirán dos puntuaciones diferentes:

```text

TechnicalFitScore

EvaluationScore

```

Si ambos existen podrá calcularse:

```text

RecommendationScore

```

con pesos configurables.

Si no existe evaluación:

```text

Recomendación basada únicamente en compatibilidad técnica.

```

---

# 126. Nunca fingir evaluación

Popularidad, downloads o estrellas no equivalen a calidad.

Se podrán mostrar como metadata, pero Model Drift será la fuente de evaluación interna.

---

# 127. Stack tecnológico

Baseline propuesta:

## Core

```text

Rust

Tokio

```

## API

```text

Axum

JSON

SSE/WebSocket

```

## Desktop

```text

Tauri 2

React

TypeScript

```

## Database

```text

SQLite

SQLx

```

## HTTP/download

```text

reqwest

```

## Logging

```text

tracing

```

La versión concreta de cada dependencia se fijará al comenzar la implementación y se mantendrá mediante lockfiles.

---

# 128. Razón de separar GUI y Core

Permite:

```text

cerrar GUI sin detener modelos

actualizar GUI independientemente

automatizar vía API

usar MCP

integrarse con AI Broker

```

---

# 129. Sistema de plugins

Tipos:

```text

RuntimeAdapterPlugin

ModelSourceAdapterPlugin

ProtocolAdapterPlugin

TransformationPlugin

BenchmarkPlugin

```

Todos declararán:

```text

plugin API version

capabilities

permissions

publisher

version

```

---

# 130. Plugin Registry

LARM podrá consultar un registro firmado de adaptadores conocidos.

También permitirá:

```text

Instalar archivo local

Añadir repositorio

Añadir adaptador personalizado

```

No dependerá obligatoriamente de dicho registro.

---

# 131. Firma de plugins

Los plugins oficiales podrán estar firmados.

Estados:

```text

OFFICIAL

SIGNED_THIRD_PARTY

UNSIGNED

CUSTOM

```

La UI deberá mostrarlo claramente.

---

# 132. Actualización de LARM

LARM también se actualizará side-by-side cuando sea posible.

La base de datos tendrá migrations reversibles cuando técnicamente sea viable y siempre backup previo.

---

# 133. Backup

LARM deberá poder exportar:

```text

configuración

registry metadata

storage map

runtimes

model registry

benchmarks

certifications

```

No incluirá obligatoriamente cientos de GB de modelos.

Los modelos podrán reindexarse a partir de sus ubicaciones.

---

# 134. Portable Recovery

Si la base de datos se pierde, una operación:

```text

Rebuild Registry

```

deberá poder escanear los StorageLocations y recuperar todo lo posible.

---

# 135. Modelos desconectados

Si:

```text

E:

```

no está disponible:

```text

modelo.estado = OFFLINE_STORAGE

```

AI Broker no deberá seleccionarlo.

Cuando vuelva:

```text

verify

 ↓

AVAILABLE

```

---

# 136. Baja disponibilidad de disco

Umbrales:

```text

WARNING

CRITICAL

```

Ejemplo configurable:

```text

WARNING < 15 %

CRITICAL < 5 %

```

o mediante GB absolutos.

---

# 137. Descarga bloqueada por espacio

Si el destino no tiene:

```text

required_free_bytes + reserve

```

la descarga no comienza.

La UI propondrá:

```text

otro StorageLocation

```

---

# 138. Performance de almacenamiento

Los benchmarks técnicos podrán registrar también:

```text

model load source volume

storage class

read throughput

```

Así podrá observarse si mover un modelo de HDD a NVMe mejora su carga.

---

# 139. Source selection

Si el mismo artefacto existe en múltiples fuentes podrá elegirse según:

```text

trust

availability

download speed

integrity

license

revision

```

No simplemente por orden fijo.

---

# 140. Offline Mode

LARM podrá funcionar sin Internet con:

```text

modelos existentes

runtimes existentes

registry cache

benchmarks

```

Las búsquedas remotas aparecerán no disponibles.

---

# 141. Import/export de adaptadores

Será posible exportar una configuración de runtime/source para utilizarla en otra instalación LARM.

No incluirá secretos.

---

# 142. Testing Strategy

Cada módulo tendrá:

```text

unit tests

contract tests

integration tests

failure tests

real hardware tests

```

---

# 143. Contract tests de Runtime Adapter

Todo adaptador oficial deberá pasar:

```text

probe

install

version

launch

health

stop

crash recovery

invalid model

invalid backend

```

---

# 144. Contract tests de Model Source Adapter

```text

search

metadata

size estimate

download

resume

checksum

authentication failure

rate limit

network interruption

```

---

# 145. Storage tests

Obligatorios:

```text

download insufficient space

drive disconnected

drive letter changed

move same volume

move different volume

copy interrupted

hash mismatch

destination disappears

duplicate artifact

network share unavailable

```

---

# 146. Lemonade tests

Especialmente:

```text

health

model discovery

backend discovery

pull

load

unload

chat

stream

tools

multimodal cuando proceda

invalid recipe

missing backend

```

---

# 147. AI Broker integration tests

```text

resolve

lease

load model

infer

release

model offline

runtime crash

fallback

```

---

# 148. Benchmark validation

Un benchmark deberá poder repetirse bajo el mismo fingerprint y detectar variaciones anómalas.

Nunca se compararán directamente benchmarks de configuraciones diferentes sin indicarlo.

---

# 149. Performance del propio LARM

El daemon deberá consumir recursos mínimos cuando esté idle.

No deberá mantener modelos en memoria sin necesidad salvo:

```text

pinned

active lease

configured warm model

```

---

# 150. Política de auto-unload

Configurable:

```text

never

idle timeout

memory pressure

LRU

```

AI Broker podrá solicitar:

```text

pin

```

para modelos críticos.

---

# 151. Model Scheduler

LARM podrá decidir qué instancia descargar cuando haya presión de memoria.

No elegirá **qué modelo responde al usuario**.

Eso sigue siendo responsabilidad de AI Broker.

---

# 152. Prioridad de instancias

```text

PINNED

HIGH

NORMAL

BACKGROUND

```

Un benchmark no deberá expulsar un modelo crítico salvo política explícita.

---

# 153. Concurrencia

Los Runtime Adapters declararán:

```text

max_instances

multi_model_support

parallel_requests

GPU sharing

```

El Scheduler respetará esas limitaciones.

---

# 154. Errores normalizados

LARM definirá códigos propios:

```text

STORAGE_INSUFFICIENT

MODEL_NOT_FOUND

MODEL_OFFLINE

MODEL_CORRUPT

RUNTIME_NOT_INSTALLED

RUNTIME_INCOMPATIBLE

RUNTIME_CRASH

BACKEND_UNAVAILABLE

AUTH_REQUIRED

LICENSE_REQUIRED

DOWNLOAD_FAILED

LOAD_OOM

PROTOCOL_ERROR

```

Los errores originales permanecerán adjuntos.

---

# 155. Diagnostics Bundle

Desde GUI:

```text

Crear paquete diagnóstico

```

Incluirá:

```text

versions

hardware

drivers

adapter states

logs seleccionados

errors

```

No incluirá tokens.

Esto será especialmente útil para depurar AI Broker ↔ Lemonade.

---

# 156. Privacidad

Por defecto:

```text

sin telemetría externa

```

Cualquier telemetría futura deberá ser opt-in.

---

# 157. Desarrollo modular

No habrá una MVP reducida.

Se desarrollará el producto completo mediante módulos independientes y verificables.

Ningún módulo se considerará terminado sin pruebas reales.

---

# 158. Orden de implementación recomendado

## Módulo 0 — Bootstrap

```text

repo

CI

logging

config

SQLite

migrations

daemon

GUI shell

API skeleton

```

## Módulo 1 — Hardware Profiler

```text

CPU

RAM

GPU

NPU

drivers

backends

storage

fingerprint

```

## Módulo 2 — Storage Manager

```text

multi-volume

Volume GUID

free space

reservation

offline disks

artifact paths

move

scan

```

## Módulo 3 — Adapter Framework

```text

out-of-process protocol

manifest

permissions

lifecycle

```

## Módulo 4 — Runtime Registry

```text

runtime

versions

installations

backends

```

## Módulo 5 — Model Registry

```text

canonical models

variants

artifacts

bundles

provenance

```

## Módulo 6 — Model Source Framework

```text

search

metadata

download plans

auth

size estimates

```

## Módulo 7 — Hugging Face Adapter

Primer Source Adapter de referencia.

## Módulo 8 — ModelScope Adapter

Segundo adapter para demostrar que el Core no depende de HF.

## Módulo 9 — Download Manager

```text

queue

resume

checksum

storage preflight

```

## Módulo 10 — llama.cpp Adapter

Incluyendo versiones múltiples.

## Módulo 11 — ONNX Runtime / Windows ML

Desde el principio del producto, no como ampliación opcional.

## Módulo 12 — Compatibility Engine

## Módulo 13 — Model Transformation Pipeline

## Módulo 14 — Runtime Supervisor

## Módulo 15 — Protocol Layer

## Módulo 16 — AI Broker Bridge

## Módulo 17 — stable-diffusion.cpp

## Módulo 18 — whisper.cpp

## Módulo 19 — Lemonade Adapter

Incluyendo adopción de la instalación existente y diagnóstico exhaustivo.

## Módulo 20 — Ollama

## Módulo 21 — PyTorch/Transformers/Diffusers

## Módulo 22 — MLC LLM

## Módulo 23 — sherpa-onnx

## Módulo 24 — OpenVINO

## Módulo 25 — Foundry Local

## Módulo 26 — Otros runtimes Windows

## Módulo 27 — Benchmark Engine

## Módulo 28 — Agora Bridge

## Módulo 29 — Local-AI-Lab / Model Drift

## Módulo 30 — MCP Administration

## Módulo 31 — Model Access MCP

## Módulo 32 — Source Watcher

## Módulo 33 — Actualizaciones y rollback

## Módulo 34 — Hardening

## Módulo 35 — UX final y pruebas end-to-end

---

# 159. Regla de avance entre módulos

Un módulo sólo permite comenzar el siguiente cuando:

```text

spec implemented

unit tests PASS

integration tests PASS

real test PASS

acceptance criteria PASS

documentation updated

```

No se aplazarán errores estructurales con:

```text

TODO

```

sin una decisión explícita.

---

# 160. Criterios de aceptación global

La aplicación completa deberá demostrar, al menos:

### Runtimes

Instalar y mantener múltiples versiones del mismo runtime.

### Sources

Buscar el mismo tipo de modelo en múltiples repositorios.

### Storage

Mostrar el tamaño antes de descargar.

### Multi-disk

Descargar dos modelos en discos diferentes.

### Move

Mover un modelo sin perder su registro.

### Drive loss

Detectar correctamente una unidad desconectada.

### Compatibility

Distinguir compatible/no compatible.

### AI Broker

Resolver un modelo, arrancarlo e inferir.

### Laya/System-1

Administrarlo como modelo normal del sistema.

### Lemonade

Detectarlo, adoptarlo, diagnosticarlo y exponer sólo modelos verificados.

### Benchmark

Comparar dos runtimes/versiones.

### Updates

Actualizar un runtime y revertirlo.

### Discovery

Detectar un nuevo modelo compatible.

### MCP

Administrar LARM mediante un agente.

### Recovery

Recuperar correctamente tras reinicio/crash.

---

# 161. Reglas arquitectónicas inmutables

Durante el desarrollo deberán respetarse estas reglas:

> **LARM administra la capacidad local de inferencia.**

> **AI Broker administra la selección y el acceso a dicha capacidad.**

> **Local-AI-Lab ejecuta experimentación y genera evidencia.**

> **Model Drift evalúa y certifica comportamiento.**

> **Agora coordina trabajos distribuidos entre máquinas.**

> **Un runtime nuevo no debe requerir modificar el Core.**

> **Una fuente nueva de modelos no debe requerir modificar el Core.**

> **La ruta física de un modelo nunca debe quedar embebida como conocimiento de otra aplicación.**

> **AI Broker identifica modelos lógicamente; LARM resuelve dónde están y cómo ejecutarlos.**

> **Ninguna actualización debe destruir automáticamente la última combinación conocida como funcional.**

> **Ningún modelo aparecerá como utilizable únicamente porque haya podido descargarse.**

---

# 162. Estado final deseado

El flujo ideal será:

```text

Usuario busca:

"modelo multimodal pequeño"

        │

        ▼

LARM consulta múltiples fuentes

        │

        ▼

normaliza duplicados

        │

        ▼

detecta variantes

        │

        ▼

cruza runtimes disponibles

        │

        ▼

cruza hardware

        │

        ▼

estima memoria

        │

        ▼

estima almacenamiento

        │

        ▼

muestra candidatos compatibles

```

El usuario elige:

```text

Modelo X

ONNX INT4

12.6 GB

```

LARM muestra:

```text

Descarga:         12.6 GB

Instalación:      12.9 GB

Pico temporal:    13.8 GB

C:\\   64 GB libres

D:\\  107 GB libres

E:\\ 1.4 TB libres

Destino recomendado: E:

```

Después:

```text

download

 ↓

verify

 ↓

register

 ↓

runtime compatibility

 ↓

load test

 ↓

benchmark

 ↓

AVAILABLE

```

AI Broker recibe:

```text

modelo X disponible

capabilities = [...]

```

Posteriormente una aplicación pide:

```text

modelo X

```

y:

```text

App

 ↓

AI Broker

 ↓

LARM.resolve

 ↓

runtime instance

 ↓

inference

```

La aplicación cliente jamás necesita saber:

```text

qué runtime

qué versión

qué puerto

qué carpeta

qué disco

qué backend

```

Todo ello queda encapsulado por LARM.

---

# 163. Objetivo estratégico

Cuando esté terminado, Local AI Runtime Manager no será simplemente:

> “un gestor de llama.cpp”.

Ni:

> “un sustituto de LM Studio”.

Será una capa de infraestructura capaz de conectar:

```text

muchos repositorios de modelos

          ↓

muchos formatos

          ↓

muchos discos

          ↓

muchos runtimes

          ↓

muchos backends de hardware

          ↓

una interfaz de capacidad unificada

```

sobre la cual AI Broker y el resto del ecosistema podrán trabajar sin depender de las particularidades de cada motor.

La arquitectura queda expresamente diseñada para que dentro de varios años puedan aparecer:

```text

nuevos formatos

nuevos runtimes

nuevos aceleradores

nuevos repositorios

nuevos tipos de modelo

```

sin tener que rediseñar el Core.

---

# 164. Baseline tecnológica externa — octubre de 2026

La arquitectura anterior está deliberadamente desacoplada del estado puntual del ecosistema, pero la implementación comienza con una situación tecnológica especialmente favorable:

- Hugging Face permite obtener metadata de archivos y tamaños antes de descargar.

- ModelScope ofrece búsqueda, descargas selectivas, caché configurable, reanudación y verificación SHA256.

- Windows ML ofrece proveedores descargables para hardware AMD, NVIDIA, Intel y Qualcomm.

- ONNX Runtime GenAI ofrece actualmente builds Windows y una capa generativa sobre ONNX Runtime.

- Foundry Local selecciona variantes apropiadas al hardware Windows y mantiene un catálogo/cache local.

- MLC LLM soporta Windows/Vulkan y servidor REST compatible con OpenAI.

- `stable-diffusion.cpp` funciona en Windows y soporta varios backends y formatos.

- `llama.cpp` puede compilarse en Windows con backend Vulkan, entre otros.

- `whisper.cpp` dispone de soporte Windows y el propio proyecto publica tamaños concretos de sus modelos.

- Lemonade ofrece APIs de administración de modelos, backends, descargas, sistema y búsquedas de catálogo, lo que permite tratarlo como un host administrable en lugar de un simple endpoint OpenAI.

---

# 165. Decisiones cerradas

Se consideran decisiones de diseño aceptadas:

```text

Nombre:

Local AI Runtime Manager / LARM

Plataforma:

Windows First

PC:

PC IA

AI Broker:

PC IA

Core:

independiente de GUI

Runtimes:

extensibles por adaptadores

Model Sources:

extensibles por adaptadores

llama.cpp:

soporte oficial

ONNX/Windows ML:

soporte oficial desde el desarrollo inicial

stable-diffusion.cpp:

soporte oficial

whisper.cpp:

soporte oficial

Lemonade:

soporte nativo y adopción

System-1:

gestionado por LARM

Hugging Face:

una fuente, no la única

Storage:

multidisco

Tamaño:

estimado antes de descarga

Ruta:

seleccionable por modelo

Move:

soportado

Inferencia del ecosistema:

AI Broker

MCP:

administración + acceso opcional a modelos

Benchmark técnico:

PC IA

Evaluación:

PC principal

Orquestación:

Agora

Actualizaciones:

side-by-side + rollback

Modelos:

hash + revision + provenance

Runtimes futuros:

sin modificar Core

Repositorios futuros:

sin modificar Core

```

---

# 166. Resultado

Esta especificación constituye la **baseline técnica completa del producto**.

La siguiente fase ya no debería consistir en seguir inventando arquitectura, sino en convertir cada módulo definido aquí en:

```text

especificación detallada del módulo

contratos

modelo de datos concreto

API concreta

tests

criterios de aceptación

prompt de implementación

```

y desarrollarlos secuencialmente hasta completar LARM.

