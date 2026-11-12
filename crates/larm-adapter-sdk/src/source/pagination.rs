use super::{invalid, ModelPage, SourceReply, SourceRequest, SourceSearch};
use larm_domain::adapter_updates::is_sha256;
use larm_domain::error::{ErrorCode, LarmError};
use std::collections::BTreeSet;

/// Estado interno de una paginación de fuente. No es un cursor cliente ni
/// acredita postfiltrado global. Core conserva la sesión detrás de su cursor.
#[derive(Debug, Clone)]
pub struct SourcePagination {
    base: SourceSearch,
    filter_digest: String,
    next_cursor: Option<String>,
    snapshot_at: Option<String>,
    seen_cursors: BTreeSet<String>,
    seen_ids: BTreeSet<String>,
    complete: bool,
}
impl SourcePagination {
    pub fn new(base: SourceSearch, full_filter_digest: String) -> Result<Self, LarmError> {
        base.validate()?;
        if !is_sha256(&full_filter_digest) {
            return Err(invalid("filter_digest"));
        }
        let mut seen_cursors = BTreeSet::new();
        if let Some(cursor) = &base.cursor {
            seen_cursors.insert(cursor.clone());
        }
        Ok(Self {
            next_cursor: base.cursor.clone(),
            base,
            filter_digest: full_filter_digest,
            snapshot_at: None,
            seen_cursors,
            seen_ids: BTreeSet::new(),
            complete: false,
        })
    }
    pub fn next_request(
        &self,
        full_filter_digest: &str,
    ) -> Result<Option<SourceSearch>, LarmError> {
        if full_filter_digest != self.filter_digest {
            return Err(invalid("bad_cursor"));
        }
        if self.complete {
            return Ok(None);
        }
        let mut request = self.base.clone();
        request.cursor = self.next_cursor.clone();
        Ok(Some(request))
    }
    /// Sólo avanza después de validar la página completa. Un error no consume
    /// el cursor. Snapshot cambiado o duplicados impiden declarar completitud.
    pub fn accept(&mut self, request: &SourceSearch, page: &ModelPage) -> Result<(), LarmError> {
        if self.next_request(&self.filter_digest)?.as_ref() != Some(request) {
            return Err(invalid("bad_cursor"));
        }
        SourceReply::Search(page.clone()).validate_for(&SourceRequest::Search(request.clone()))?;
        if self
            .snapshot_at
            .as_ref()
            .is_some_and(|s| *s != page.snapshot_at)
        {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "La fuente cambió durante la paginación",
                "source_snapshot_changed",
            )
            .retryable(true));
        }
        if page
            .next_cursor
            .as_ref()
            .is_some_and(|c| self.seen_cursors.contains(c))
        {
            return Err(invalid("source_cursor_repeated"));
        }
        if page
            .items
            .iter()
            .any(|item| self.seen_ids.contains(&item.id))
        {
            return Err(invalid("source_item_repeated"));
        }
        self.seen_ids
            .extend(page.items.iter().map(|item| item.id.clone()));
        if let Some(cursor) = &page.next_cursor {
            self.seen_cursors.insert(cursor.clone());
        }
        self.snapshot_at = Some(page.snapshot_at.clone());
        self.next_cursor = page.next_cursor.clone();
        self.complete = page.next_cursor.is_none();
        Ok(())
    }
}
