//! Inicialización de `tracing`: formato legible o JSON, filtro por nivel,
//! fichero bajo `paths.logs_relative_path` y redacción de secretos en toda
//! salida (P003). Sin telemetría externa: todo queda en el host.

use crate::config::{LoggingOptions, Settings};
use crate::redact::redact;
use larm_domain::error::{ErrorCode, LarmError};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

/// Instancia compartida que redacta y escribe en stdout + fichero.
pub struct RedactingSink {
    targets: Mutex<Vec<Box<dyn Write + Send>>>,
}

impl RedactingSink {
    fn new(targets: Vec<Box<dyn Write + Send>>) -> Self {
        Self {
            targets: Mutex::new(targets),
        }
    }

    fn write_line(&self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let redacted = redact(&text);
        let mut targets = self
            .targets
            .lock()
            .map_err(|_| std::io::Error::other("sink de logs bloqueado"))?;
        for t in targets.iter_mut() {
            // Un destino caído (stdout cerrado) no aborta el resto del log.
            let _ = t.write_all(redacted.as_bytes());
        }
        Ok(buf.len())
    }

    fn flush_all(&self) -> std::io::Result<()> {
        let mut targets = self
            .targets
            .lock()
            .map_err(|_| std::io::Error::other("sink de logs bloqueado"))?;
        for t in targets.iter_mut() {
            let _ = t.flush();
        }
        Ok(())
    }
}

/// Vista `Write` por evento sobre el sink compartido.
pub struct SinkWriter(Arc<RedactingSink>);

impl Write for SinkWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write_line(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush_all()
    }
}

/// Newtype: `MakeWriter` no puede implementarse sobre `Arc` ajeno al crate.
pub struct SinkMaker(Arc<RedactingSink>);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SinkMaker {
    type Writer = SinkWriter;

    fn make_writer(&'a self) -> Self::Writer {
        SinkWriter(Arc::clone(&self.0))
    }
}

/// Handle del logging; conservarlo mantiene vivos los destinos y, al
/// soltarlo, vacía lo pendiente de stdout.
#[must_use]
pub struct LoggingGuard {
    /// Carpeta de logs efectiva (rotación diaria `larm.AAAA-MM-DD.log`).
    pub logs_dir: PathBuf,
    _stdout: tracing_appender::non_blocking::WorkerGuard,
}

/// Líneas de stdout en vuelo como máximo; por encima se descartan.
pub const STDOUT_BUFFERED_LINES: usize = 10_000;

/// Timestamps de log en UTC RFC 3339 con milisegundos y `Z` (contrato 00).
struct UtcMillis;

impl tracing_subscriber::fmt::time::FormatTime for UtcMillis {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"))
    }
}

/// Ficheros diarios conservados: `diagnostics.log_retention_days` (0 = solo el del día).
#[must_use]
pub fn retained_files(settings: &Settings) -> usize {
    usize::try_from(settings.diagnostics.log_retention_days)
        .unwrap_or(usize::MAX)
        .max(1)
}

/// Instala el subscriber global. Llamar una sola vez por proceso.
///
/// # Errors
/// `VALIDATION_FAILED` si el nivel no es válido, `INTERNAL` si falla el IO de la carpeta de logs.
pub fn init(
    settings: &Settings,
    options: &LoggingOptions,
    logs_dir: PathBuf,
) -> Result<LoggingGuard, LarmError> {
    let filter = EnvFilter::try_new(&options.level).map_err(|e| {
        LarmError::new(
            ErrorCode::ValidationFailed,
            "Nivel de log no válido",
            "log_level_invalid",
        )
        .with_technical(e.to_string())
    })?;

    fs::create_dir_all(&logs_dir).map_err(|e| file_error("creando directorio de logs", &e))?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("larm")
        .filename_suffix("log")
        .max_log_files(retained_files(settings))
        .build(&logs_dir)
        .map_err(|e| {
            LarmError::new(ErrorCode::Internal, "Fallo de IO en logging", "log_io")
                .with_technical(format!("creando el fichero rotativo de logs: {e}"))
        })?;

    // stdout desde un hilo propio y con pérdida: si quien lanzó el daemon no
    // lee su salida (pipe lleno), se descartan líneas de consola en vez de
    // bloquear el daemon con el sink tomado. El fichero es el registro completo.
    let (stdout, stdout_guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(true)
        .buffered_lines_limit(STDOUT_BUFFERED_LINES)
        .thread_name("larm-log-stdout")
        .finish(std::io::stdout());
    let sink = SinkMaker(Arc::new(RedactingSink::new(vec![
        Box::new(stdout),
        Box::new(appender),
    ])));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(sink)
        .with_timer(UtcMillis)
        .with_target(true)
        .with_ansi(false);

    if options.format == "json" {
        builder.json().init();
    } else {
        builder.init();
    }

    tracing::info!(
        log_format = %options.format,
        log_level = %options.level,
        log_retention_days = settings.diagnostics.log_retention_days,
        external_telemetry = settings.privacy.external_telemetry,
        "logging inicializado (salida local; telemetría externa desactivada)"
    );
    Ok(LoggingGuard {
        logs_dir,
        _stdout: stdout_guard,
    })
}

/// Redacción accesible para componentes que formatean texto manualmente.
#[must_use]
pub fn redact_line(input: &str) -> String {
    redact(input)
}

fn file_error(context: &str, e: &std::io::Error) -> LarmError {
    LarmError::new(ErrorCode::Internal, "Fallo de IO en logging", "log_io")
        .with_technical(format!("{context}: {e}"))
        .with_original("io", &format!("{:?}", e.kind()), &e.to_string())
}
