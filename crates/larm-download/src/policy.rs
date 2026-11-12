//! Políticas deterministas de cola, transporte, reintentos, integridad y caudal.
//!
//! Este módulo no crea jobs ni accede a la red o al disco. El servicio durable
//! debe aplicar estas decisiones después de reservar espacio y autorizar la
//! fuente mediante los componentes P015/P019/P022/P043.

use larm_domain::{download::DownloadPriority, model::Trust};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU64,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::Notify;

const NANOS_PER_SECOND: u128 = 1_000_000_000;
const MAX_RETRY_AFTER_SECONDS: u64 = 60;
const RETRY_BASE_SECONDS: [u64; 5] = [1, 2, 4, 8, 16];
const RETRY_JITTER_MAX_MILLIS: u64 = 250;

/// Un job pendiente junto con su clave FIFO contractual.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueCandidate {
    pub job_id: String,
    pub source_id: String,
    /// Timestamp RFC 3339 UTC con milisegundos, comparable lexicográficamente.
    pub created_at: String,
    pub priority: DownloadPriority,
}

/// Selecciona nuevas plazas sin desalojar jobs activos.
///
/// Devuelve IDs en orden HIGH, NORMAL, BACKGROUND y FIFO por fecha/ID dentro
/// de cada prioridad. Los límites global y por fuente incluyen los jobs ya
/// activos; un límite cero no habilita plazas.
#[must_use]
pub fn schedule(
    queued: &[QueueCandidate],
    active: &[QueueCandidate],
    global_limit: usize,
    per_source_limit: usize,
) -> Vec<String> {
    let mut source_counts = HashMap::<String, usize>::new();
    for job in active {
        *source_counts.entry(job.source_id.clone()).or_default() += 1;
    }

    let mut candidates = queued.to_vec();
    candidates.sort_by(|left, right| {
        right
            .priority
            .queue_rank()
            .cmp(&left.priority.queue_rank())
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.job_id.cmp(&right.job_id))
    });

    let mut active_ids: HashSet<String> = active.iter().map(|job| job.job_id.clone()).collect();
    let mut slots = global_limit.saturating_sub(active.len());
    let mut selected = Vec::new();

    for job in candidates {
        if slots == 0 || per_source_limit == 0 || !active_ids.insert(job.job_id.clone()) {
            continue;
        }
        let count = source_counts.entry(job.source_id.clone()).or_default();
        if *count >= per_source_limit {
            continue;
        }
        *count += 1;
        slots -= 1;
        selected.push(job.job_id);
    }

    selected
}

/// Validador que permite reanudar exactamente el mismo objeto remoto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeValidator {
    /// ETag fuerte HTTP, no W/.
    StrongEtag(String),
    /// Fecha de modificación respaldada por un hash SHA-256 fijado en el plan.
    LastModifiedAndSha256 {
        last_modified: String,
        sha256: String,
    },
}

impl ResumeValidator {
    /// Comprueba que el validador es suficiente y no contiene una cabecera inválida.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::StrongEtag(value) => {
                value.len() >= 2
                    && value.starts_with('"')
                    && value.ends_with('"')
                    && !value.starts_with("W/")
                    && !value.bytes().any(|byte| byte == b'\r' || byte == b'\n')
            }
            Self::LastModifiedAndSha256 {
                last_modified,
                sha256,
            } => {
                !last_modified.is_empty()
                    && !last_modified
                        .bytes()
                        .any(|byte| byte == b'\r' || byte == b'\n')
                    && is_sha256(sha256)
            }
        }
    }

    fn matches(&self, response: &HttpResponse, expected_sha256: Option<&str>) -> bool {
        if !self.is_valid() {
            return false;
        }
        match self {
            Self::StrongEtag(expected) => response.etag.as_ref() == Some(expected),
            Self::LastModifiedAndSha256 {
                last_modified,
                sha256,
            } => {
                response.last_modified.as_ref() == Some(last_modified)
                    && expected_sha256 == Some(sha256.as_str())
            }
        }
    }
}

/// Rango Content-Range ya analizado; el total puede ser desconocido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: Option<u64>,
}

/// Cabeceras relevantes para decidir si se puede anexar una respuesta HTTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub content_range: Option<ContentRange>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_encoding: Option<String>,
}

/// Estado local que debe seguir coincidiendo con el plan aprobado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeContext {
    pub durable_offset: u64,
    pub expected_size: Option<u64>,
    pub expected_sha256: Option<String>,
    pub validator: Option<ResumeValidator>,
}

/// Resultado de evaluar la respuesta sin tocar el temporal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpDecision {
    /// Se puede escribir la respuesta empezando en el offset indicado.
    Append {
        offset: u64,
        range_end: Option<u64>,
        total: Option<u64>,
    },
    /// Debe rehacerse el preflight/reserva y truncarse sólo el temporal propio.
    RestartFromZero,
    /// La respuesta 416 sólo confirma éxito tras comparar tamaño y hash esperado.
    AlreadyComplete,
}

/// Causa por la que una respuesta no se puede aplicar al temporal actual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpPolicyError {
    UnsupportedContentEncoding,
    InvalidContentRange,
    RangeNotSatisfiable,
    UnexpectedStatus,
}

/// Valida 200/206/416 y los validadores fijados antes de anexar bytes.
pub fn decide_http_response(
    context: &ResumeContext,
    response: &HttpResponse,
    final_size_matches: bool,
    final_hash_matches_expected: bool,
) -> Result<HttpDecision, HttpPolicyError> {
    if response
        .content_encoding
        .as_deref()
        .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("identity"))
    {
        return Err(HttpPolicyError::UnsupportedContentEncoding);
    }

    match response.status {
        200 if context.durable_offset > 0 => Ok(HttpDecision::RestartFromZero),
        200 => Ok(HttpDecision::Append {
            offset: 0,
            range_end: None,
            total: context.expected_size,
        }),
        206 => {
            if context.durable_offset > 0 {
                let Some(validator) = &context.validator else {
                    return Ok(HttpDecision::RestartFromZero);
                };
                if !validator.matches(response, context.expected_sha256.as_deref()) {
                    return Ok(HttpDecision::RestartFromZero);
                }
            }
            let Some(range) = response.content_range else {
                return Err(HttpPolicyError::InvalidContentRange);
            };
            if range.start != context.durable_offset
                || range.end < range.start
                || range.end.checked_add(1).is_none()
                || range
                    .total
                    .is_some_and(|total| total <= range.end || total < range.start)
                || context
                    .expected_size
                    .is_some_and(|expected| range.total != Some(expected))
            {
                return Err(HttpPolicyError::InvalidContentRange);
            }
            Ok(HttpDecision::Append {
                offset: range.start,
                range_end: Some(range.end),
                total: range.total,
            })
        }
        416 => {
            if context.durable_offset > 0
                && context.expected_size == Some(context.durable_offset)
                && context.expected_sha256.is_some()
                && final_size_matches
                && final_hash_matches_expected
            {
                Ok(HttpDecision::AlreadyComplete)
            } else {
                Err(HttpPolicyError::RangeNotSatisfiable)
            }
        }
        _ => Err(HttpPolicyError::UnexpectedStatus),
    }
}

/// Fallos observados en una operación HTTP o de transporte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferFailure {
    Timeout,
    HttpStatus(u16),
    Network,
}

/// Acción segura posterior a un fallo de transferencia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureAction {
    RetryAfter(Duration),
    RefreshCredentials,
    RequestLicense,
    Terminal,
}

/// Clasifica fallos y calcula un backoff reproducible, limitado a cinco reintentos.
#[must_use]
pub fn classify_failure(
    job_id: &str,
    failure: TransferFailure,
    retries_used: u8,
    retry_after: Option<Duration>,
    credential_refresh_used: bool,
) -> FailureAction {
    match failure {
        TransferFailure::HttpStatus(401) if !credential_refresh_used => {
            FailureAction::RefreshCredentials
        }
        TransferFailure::HttpStatus(403) => FailureAction::RequestLicense,
        TransferFailure::HttpStatus(408 | 429 | 500..=599)
        | TransferFailure::Timeout
        | TransferFailure::Network => retry_delay(job_id, retries_used, retry_after)
            .map_or(FailureAction::Terminal, FailureAction::RetryAfter),
        _ => FailureAction::Terminal,
    }
}

fn retry_delay(job_id: &str, retries_used: u8, retry_after: Option<Duration>) -> Option<Duration> {
    let base_seconds = *RETRY_BASE_SECONDS.get(usize::from(retries_used))?;
    let server_delay = retry_after
        .unwrap_or_default()
        .min(Duration::from_secs(MAX_RETRY_AFTER_SECONDS));
    let base = Duration::from_secs(base_seconds).max(server_delay);

    let mut hasher = Sha256::new();
    hasher.update(job_id.as_bytes());
    hasher.update([retries_used.saturating_add(1)]);
    let digest = hasher.finalize();
    let jitter_millis =
        u64::from(u16::from_be_bytes([digest[0], digest[1]])) % (RETRY_JITTER_MAX_MILLIS + 1);
    Some(
        base.saturating_add(Duration::from_millis(jitter_millis))
            .min(Duration::from_secs(MAX_RETRY_AFTER_SECONDS)),
    )
}

/// Resultado de verificación local del contenido completo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDigest {
    pub size_bytes: u64,
    pub sha256: String,
    /// Sin hash upstream, el digest local queda DECLARED y no VERIFIED.
    pub trust: Trust,
}

/// Errores de tamaño o de integridad al recibir el artefacto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationError {
    InvalidExpectedHash,
    SizeOverflow,
    ExceedsExpectedSize,
    SizeMismatch,
    HashMismatch,
}

/// Acumula SHA-256 y tamaño durante la escritura de bloques.
pub struct ArtifactVerifier {
    hasher: Sha256,
    bytes_seen: u64,
    expected_size: Option<u64>,
    expected_sha256: Option<String>,
}

impl ArtifactVerifier {
    /// Crea un verificador ligado a los valores del plan.
    pub fn new(
        expected_size: Option<u64>,
        expected_sha256: Option<String>,
    ) -> Result<Self, VerificationError> {
        if expected_sha256
            .as_deref()
            .is_some_and(|expected| !is_sha256(expected))
        {
            return Err(VerificationError::InvalidExpectedHash);
        }
        Ok(Self {
            hasher: Sha256::new(),
            bytes_seen: 0,
            expected_size,
            expected_sha256,
        })
    }

    /// Añade un bloque después de que el llamador haya escrito y sincronizado el temporal.
    pub fn update(&mut self, chunk: &[u8]) -> Result<(), VerificationError> {
        let chunk_len = u64::try_from(chunk.len()).map_err(|_| VerificationError::SizeOverflow)?;
        let next = self
            .bytes_seen
            .checked_add(chunk_len)
            .ok_or(VerificationError::SizeOverflow)?;
        if self.expected_size.is_some_and(|expected| next > expected) {
            return Err(VerificationError::ExceedsExpectedSize);
        }
        self.hasher.update(chunk);
        self.bytes_seen = next;
        Ok(())
    }

    /// Finaliza el hash y exige todos los valores que el plan declaró.
    pub fn finish(self) -> Result<VerifiedDigest, VerificationError> {
        if self
            .expected_size
            .is_some_and(|expected| self.bytes_seen != expected)
        {
            return Err(VerificationError::SizeMismatch);
        }
        let digest = self.hasher.finalize();
        let digest = hex_lower(&digest);
        if self
            .expected_sha256
            .as_ref()
            .is_some_and(|expected| expected != &digest)
        {
            return Err(VerificationError::HashMismatch);
        }
        Ok(VerifiedDigest {
            size_bytes: self.bytes_seen,
            sha256: digest,
            trust: if self.expected_sha256.is_some() {
                Trust::Verified
            } else {
                Trust::Declared
            },
        })
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// Resultado de una petición al límite compartido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandwidthPermit {
    /// Bytes permitidos ahora; nunca supera el saldo agregado.
    pub bytes: u64,
    /// Espera mínima para permitir al menos un byte cuando no hay saldo.
    pub wait: Option<Duration>,
}

struct Bucket {
    limit: Option<NonZeroU64>,
    tokens: u64,
    remainder_nanos: u128,
    last_refill: Instant,
}

/// Token bucket compartido por todas las descargas de LARM.
///
/// La capacidad de la ráfaga es exactamente un segundo del límite. El caller
/// comparte una única instancia entre jobs y solicita bloques no mayores al
/// permiso devuelto.
pub struct BandwidthLimiter {
    bucket: Mutex<Bucket>,
    changed: Notify,
}

impl BandwidthLimiter {
    /// Inicia un limitador con saldo de hasta un segundo.
    #[must_use]
    pub fn new(limit_bytes_per_second: Option<NonZeroU64>) -> Self {
        Self::at(limit_bytes_per_second, Instant::now())
    }

    fn at(limit_bytes_per_second: Option<NonZeroU64>, now: Instant) -> Self {
        Self {
            bucket: Mutex::new(Bucket {
                limit: limit_bytes_per_second,
                tokens: limit_bytes_per_second.map_or(u64::MAX, NonZeroU64::get),
                remainder_nanos: 0,
                last_refill: now,
            }),
            changed: Notify::new(),
        }
    }

    /// Actualiza el límite sin perder el tiempo monotónico transcurrido.
    pub fn set_limit(&self, limit_bytes_per_second: Option<NonZeroU64>) {
        let mut bucket = self
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = bucket.limit;
        bucket.limit = limit_bytes_per_second;
        bucket.tokens = match (previous, limit_bytes_per_second) {
            (_, None) => u64::MAX,
            (None, Some(limit)) => limit.get(),
            (Some(_), Some(limit)) => bucket.tokens.min(limit.get()),
        };
        bucket.remainder_nanos = 0;
        drop(bucket);
        self.changed.notify_waiters();
    }

    /// Solicita permiso de lectura en un instante monotónico.
    #[must_use]
    pub fn acquire(&self, requested_bytes: u64, now: Instant) -> BandwidthPermit {
        let mut bucket = self
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(elapsed) = now.checked_duration_since(bucket.last_refill) {
            if let Some(limit) = bucket.limit {
                refill(&mut bucket, limit, elapsed);
            }
            bucket.last_refill = now;
        }

        let Some(limit) = bucket.limit else {
            return BandwidthPermit {
                bytes: requested_bytes,
                wait: None,
            };
        };

        let granted = requested_bytes.min(bucket.tokens);
        bucket.tokens -= granted;
        let wait = if requested_bytes > 0 && granted == 0 {
            Some(wait_for_one_byte(limit, bucket.remainder_nanos))
        } else {
            None
        };
        BandwidthPermit {
            bytes: granted,
            wait,
        }
    }

    /// Espera a que exista permiso y despierta antes si el límite cambia.
    pub async fn acquire_async(&self, requested_bytes: u64) -> BandwidthPermit {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let permit = self.acquire(requested_bytes, Instant::now());
            if permit.bytes > 0 || requested_bytes == 0 {
                return permit;
            }
            let wait = permit.wait.unwrap_or(Duration::from_millis(1));
            tokio::select! {
                () = &mut notified => {},
                () = tokio::time::sleep(wait) => {},
            }
        }
    }
}

fn refill(bucket: &mut Bucket, limit: NonZeroU64, elapsed: Duration) {
    let numerator = u128::from(limit.get())
        .saturating_mul(elapsed.as_nanos())
        .saturating_add(bucket.remainder_nanos);
    let added = numerator / NANOS_PER_SECOND;
    bucket.remainder_nanos = numerator % NANOS_PER_SECOND;
    bucket.tokens = bucket
        .tokens
        .saturating_add(u64::try_from(added).unwrap_or(u64::MAX))
        .min(limit.get());
    if bucket.tokens == limit.get() {
        bucket.remainder_nanos = 0;
    }
}

fn wait_for_one_byte(limit: NonZeroU64, remainder_nanos: u128) -> Duration {
    let nanos_left = NANOS_PER_SECOND.saturating_sub(remainder_nanos);
    let rate = u128::from(limit.get());
    let wait_nanos = nanos_left.saturating_add(rate - 1) / rate;
    Duration::from_nanos(u64::try_from(wait_nanos).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn candidate(
        job_id: &str,
        source_id: &str,
        priority: DownloadPriority,
        created_at: &str,
    ) -> QueueCandidate {
        QueueCandidate {
            job_id: job_id.into(),
            source_id: source_id.into(),
            created_at: created_at.into(),
            priority,
        }
    }

    #[test]
    fn queue_applies_priority_fifo_limits_and_never_preempts_active_jobs() {
        let active = [candidate("active", "a", DownloadPriority::Background, "0")];
        let queued = [
            candidate("normal-old", "b", DownloadPriority::Normal, "1"),
            candidate("high-later", "b", DownloadPriority::High, "3"),
            candidate("high-first", "c", DownloadPriority::High, "2"),
            candidate("normal-source-capped", "a", DownloadPriority::Normal, "0"),
        ];
        assert_eq!(
            schedule(&queued, &active, 3, 1),
            ["high-first", "high-later"]
        );
        assert_eq!(schedule(&queued, &active, 1, 1), Vec::<String>::new());
        assert_eq!(schedule(&queued, &active, 4, 0), Vec::<String>::new());
    }

    fn resume_context() -> ResumeContext {
        ResumeContext {
            durable_offset: 4,
            expected_size: Some(8),
            expected_sha256: Some("a".repeat(64)),
            validator: Some(ResumeValidator::StrongEtag("\"v1\"".into())),
        }
    }

    fn response(status: u16) -> HttpResponse {
        HttpResponse {
            status,
            content_range: Some(ContentRange {
                start: 4,
                end: 7,
                total: Some(8),
            }),
            etag: Some("\"v1\"".into()),
            last_modified: None,
            content_encoding: Some("identity".into()),
        }
    }

    #[test]
    fn resume_requires_matching_range_etag_and_identity_encoding() {
        let context = resume_context();
        assert_eq!(
            decide_http_response(&context, &response(206), false, false),
            Ok(HttpDecision::Append {
                offset: 4,
                range_end: Some(7),
                total: Some(8),
            })
        );

        let mut changed_etag = response(206);
        changed_etag.etag = Some("\"v2\"".into());
        assert_eq!(
            decide_http_response(&context, &changed_etag, false, false),
            Ok(HttpDecision::RestartFromZero)
        );

        let mut wrong_range = response(206);
        wrong_range.content_range = Some(ContentRange {
            start: 3,
            end: 7,
            total: Some(8),
        });
        assert_eq!(
            decide_http_response(&context, &wrong_range, false, false),
            Err(HttpPolicyError::InvalidContentRange)
        );

        let mut compressed = response(206);
        compressed.content_encoding = Some("gzip".into());
        assert_eq!(
            decide_http_response(&context, &compressed, false, false),
            Err(HttpPolicyError::UnsupportedContentEncoding)
        );
    }

    #[test]
    fn resumed_200_restarts_and_416_needs_size_and_expected_hash() {
        let context = resume_context();
        assert_eq!(
            decide_http_response(&context, &response(200), false, false),
            Ok(HttpDecision::RestartFromZero)
        );
        assert_eq!(
            decide_http_response(&context, &response(416), true, false),
            Err(HttpPolicyError::RangeNotSatisfiable)
        );
        assert_eq!(
            decide_http_response(&context, &response(416), false, true),
            Err(HttpPolicyError::RangeNotSatisfiable)
        );
        let complete_context = ResumeContext {
            durable_offset: 8,
            ..context
        };
        assert_eq!(
            decide_http_response(&complete_context, &response(416), true, true),
            Ok(HttpDecision::AlreadyComplete)
        );
    }

    #[test]
    fn retry_policy_is_classified_bounded_and_deterministic() {
        assert_eq!(
            classify_failure("job-1", TransferFailure::HttpStatus(401), 0, None, false),
            FailureAction::RefreshCredentials
        );
        assert_eq!(
            classify_failure("job-1", TransferFailure::HttpStatus(401), 0, None, true),
            FailureAction::Terminal
        );
        assert_eq!(
            classify_failure("job-1", TransferFailure::HttpStatus(403), 0, None, false),
            FailureAction::RequestLicense
        );
        for status in [400, 404, 418] {
            assert_eq!(
                classify_failure("job-1", TransferFailure::HttpStatus(status), 0, None, false),
                FailureAction::Terminal
            );
        }
        let delay = classify_failure(
            "job-1",
            TransferFailure::HttpStatus(429),
            0,
            Some(Duration::from_secs(90)),
            false,
        );
        assert_eq!(
            delay,
            classify_failure(
                "job-1",
                TransferFailure::HttpStatus(429),
                0,
                Some(Duration::from_secs(90)),
                false
            )
        );
        assert!(
            matches!(delay, FailureAction::RetryAfter(value) if value <= Duration::from_secs(60))
        );
        assert_eq!(
            classify_failure("job-1", TransferFailure::Timeout, 5, None, false),
            FailureAction::Terminal
        );
    }

    #[test]
    fn verifier_checks_size_hash_and_truthfully_marks_local_only_digest() {
        let expected = hex_lower(&Sha256::digest(b"model weights"));
        let mut verified = ArtifactVerifier::new(Some(13), Some(expected.clone())).unwrap();
        verified.update(b"model ").unwrap();
        verified.update(b"weights").unwrap();
        let result = verified.finish().unwrap();
        assert_eq!(result.sha256, expected);
        assert_eq!(result.size_bytes, 13);
        assert_eq!(result.trust, Trust::Verified);

        let mut mismatch = ArtifactVerifier::new(Some(4), Some("0".repeat(64))).unwrap();
        mismatch.update(b"data").unwrap();
        assert_eq!(mismatch.finish(), Err(VerificationError::HashMismatch));

        let mut local_only = ArtifactVerifier::new(None, None).unwrap();
        local_only.update(b"data").unwrap();
        assert_eq!(local_only.finish().unwrap().trust, Trust::Declared);
    }

    #[test]
    fn shared_bandwidth_bucket_caps_burst_and_applies_live_limit_changes() {
        let start = Instant::now();
        let limiter = BandwidthLimiter::at(NonZeroU64::new(10), start);
        assert_eq!(limiter.acquire(7, start).bytes, 7);
        assert_eq!(limiter.acquire(5, start).bytes, 3);
        assert_eq!(
            limiter.acquire(1, start).wait,
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            limiter.acquire(1, start + Duration::from_millis(500)).bytes,
            1
        );

        limiter.set_limit(NonZeroU64::new(2));
        assert_eq!(
            limiter.acquire(5, start + Duration::from_millis(500)).bytes,
            2
        );
        limiter.set_limit(None);
        assert_eq!(
            limiter
                .acquire(100, start + Duration::from_millis(500))
                .bytes,
            100
        );
        limiter.set_limit(NonZeroU64::new(2));
        assert_eq!(
            limiter.acquire(2, start + Duration::from_millis(500)).bytes,
            2
        );
    }

    #[test]
    fn parallel_transfers_consume_one_shared_burst_budget() {
        let start = Instant::now();
        let limiter = std::sync::Arc::new(BandwidthLimiter::at(NonZeroU64::new(128), start));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let workers = (0..2)
            .map(|_| {
                let limiter = limiter.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    limiter.acquire(128, start).bytes
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let granted = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum::<u64>();
        assert_eq!(granted, 128);
    }

    #[test]
    fn refill_uses_fractional_monotonic_time_and_ignores_clock_regression() {
        let start = Instant::now();
        let limiter = BandwidthLimiter::at(NonZeroU64::new(3), start);
        assert_eq!(limiter.acquire(3, start).bytes, 3);
        assert_eq!(
            limiter.acquire(2, start + Duration::from_millis(500)).bytes,
            1
        );
        assert_eq!(
            limiter.acquire(1, start + Duration::from_millis(400)).bytes,
            0
        );
        assert_eq!(limiter.acquire(1, start + Duration::from_secs(1)).bytes, 1);
    }

    #[tokio::test]
    async fn async_acquire_waits_for_tokens_and_live_limit_change_wakes_it() {
        let limiter = std::sync::Arc::new(BandwidthLimiter::new(NonZeroU64::new(1)));
        assert_eq!(limiter.acquire(1, Instant::now()).bytes, 1);
        let waiting = {
            let limiter = limiter.clone();
            tokio::spawn(async move { limiter.acquire_async(10).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        limiter.set_limit(None);
        let result = tokio::time::timeout(Duration::from_millis(100), waiting).await;
        assert!(result.is_ok(), "live update wakes throttled transfer");
        let Ok(joined) = result else {
            return;
        };
        assert!(joined.is_ok(), "throttled transfer task completes");
        let Ok(permit) = joined else {
            return;
        };
        assert_eq!(permit.bytes, 10);
        assert_eq!(permit.wait, None);
    }

    #[test]
    fn resume_validator_rejects_weak_etag_and_malformed_hash() {
        assert!(!ResumeValidator::StrongEtag("W/\"weak\"".into()).is_valid());
        assert!(!ResumeValidator::LastModifiedAndSha256 {
            last_modified: "yesterday".into(),
            sha256: "not-a-hash".into(),
        }
        .is_valid());
    }
}
