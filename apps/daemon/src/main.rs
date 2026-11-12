//! Daemon de LARM: configuración por capas, logging estructurado sin secretos,
//! Ctrl+C y salida limpia con código 0 (P001+P003); base de datos (P004);
//! principales, credenciales y aprobaciones (P009).
#![deny(unsafe_code)] // única excepción: winsec.rs (ADR-0009)

mod adapters;
mod admin;
mod adoption_references;
mod bootstrap;
mod bridge;
mod download_execution;
#[cfg(windows)]
mod guardian;
mod hardware;
mod model_import_preview;
mod model_search;
mod preflight;
mod probe_profiles;
mod probes;
mod runtime_discovery;
mod runtime_install;
mod runtime_instance_process;
mod runtime_instance_start;
mod serve;
mod source_probes;
mod source_search;
mod source_watches;
mod sources;
#[cfg(windows)]
mod transformation_recovery;
#[cfg(windows)]
mod transformations;
#[cfg(windows)]
mod winsec;

use clap::{Parser, Subcommand};
use larm_core::config::{LoggingOptions, Settings};
use std::path::PathBuf;

/// Argumentos de línea de comandos (capa superior de precedencia).
#[derive(Parser, Debug)]
#[command(name = "larm-daemon", about = "Daemon administrativo de LARM")]
struct Cli {
    /// Ruta al fichero de configuración JSON (parcial o completo).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Bind de la API (default del contrato: 127.0.0.1).
    #[arg(long)]
    bind: Option<String>,
    /// Puerto de la API (default del contrato: 8790).
    #[arg(long)]
    port: Option<u16>,
    /// Raíz de datos (sobrescribe `paths.data_root`).
    #[arg(long)]
    data_root: Option<String>,
    /// Formato de log: pretty | json.
    #[arg(long)]
    log_format: Option<String>,
    /// Nivel de log: error | warn | info | debug | trace.
    #[arg(long)]
    log_level: Option<String>,
    /// Puente Broker–LARM: fichero JSON con la adopción externa. Implica `--serve-api`.
    #[arg(long)]
    bridge_config: Option<PathBuf>,
    /// Sirve la API (rutas de seguridad y, con `--bridge-config`, el puente) y
    /// el pipe de bootstrap del escritorio. Sin él, el daemon arranca sin API.
    #[arg(long)]
    serve_api: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Internal native process owner. Bootstrap is accepted only through stdin.
    #[cfg(windows)]
    #[command(hide = true)]
    WorkerGuardian,
    /// Registra un binding de inventario runtime; no instala ni valida el engine.
    RegisterRuntimeAdapter {
        #[arg(long)]
        installation: String,
        #[arg(long)]
        runtime_family: String,
    },
    /// Descubre versiones con la sesión P022/P026 del actor y guarda el inventario.
    /// No instala ni promueve una versión.
    DiscoverRuntime {
        #[arg(long)]
        runtime_id: String,
        #[arg(long)]
        actor_id: String,
    },
    /// Previsualiza un plan de instalación runtime desde un JSON InstallRequest.
    /// Abre y cierra el worker cualificado, pero no crea jobs ni instala.
    PreviewRuntimeInstall {
        #[arg(long)]
        request: PathBuf,
        #[arg(long, default_value = "local-operator")]
        actor_id: String,
    },
    /// Registra datos de una receta compilada para una instalación local.
    /// Consume una aprobación P022; no ejecuta ni valida el paquete.
    RegisterAdapterProbeProfile {
        #[arg(long)]
        installation: String,
        #[arg(long)]
        actor_id: String,
        #[arg(long)]
        approval_id: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        expected_profile: Option<String>,
    },
    /// Registra el binding de una instalación source existente. No concede
    /// confianza, VALIDATED ni permisos de ejecución.
    RegisterSourceAdapter {
        #[arg(long)]
        installation: String,
        #[arg(long)]
        source_type: String,
        /// SearchSupport cerrado: search_applicable, supported_filters, limitations.
        #[arg(long)]
        descriptor: PathBuf,
    },
    /// Imprime la configuración efectiva (defaults < fichero < entorno < CLI) en JSON y sale.
    PrintSettings,
    /// Genera el OpenAPI 3.1 desde los contratos. Con `--check`, falla si `--out` difiere.
    Openapi {
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        check: bool,
    },
    /// Inspecciona los ZIP de una carpeta, sin instalar ni ejecutar contenido.
    InspectAdapterRegistry {
        #[arg(long)]
        folder: PathBuf,
    },
    /// Comprueba un ZIP y su hash sin escribir ni ejecutar su contenido.
    InspectAdapterPackage {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        expected_hash: String,
    },
    /// Consulta o cambia la selección local. Promover exige VALIDATED;
    /// nunca ejecuta ni acredita la conformidad de un paquete.
    AdapterSelection {
        #[arg(long)]
        adapter_id: String,
        /// Identidad local de la concesión P022 registrada; no es un token API.
        #[arg(long, default_value = "local-operator")]
        actor_id: String,
        #[arg(long, requires = "expected_revision", conflicts_with_all = ["rollback", "enabled"])]
        promote: Option<String>,
        #[arg(long, requires = "expected_revision", conflicts_with_all = ["promote", "enabled"])]
        rollback: bool,
        #[arg(long, requires = "expected_revision", conflicts_with_all = ["promote", "rollback"], action = clap::ArgAction::Set)]
        enabled: Option<bool>,
        #[arg(long)]
        expected_revision: Option<i64>,
    },
    /// Actualiza side-by-side desde un AdapterImport JSON; no promueve ni
    /// ejecuta la candidata. Identidad del operador local de Windows.
    UpdateAdapter {
        #[arg(long)]
        installation: String,
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        idempotency_key: String,
        #[arg(long)]
        trust_store: Option<PathBuf>,
    },
    /// Reconcilia trabajos ZIP pendientes; conserva el directorio final si
    /// no se puede demostrar su registro seguro. No ejecuta adaptadores.
    RecoverAdapterUpdates {
        #[arg(long)]
        trust_store: Option<PathBuf>,
    },
    /// Abre y migra la base de datos e imprime su estado en JSON (sin datos sensibles).
    DbInfo,
    /// Copia de seguridad atómica de la base (VACUUM INTO) y verificación contra la viva.
    DbBackup {
        /// Fichero destino; un backup previo solo se sustituye si el nuevo se completa.
        #[arg(long)]
        out: PathBuf,
    },
    /// Restaura un backup verificado. El daemon no debe estar usando la base.
    DbRestore {
        /// Backup a restaurar.
        #[arg(long)]
        from: PathBuf,
    },
    /// Da de alta un principal de la API e imprime su token una sola vez.
    EnrollPrincipal {
        /// Nombre (owner_id de sus recursos).
        #[arg(long)]
        name: String,
        /// OPERATOR | DESKTOP | BROKER | MCP.
        #[arg(long)]
        kind: String,
        /// Scopes de API separados por comas (p. ej. `read,leases:manage`).
        #[arg(long, default_value = "")]
        scopes: String,
        /// Permisos MCP separados por comas (solo principales MCP).
        #[arg(long, default_value = "")]
        mcp_scopes: String,
        /// Usar el token de la primera línea de stdin en vez de generarlo.
        #[arg(long)]
        token_stdin: bool,
    },
    /// Emite un token nuevo para un principal; el anterior deja de valer.
    RotatePrincipal {
        #[arg(long)]
        name: String,
    },
    /// Revoca un principal: borra su token y su referencia.
    RevokePrincipal {
        #[arg(long)]
        name: String,
    },
    /// Lista los principales registrados (sin tokens).
    ListPrincipals,
    /// Sondea OS, CPU y memoria e imprime el snapshot con su huella. Con
    /// `--record`, lo registra en la base como hace el daemon al arrancar.
    HardwareProbe {
        #[arg(long)]
        record: bool,
    },
    /// Sondeo nativo de aceleradores (ADR-0010). Lo lanza el propio daemon
    /// como proceso hijo; imprime un JSON y sale.
    /// Valida un fichero de LaunchParameters (H03–H04) e imprime su
    /// `profile_hash` (RFC 8785 + sha256).
    LaunchProfileHash {
        #[arg(long)]
        file: PathBuf,
    },
    /// Preflight de espacio (P015) de una operación descrita en JSON
    /// (`{"phases": [...], "hard_budget_bytes": null}`; el volumen
    /// `$destination` es el de la ubicación evaluada). Con `--location` y
    /// `--job`, admite la operación y reserva el espacio por volumen.
    StoragePreflight {
        #[arg(long)]
        file: PathBuf,
        #[arg(long, requires = "job")]
        location: Option<String>,
        #[arg(long, requires = "location")]
        job: Option<String>,
    },
    /// Registra modelos (P040) desde un JSON `[ModelImport, …]`: agrupa por
    /// identidad upstream explícita, nunca por nombre, y es idempotente.
    /// Imprime un informe por modelo.
    ModelsImport {
        #[arg(long)]
        file: PathBuf,
    },
    /// Importa una carpeta de modelos (P017). `adopt` (por defecto) registra
    /// los ficheros donde están sin tocarlos (EXTERNAL, o HOST_MANAGED con
    /// `--host-managed`); `copy` los copia verificados al CAS de la
    /// ubicación. Los ficheros no reconocidos se ignoran y se informan.
    ModelsImportDir {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        location: String,
        #[arg(long)]
        source: String,
        #[arg(long, default_value = "adopt")]
        mode: String,
        #[arg(long)]
        host_managed: bool,
    },
    /// Reconcilia los artefactos con el estado de sus volúmenes (P018):
    /// OFFLINE/MISSING sin borrar nada; al volver, reverifica los bytes.
    StorageReconcile,
    /// Reconstruye el registro (P018) desde los manifiestos de las raíces
    /// dadas en una base de staging nueva (`--out`), que se revisa y se
    /// fusiona con `db-restore --from`. No escribe en las raíces.
    RegistryRebuild {
        #[arg(long = "root", required = true)]
        roots: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Ficheros con el mismo hash en más de un sitio y el ahorro posible.
    /// No borra nada.
    StorageDuplicates,
    /// Vista de una variante para un runtime (P017): los nombres originales
    /// enlazados a los blobs del CAS de `--location`; sin enlace posible,
    /// referencias separadas en `view.json`.
    RuntimeView {
        #[arg(long)]
        location: String,
        #[arg(long)]
        variant: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Verifica el bundle de una variante (P041) contra los ficheros de
    /// `--dir`: hashea cada artefacto y guarda VERIFIED, INCOMPLETE o
    /// CORRUPT con su evidencia.
    BundleVerify {
        #[arg(long)]
        variant: String,
        #[arg(long)]
        dir: PathBuf,
    },
    /// Sondea los volúmenes (identidad, clase, capacidad y espacio libre) e
    /// imprime la observación. `--unc` añade raíces de red que comprobar.
    StorageProbe {
        #[arg(long = "unc")]
        unc: Vec<String>,
    },
    /// Sondeo nativo de almacenamiento (ADR-0010): volúmenes, unidades de red
    /// y las raíces UNC pedidas. Lo lanza el propio daemon; imprime un JSON.
    #[command(hide = true)]
    ProbeStorage {
        /// Raíz UNC que comprobar (`\\servidor\recurso`).
        #[arg(long = "unc")]
        unc: Vec<String>,
    },
    #[command(hide = true)]
    ProbeAccelerators {
        /// DLL de las que leer la versión de fichero.
        #[arg(long = "file")]
        files: Vec<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();

    #[cfg(windows)]
    if matches!(cli.command, Some(Command::WorkerGuardian)) {
        guardian::main();
        return;
    }

    // defaults < config file < env < cli
    let file_value = cli
        .config
        .as_ref()
        .map(|p| match std::fs::read_to_string(p) {
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("config {} no es JSON válido: {e}", p.display());
                    std::process::exit(2);
                }
            },
            Err(e) => {
                eprintln!("no se pudo leer config {}: {e}", p.display());
                std::process::exit(2);
            }
        });

    let env_value = Settings::overrides_from_env();
    let cli_value: serde_json::Value = {
        let mut m = serde_json::Map::new();
        let mut api = serde_json::Map::new();
        if let Some(b) = &cli.bind {
            api.insert("bind".into(), serde_json::Value::String(b.clone()));
        }
        if let Some(p) = cli.port {
            api.insert("port".into(), serde_json::Value::from(p));
        }
        if !api.is_empty() {
            m.insert("api".into(), serde_json::Value::Object(api));
        }
        if let Some(d) = &cli.data_root {
            m.insert("paths".into(), serde_json::json!({ "data_root": d }));
        }
        serde_json::Value::Object(m)
    };

    let settings = Settings::compose(file_value.as_ref(), Some(&env_value), Some(&cli_value))
        .unwrap_or_else(|e| {
            eprintln!(
                "configuración inválida: {} | detalle: {}",
                e.user_message(),
                e.technical_detail()
            );
            std::process::exit(2);
        });

    if let Some(Command::ProbeAccelerators { files }) = &cli.command {
        std::process::exit(probe_accelerators(files));
    }
    if let Some(Command::LaunchProfileHash { file }) = &cli.command {
        std::process::exit(launch_profile_hash(file));
    }
    if let Some(Command::ProbeStorage { unc }) = &cli.command {
        std::process::exit(probe_storage(unc));
    }

    if let Some(Command::Openapi { out, check }) = &cli.command {
        std::process::exit(openapi(out, *check));
    }
    if matches!(
        cli.command,
        Some(Command::InspectAdapterRegistry { .. } | Command::InspectAdapterPackage { .. })
    ) {
        std::process::exit(adapters::inspect(&cli.command));
    }

    if matches!(cli.command, Some(Command::PrintSettings)) {
        match serde_json::to_string_pretty(&settings) {
            Ok(json) => {
                println!("{json}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("no se pudo serializar la configuración: {e}");
                std::process::exit(2);
            }
        }
    }

    let env_level = std::env::var("LARM_LOG_LEVEL").ok();
    let env_format = std::env::var("LARM_LOG_FORMAT").ok();
    let log_options = LoggingOptions::resolve(
        env_level.as_deref(),
        env_format.as_deref(),
        cli.log_level.as_deref(),
        cli.log_format.as_deref(),
    )
    .unwrap_or_else(|e| {
        eprintln!(
            "configuración inválida: {} | detalle: {}",
            e.user_message(),
            e.technical_detail()
        );
        std::process::exit(2);
    });

    let data_root = expand_data_root(&settings.paths.data_root);

    if matches!(
        cli.command,
        Some(
            Command::DbInfo
                | Command::RegisterRuntimeAdapter { .. }
                | Command::DiscoverRuntime { .. }
                | Command::PreviewRuntimeInstall { .. }
                | Command::RegisterAdapterProbeProfile { .. }
                | Command::RegisterSourceAdapter { .. }
                | Command::AdapterSelection { .. }
                | Command::UpdateAdapter { .. }
                | Command::RecoverAdapterUpdates { .. }
                | Command::DbBackup { .. }
                | Command::DbRestore { .. }
                | Command::EnrollPrincipal { .. }
                | Command::RotatePrincipal { .. }
                | Command::RevokePrincipal { .. }
                | Command::ListPrincipals
                | Command::HardwareProbe { .. }
                | Command::StorageProbe { .. }
                | Command::StoragePreflight { .. }
                | Command::ModelsImport { .. }
                | Command::BundleVerify { .. }
                | Command::ModelsImportDir { .. }
                | Command::StorageDuplicates
                | Command::StorageReconcile
                | Command::RegistryRebuild { .. }
                | Command::RuntimeView { .. }
        )
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|e| {
                eprintln!("no se pudo crear el runtime Tokio: {e}");
                std::process::exit(1);
            });
        let database = data_root.join(&settings.paths.database_relative_path);
        let result = runtime.block_on(async {
            match &cli.command {
                Some(Command::DbBackup { out }) => db_backup(&database, out).await,
                Some(Command::DbRestore { from }) => match larm_adapter_host::daemon_owner::DaemonOwnership::acquire_for_database(&data_root,std::path::Path::new(&settings.paths.database_relative_path)) {
                    Ok(owner)=>{ owner.recheck_database(std::path::Path::new(&settings.paths.database_relative_path))?;db_restore(from,&database).await },
                    Err(error)=>Err(error),
                },
                Some(Command::EnrollPrincipal {
                    name,
                    kind,
                    scopes,
                    mcp_scopes,
                    token_stdin,
                }) => {
                    let token = if *token_stdin {
                        let mut line = String::new();
                        if std::io::stdin().read_line(&mut line).is_err() {
                            eprintln!("no se pudo leer el token de stdin");
                            std::process::exit(2);
                        }
                        Some(line.trim_end_matches(['\r', '\n']).to_string())
                    } else {
                        None
                    };
                    match admin::open_db(&settings, &data_root).await {
                        Ok(db) => admin::enroll(&db, name, kind, scopes, mcp_scopes, token)
                            .await
                            .and_then(|v| to_json(&v)),
                        Err(e) => Err(e),
                    }
                }
                Some(Command::RotatePrincipal { name }) => {
                    match admin::open_db(&settings, &data_root).await {
                        Ok(db) => admin::rotate(&db, name).await.and_then(|v| to_json(&v)),
                        Err(e) => Err(e),
                    }
                }
                Some(Command::RevokePrincipal { name }) => {
                    match admin::open_db(&settings, &data_root).await {
                        Ok(db) => admin::revoke(&db, name).await.and_then(|v| to_json(&v)),
                        Err(e) => Err(e),
                    }
                }
                Some(Command::ListPrincipals) => {
                    match admin::open_db(&settings, &data_root).await {
                        Ok(db) => admin::list(&db).await.and_then(|v| to_json(&v)),
                        Err(e) => Err(e),
                    }
                }
                Some(Command::DbInfo) => db_info(&settings, &data_root).await,
                Some(Command::RegisterRuntimeAdapter { installation, runtime_family }) => match admin::open_db(&settings,&data_root).await {
                    Ok(db) => larm_domain::runtime_ports::RuntimeRepository::register_runtime_adapter(
                        &larm_db::runtimes::SqlRuntimes { pool: db.pool }, installation, runtime_family,
                        &larm_domain::ports::MutationContext { actor_id: "local-operator".into(), correlation_id: uuid::Uuid::now_v7().to_string() }
                    ).await.and_then(|value|to_json(&value)),
                    Err(error) => Err(error),
                },
                Some(Command::DiscoverRuntime { runtime_id, actor_id }) => match admin::open_db(&settings, &data_root).await {
                    Ok(db) => runtime_discovery::discover(&db, runtime_id, actor_id)
                        .await
                        .and_then(|value| to_json(&value)),
                    Err(error) => Err(error),
                },
                Some(Command::PreviewRuntimeInstall { request, actor_id }) => match admin::open_db(&settings, &data_root).await {
                    Ok(db) => runtime_install::plan_only(&db, request, actor_id)
                        .await
                        .and_then(|value| to_json(&value)),
                    Err(error) => Err(error),
                },
                Some(Command::RegisterAdapterProbeProfile{installation,actor_id,approval_id,file,expected_profile}) => match admin::open_db(&settings,&data_root).await {
                    Ok(db)=>probe_profiles::register(&db,&data_root,installation,actor_id,approval_id,file,expected_profile.as_deref()).await.and_then(|p|to_json(&serde_json::json!({"profile_id":p.id,"installation_id":p.installation_id}))),
                    Err(e)=>Err(e),
                },
                Some(Command::RegisterSourceAdapter {
                    installation,
                    source_type,
                    descriptor,
                }) => match admin::open_db(&settings, &data_root).await {
                    Ok(db) => sources::register(&db, installation, source_type, descriptor)
                        .await
                        .and_then(|v| to_json(&v)),
                    Err(e) => Err(e),
                },
                Some(Command::AdapterSelection {
                    adapter_id,
                    actor_id,
                    promote,
                    rollback,
                    enabled,
                    expected_revision,
                }) => match admin::open_db(&settings, &data_root).await {
                    Ok(db) => {
                        let runner=probes::runner(&db);
                        let result=adapters::selection(
                        &db,
                        adapter_id,
                        promote.as_deref(),
                        *rollback,
                        *enabled,
                        *expected_revision,
                        adapters::SelectionContext{mutation:larm_domain::ports::MutationContext{actor_id:actor_id.clone(),correlation_id:uuid::Uuid::now_v7().to_string()},qualification:runner.clone()},
                    )
                    .await;
                        let closed=runner.shutdown().await;
                        match result {Ok(value)=>closed.and_then(|()|to_json(&value)),Err(error)=>Err(error)}
                    },
                    Err(e) => Err(e),
                },
                Some(Command::StoragePreflight {
                    file,
                    location,
                    job,
                }) => match admin::open_db(&settings, &data_root).await {
                    Ok(db) => {
                        let target = location.as_deref().zip(job.as_deref());
                        preflight::run(&db, &settings, file, target)
                            .await
                            .and_then(|v| to_json(&v))
                    }
                    Err(e) => Err(e),
                },
                Some(Command::UpdateAdapter {
                    installation,
                    request,
                    idempotency_key,
                    trust_store,
                }) => match admin::open_db(&settings, &data_root).await {
                    Ok(db) => adapters::update(
                        &db,
                        &settings,
                        &data_root,
                        installation,
                        request,
                        idempotency_key,
                        trust_store.as_deref(),
                    )
                    .await
                    .and_then(|v| to_json(&v)),
                    Err(e) => Err(e),
                },
                Some(Command::RecoverAdapterUpdates { trust_store }) => {
                    match admin::open_db(&settings, &data_root).await {
                        Ok(db) => match adapters::coordinator(
                            &db,
                            &settings,
                            &data_root,
                            trust_store.as_deref(),
                        ) {
                            Ok(c) => c
                                .recover()
                                .await
                                .and_then(|n| to_json(&serde_json::json!({"reconciled_jobs":n}))),
                            Err(e) => Err(e),
                        },
                        Err(e) => Err(e),
                    }
                }
                Some(Command::ModelsImport { file }) => {
                    match admin::open_db(&settings, &data_root).await {
                        Ok(db) => models_import(&db, file).await.and_then(|v| to_json(&v)),
                        Err(e) => Err(e),
                    }
                }
                Some(Command::ModelsImportDir {
                    dir,
                    location,
                    source,
                    mode,
                    host_managed,
                }) => match admin::open_db(&settings, &data_root).await {
                    Ok(db) => models_import_dir(&db, dir, location, source, mode, *host_managed)
                        .await
                        .and_then(|v| to_json(&v)),
                    Err(e) => Err(e),
                },
                Some(Command::StorageReconcile) => {
                    match admin::open_db(&settings, &data_root).await {
                        Ok(db) => match hardware::reconciler(&db) {
                            Some(r) => r
                                .run(&larm_domain::ports::MutationContext {
                                    actor_id: "larm-daemon:storage-reconcile".into(),
                                    correlation_id: uuid::Uuid::now_v7().to_string(),
                                })
                                .await
                                .and_then(|v| to_json(&v)),
                            None => Err(larm_domain::error::LarmError::new(
                                larm_domain::error::ErrorCode::Internal,
                                "Sin ruta del ejecutable",
                                "storage_probe_unavailable",
                            )),
                        },
                        Err(e) => Err(e),
                    }
                }
                Some(Command::RegistryRebuild { roots, out }) => {
                    registry_rebuild(&settings, roots, out)
                        .await
                        .and_then(|v| to_json(&v))
                }
                Some(Command::StorageDuplicates) => {
                    match admin::open_db(&settings, &data_root).await {
                        Ok(db) => {
                            use larm_domain::ports::ArtifactLocationRepository;
                            larm_db::artifacts::SqlArtifactLocations {
                                pool: db.pool.clone(),
                            }
                            .duplicates()
                            .await
                            .and_then(|v| to_json(&v))
                        }
                        Err(e) => Err(e),
                    }
                }
                Some(Command::RuntimeView {
                    location,
                    variant,
                    out,
                }) => match admin::open_db(&settings, &data_root).await {
                    Ok(db) => runtime_view(&db, location, variant, out)
                        .await
                        .and_then(|v| to_json(&v)),
                    Err(e) => Err(e),
                },
                Some(Command::BundleVerify { variant, dir }) => {
                    match admin::open_db(&settings, &data_root).await {
                        Ok(db) => {
                            let repo = larm_db::variants::SqlVariants {
                                pool: db.pool.clone(),
                            };
                            let ctx = larm_domain::ports::MutationContext {
                                actor_id: "larm-daemon:bundle-verify".into(),
                                correlation_id: uuid::Uuid::now_v7().to_string(),
                            };
                            larm_models::bundle::verify(&repo, variant, dir, &ctx)
                                .await
                                .and_then(|v| to_json(&v))
                        }
                        Err(e) => Err(e),
                    }
                }
                Some(Command::StorageProbe { unc }) => {
                    hardware::storage_probe(unc).await.and_then(|v| to_json(&v))
                }
                Some(Command::HardwareProbe { record: false }) => {
                    hardware::probe(None).await.and_then(|v| to_json(&v))
                }
                Some(Command::HardwareProbe { record: true }) => {
                    match admin::open_db(&settings, &data_root).await {
                        Ok(db) => hardware::probe(Some(&larm_db::hardware::SqlHardware {
                            pool: db.pool.clone(),
                        }))
                        .await
                        .and_then(|v| to_json(&v)),
                        Err(e) => Err(e),
                    }
                }
                // Filtrados por el `matches!` de arriba; brazos explícitos para que un
                // comando nuevo no acabe en db-info en silencio.
                #[cfg(windows)]
                Some(Command::WorkerGuardian) => Err(larm_domain::error::LarmError::new(
                    larm_domain::error::ErrorCode::Internal,
                    "Comando interno no despachado",
                    "command_dispatch",
                )),
                Some(
                    Command::PrintSettings
                    | Command::Openapi { .. }
                    | Command::InspectAdapterRegistry { .. }
                    | Command::InspectAdapterPackage { .. }
                    | Command::ProbeAccelerators { .. }
                    | Command::ProbeStorage { .. }
                    | Command::LaunchProfileHash { .. },
                )
                | None => Err(larm_domain::error::LarmError::new(
                    larm_domain::error::ErrorCode::Internal,
                    "Comando no despachado",
                    "command_dispatch",
                )),
            }
        });
        match result {
            Ok(json) => {
                println!("{json}");
                std::process::exit(0);
            }
            Err(e) => {
                if matches!(
                    cli.command,
                    Some(
                        Command::AdapterSelection { .. }
                            | Command::RegisterRuntimeAdapter { .. }
                            | Command::RegisterAdapterProbeProfile { .. }
                            | Command::RegisterSourceAdapter { .. }
                            | Command::UpdateAdapter { .. }
                            | Command::RecoverAdapterUpdates { .. }
                    )
                ) {
                    eprintln!("{}", serde_json::to_string(&e).unwrap_or_default());
                } else {
                    eprintln!(
                        "operación fallida: {} | código: {} | motivo: {} | detalle: {}",
                        e.user_message(),
                        e.code,
                        e.details.reason,
                        e.technical_detail()
                    );
                }
                std::process::exit(1);
            }
        }
    }
    // Physical ownership precedes logging, opening SQLite and recovery. A
    // second process must not classify another live daemon's jobs as crashed.
    let data_owner = if cli.serve_api || cli.bridge_config.is_some() {
        Some(
            larm_adapter_host::daemon_owner::DaemonOwnership::acquire_for_database(
                &data_root,
                std::path::Path::new(&settings.paths.database_relative_path),
            )
            .unwrap_or_else(|error| {
                eprintln!(
                    "{} | motivo: {}",
                    error.user_message(),
                    error.details.reason
                );
                std::process::exit(1);
            }),
        )
    } else {
        None
    };
    let logs_dir = data_root.join(&settings.paths.logs_relative_path);

    let _log_guard =
        larm_core::logging::init(&settings, &log_options, logs_dir).unwrap_or_else(|e| {
            eprintln!("fallo inicializando logging: {}", e.technical_detail());
            std::process::exit(2);
        });

    let correlation = larm_domain::correlation::ensure_correlation(None);
    tracing::info!(
        correlation_id = %correlation,
        api.bind = %settings.api.bind,
        api.port = settings.api.port,
        api.remote_enabled = settings.api.remote_enabled,
        bandwidth_limit = ?settings.storage.download_bandwidth_limit_bytes_per_second,
        thresholds = ?settings.storage.low_disk_thresholds,
        "larm-daemon arrancando"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            tracing::error!("no se pudo crear el runtime Tokio: {e}");
            std::process::exit(1);
        });
    if let Some(data_owner) = data_owner.as_ref() {
        let served = runtime.block_on(serve::run(
            &settings,
            file_value.as_ref(),
            &env_value,
            &cli_value,
            data_owner,
            cli.bridge_config.as_deref(),
            wait_for_shutdown(),
        ));
        if let Err(error) = served {
            tracing::error!(error = %error, "la API no pudo arrancar");
            std::process::exit(1);
        }
    } else {
        runtime.block_on(wait_for_shutdown());
    }

    tracing::info!(correlation_id = %correlation, "cierre limpio");
}

/// Estado de la base para diagnóstico (P004): pragmas, esquema, migraciones y filas por tabla.
/// No incluye contenido de filas; la ruta solo aparece con `privacy.include_personal_paths`.
async fn db_info(
    settings: &Settings,
    data_root: &std::path::Path,
) -> Result<String, larm_domain::error::LarmError> {
    let path = data_root.join(&settings.paths.database_relative_path);
    let db = larm_db::pool::Db::open(&path).await?;
    let newly = larm_db::migrate::migrate(&db.pool).await?;
    let health = db.health().await?;
    let migrations = larm_db::migrate::applied(&db.pool).await?;
    let rows = larm_db::backup::row_counts(&db.pool).await?;
    let sqlite_version = db.sqlite_version().await?;
    db.pool.close().await;
    let file_bytes = std::fs::metadata(&path).map(|m| m.len().to_string()).ok();
    let shown_path = if settings.privacy.include_personal_paths {
        path.display().to_string()
    } else {
        format!(
            "<data_root>/{}",
            settings.paths.database_relative_path.replace('\\', "/")
        )
    };
    let info = serde_json::json!({
        "database": shown_path,
        "file_bytes": file_bytes,
        "sqlite_version": sqlite_version,
        "health": health,
        "healthy": health.is_healthy(),
        "migrations": migrations,
        "migrated_now": newly,
        "tables": rows.len(),
        "row_counts": rows,
    });
    serde_json::to_string_pretty(&info).map_err(|e| {
        larm_domain::error::LarmError::new(
            larm_domain::error::ErrorCode::Internal,
            "No se pudo serializar db-info",
            "serialize",
        )
        .with_technical(e.to_string())
    })
}

/// Escribe (o comprueba) el OpenAPI generado. Devuelve el código de salida.
fn openapi(out: &std::path::Path, check: bool) -> i32 {
    let doc = match larm_api::openapi::document(
        env!("CARGO_PKG_VERSION"),
        &larm_api::openapi::is_implemented,
    ) {
        Ok(doc) => doc,
        Err(e) => {
            eprintln!("no se pudo generar el OpenAPI: {e}");
            return 2;
        }
    };
    let text = match serde_json::to_string_pretty(&doc) {
        Ok(t) => t + "\n",
        Err(e) => {
            eprintln!("no se pudo serializar el OpenAPI: {e}");
            return 2;
        }
    };
    if check {
        let current = std::fs::read_to_string(out).unwrap_or_default();
        if current.replace("\r\n", "\n") == text {
            println!("OpenAPI al día: {}", out.display());
            return 0;
        }
        eprintln!(
            "{} no coincide con el generado; ejecutar `larm-daemon openapi --out {}`",
            out.display(),
            out.display()
        );
        return 1;
    }
    match std::fs::write(out, text) {
        Ok(()) => {
            println!("OpenAPI escrito en {}", out.display());
            0
        }
        Err(e) => {
            eprintln!("no se pudo escribir {}: {e}", out.display());
            2
        }
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, larm_domain::error::LarmError> {
    serde_json::to_string_pretty(value).map_err(|e| {
        larm_domain::error::LarmError::new(
            larm_domain::error::ErrorCode::Internal,
            "No se pudo serializar el resultado",
            "serialize",
        )
        .with_technical(e.to_string())
    })
}

/// Backup atómico de la base y verificación contra la viva (P004, §132).
async fn db_backup(
    database: &std::path::Path,
    out: &std::path::Path,
) -> Result<String, larm_domain::error::LarmError> {
    let db = larm_db::pool::Db::open(database).await?;
    larm_db::migrate::migrate(&db.pool).await?;
    larm_db::backup::backup(&db.pool, out).await?;
    let verification = larm_db::backup::verify_backup(&db.pool, out).await?;
    db.pool.close().await;
    if !verification.is_valid() {
        return Err(larm_domain::error::LarmError::new(
            larm_domain::error::ErrorCode::IntegrityFailed,
            "El backup no coincide con la base",
            "backup_mismatch",
        ));
    }
    to_json(&serde_json::json!({ "backup": "ok", "verification": verification }))
}

/// Restaura un backup verificado sobre la base de la raíz de datos.
async fn db_restore(
    from: &std::path::Path,
    database: &std::path::Path,
) -> Result<String, larm_domain::error::LarmError> {
    let report = larm_db::backup::restore(from, database).await?;
    to_json(&serde_json::json!({
        "restore": "ok",
        "integrity": report.integrity,
        "row_counts": report.rows,
        "previous_kept": report.previous.is_some(),
    }))
}

/// Expande `%LOCALAPPDATA%`/`%PROGRAMDATA%` del contrato de rutas.
#[must_use]
pub fn expand_data_root(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("%LOCALAPPDATA%") {
        if let Some(base) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(base).join(rest.trim_start_matches(['/', '\\']));
        }
    }
    if let Some(rest) = path.strip_prefix("%PROGRAMDATA%") {
        if let Some(base) = std::env::var_os("PROGRAMDATA") {
            return PathBuf::from(base).join(rest.trim_start_matches(['/', '\\']));
        }
    }
    PathBuf::from(path)
}

async fn wait_for_shutdown() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(windows)]
    let terminate = async {
        match tokio::signal::windows::ctrl_break() {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(windows))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(windows)]
fn probe_accelerators(files: &[PathBuf]) -> i32 {
    match serde_json::to_string(&larm_hardware::native::probe(files)) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(e) => {
            eprintln!("no se pudo serializar el sondeo nativo: {e}");
            1
        }
    }
}

#[cfg(not(windows))]
fn probe_accelerators(_: &[PathBuf]) -> i32 {
    eprintln!("el sondeo nativo de aceleradores solo existe en Windows");
    2
}

#[cfg(windows)]
fn probe_storage(unc: &[String]) -> i32 {
    match serde_json::to_string(&larm_hardware::native::probe_storage(unc)) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(e) => {
            eprintln!("no se pudo serializar el sondeo de almacenamiento: {e}");
            1
        }
    }
}

#[cfg(not(windows))]
fn probe_storage(_: &[String]) -> i32 {
    eprintln!("el sondeo nativo de almacenamiento solo existe en Windows");
    2
}

fn launch_profile_hash(file: &std::path::Path) -> i32 {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("no se pudo leer {}: {e}", file.display());
            return 2;
        }
    };
    let parameters: larm_domain::launch::LaunchParameters = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(e) => {
            println!(
                "{}",
                serde_json::json!({"valid": false, "reason": "schema_violation", "detail": e.to_string()})
            );
            return 1;
        }
    };
    match parameters.profile_hash() {
        Ok(hash) => {
            println!(
                "{}",
                serde_json::json!({"valid": true, "profile_hash": hash})
            );
            0
        }
        Err(e) => {
            println!(
                "{}",
                serde_json::json!({"valid": false, "reason": e.reason, "detail": e.detail})
            );
            1
        }
    }
}

/// `models-import`: cada modelo se registra por separado; el primer error
/// detiene la importación (lo ya registrado queda, y repetir es idempotente).
async fn models_import(
    db: &larm_db::pool::Db,
    file: &std::path::Path,
) -> Result<Vec<larm_models::registry::ImportReport>, larm_domain::error::LarmError> {
    let invalid = |detail: String| {
        larm_domain::error::LarmError::new(
            larm_domain::error::ErrorCode::ValidationFailed,
            "Importación no válida",
            "schema_violation",
        )
        .with_technical(detail)
    };
    let text =
        std::fs::read_to_string(file).map_err(|e| invalid(format!("{}: {e}", file.display())))?;
    let imports: Vec<larm_models::registry::ModelImport> =
        serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
    let repo = larm_db::models::SqlModels {
        pool: db.pool.clone(),
    };
    let variants = larm_db::variants::SqlVariants {
        pool: db.pool.clone(),
    };
    let ctx = larm_domain::ports::MutationContext {
        actor_id: "larm-daemon:models-import".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let mut reports = Vec::with_capacity(imports.len());
    for import in &imports {
        reports.push(larm_models::registry::register(&repo, &variants, import, &ctx).await?);
    }
    Ok(reports)
}

async fn location_root(
    db: &larm_db::pool::Db,
    id: &str,
) -> Result<std::path::PathBuf, larm_domain::error::LarmError> {
    use larm_domain::ports::StorageLocationRepository;
    let l = larm_db::locations::SqlStorageLocations {
        pool: db.pool.clone(),
    }
    .get(id)
    .await?
    .ok_or_else(|| {
        larm_domain::error::LarmError::new(
            larm_domain::error::ErrorCode::NotFound,
            "Ubicación desconocida",
            "not_found",
        )
        .with_resource(id.to_string())
    })?;
    Ok(std::path::PathBuf::from(l.canonical_path))
}

/// `models-import-dir` (P017).
async fn models_import_dir(
    db: &larm_db::pool::Db,
    dir: &std::path::Path,
    location: &str,
    source: &str,
    mode: &str,
    host_managed: bool,
) -> Result<larm_models::import::DirImportReport, larm_domain::error::LarmError> {
    use larm_models::import::{import_dir, ImportMode, ImportOptions};
    let mode = match mode {
        "adopt" => ImportMode::Adopt,
        "copy" => ImportMode::Copy,
        other => {
            return Err(larm_domain::error::LarmError::new(
                larm_domain::error::ErrorCode::ValidationFailed,
                "Modo de importación desconocido (adopt o copy)",
                "import_mode_invalid",
            )
            .with_technical(other.to_string()))
        }
    };
    let options = ImportOptions {
        dir: dir.to_path_buf(),
        storage_location_id: location.to_string(),
        location_root: location_root(db, location).await?,
        source_id: source.to_string(),
        mode,
        format: None,
        license_id: None,
        adopt_as: if host_managed {
            larm_domain::artifacts::Ownership::HostManaged
        } else {
            larm_domain::artifacts::Ownership::External
        },
    };
    let ctx = larm_domain::ports::MutationContext {
        actor_id: "larm-daemon:models-import-dir".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    import_dir(
        &larm_db::models::SqlModels {
            pool: db.pool.clone(),
        },
        &larm_db::variants::SqlVariants {
            pool: db.pool.clone(),
        },
        &larm_db::artifacts::SqlArtifactLocations {
            pool: db.pool.clone(),
        },
        &options,
        &ctx,
    )
    .await
}

/// `runtime-view` (P017).
async fn runtime_view(
    db: &larm_db::pool::Db,
    location: &str,
    variant: &str,
    out: &std::path::Path,
) -> Result<Vec<larm_storage::cas::ViewEntry>, larm_domain::error::LarmError> {
    use larm_domain::ports::VariantRepository;
    let root = location_root(db, location).await?;
    let files: Vec<(String, String)> = larm_db::variants::SqlVariants {
        pool: db.pool.clone(),
    }
    .artifacts_of(variant)
    .await?
    .into_iter()
    .filter_map(|a| a.sha256.map(|h| (a.filename, h)))
    .collect();
    let out = out.to_path_buf();
    tokio::task::spawn_blocking(move || larm_storage::cas::runtime_view(&root, &out, &files))
        .await
        .map_err(|e| {
            larm_domain::error::LarmError::new(
                larm_domain::error::ErrorCode::Internal,
                "Vista interrumpida",
                "join",
            )
            .with_technical(e.to_string())
        })?
        .map_err(|e| {
            larm_domain::error::LarmError::new(
                larm_domain::error::ErrorCode::ResourceUnavailable,
                "No se pudo crear la vista",
                "io_failed",
            )
            .with_technical(e.to_string())
        })
}

/// `registry-rebuild` (P018): base de staging nueva con lo recuperable de
/// los manifiestos de cada raíz. La base viva no se toca.
async fn registry_rebuild(
    settings: &larm_core::config::Settings,
    roots: &[std::path::PathBuf],
    out: &std::path::Path,
) -> Result<serde_json::Value, larm_domain::error::LarmError> {
    use larm_domain::error::{ErrorCode, LarmError};
    use larm_domain::ports::LocationService;
    if out.exists() {
        return Err(LarmError::new(
            ErrorCode::Conflict,
            "La base de staging ya existe: elige otra ruta",
            "staging_exists",
        ));
    }
    let work = out.with_extension("building");
    let _ = std::fs::remove_file(&work);
    let db = larm_db::pool::Db::open(&work).await?;
    larm_db::migrate::migrate(&db.pool).await?;
    let ctx = larm_domain::ports::MutationContext {
        actor_id: "larm-daemon:registry-rebuild".into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let prober = hardware::storage_prober().ok_or_else(|| {
        LarmError::new(
            ErrorCode::Internal,
            "Sin ruta del ejecutable",
            "storage_probe_unavailable",
        )
    })?;
    let locations = larm_storage::locations::LocationServiceImpl {
        repo: std::sync::Arc::new(larm_db::locations::SqlStorageLocations {
            pool: db.pool.clone(),
        }),
        prober: std::sync::Arc::new(prober),
        thresholds: Some(hardware::thresholds(settings)),
    };
    let models = larm_db::models::SqlModels {
        pool: db.pool.clone(),
    };
    let variants = larm_db::variants::SqlVariants {
        pool: db.pool.clone(),
    };
    let artifacts = larm_db::artifacts::SqlArtifactLocations {
        pool: db.pool.clone(),
    };
    let mut reports = Vec::new();
    for root in roots {
        let scan_root = root.clone();
        let scan =
            tokio::task::spawn_blocking(move || larm_storage::rebuild::scan_root(&scan_root))
                .await
                .map_err(|e| {
                    LarmError::new(ErrorCode::Internal, "Escaneo interrumpido", "join")
                        .with_technical(e.to_string())
                })?
                .map_err(|e| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "No se pudo leer la raíz",
                        "io_failed",
                    )
                    .with_technical(format!("{}: {e}", root.display()))
                })?;
        let name = root.file_name().map_or_else(
            || "recuperada".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let location = locations
            .create(
                &larm_domain::locations::StorageLocationCreate {
                    name,
                    root_path: root.display().to_string(),
                    reserved_bytes: larm_domain::units::DecimalU64::default(),
                    priority: 0,
                    auto_placement: false,
                    performance_class: larm_domain::storage::PerformanceClass::Unknown,
                },
                &ctx,
            )
            .await?;
        // Las fuentes de los manifiestos se recrean como marcadores sin
        // credenciales ni adaptador: no se inventa su configuración.
        let mut sources: Vec<String> = scan
            .manifests
            .iter()
            .map(|m| m.manifest.distribution.source_id.clone())
            .collect();
        sources.sort();
        sources.dedup();
        larm_db::models::insert_recovered_sources(&db.pool, &sources).await?;
        let report = larm_models::rebuild::rebuild(
            &models,
            &variants,
            &artifacts,
            &location.id,
            &scan,
            &ctx,
        )
        .await?;
        reports.push(serde_json::json!({
            "root": root.display().to_string(),
            "storage_location_id": location.id,
            "report": report,
        }));
    }
    // Una base de un solo fichero (sin -wal) lista para `db-restore --from`.
    larm_db::backup::backup(&db.pool, out).await?;
    db.pool.close().await;
    let _ = std::fs::remove_file(&work);
    let _ = std::fs::remove_file(work.with_extension("building-wal"));
    let _ = std::fs::remove_file(work.with_extension("building-shm"));
    Ok(serde_json::json!({"staging": out.display().to_string(), "roots": reports}))
}
