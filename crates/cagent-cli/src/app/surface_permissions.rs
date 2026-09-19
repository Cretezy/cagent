//! Permission preview geometry and caching.
//!
//! Permission surfaces are assembled by `surfaces`, but their expensive preview
//! layout is deliberately kept here.  This keeps cache invalidation and the
//! terminal viewport mechanics together instead of mixing them into the large
//! surface dispatcher.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum PermissionPreview<'a> {
    Diff(&'a cagent_agent::tools::SemanticDiff),
    Json(&'a serde_json::Value),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PermissionDiffCacheKey {
    request: cagent_agent::protocol::InteractionRequestId,
    width: u16,
}

pub(crate) struct PermissionDiffRenderCache {
    key: PermissionDiffCacheKey,
    layout: DiffPreviewLayout,
}

fn with_cached_permission_diff_layout<R>(
    cache: &RefCell<Option<PermissionDiffRenderCache>>,
    request: cagent_agent::protocol::InteractionRequestId,
    diff: &cagent_agent::tools::SemanticDiff,
    width: u16,
    read: impl FnOnce(&DiffPreviewLayout) -> R,
) -> R {
    let key = PermissionDiffCacheKey { request, width };
    let mut cache = cache.borrow_mut();
    if cache.as_ref().is_none_or(|cached| cached.key != key) {
        *cache = Some(PermissionDiffRenderCache {
            key,
            layout: DiffPreviewLayout::new(diff, width),
        });
    }
    read(
        &cache
            .as_ref()
            .expect("permission diff cache was initialized")
            .layout,
    )
}

pub(super) fn permission_preview_line_count(preview: PermissionPreview<'_>, width: u16) -> usize {
    match preview {
        PermissionPreview::Diff(diff) => diff_preview_line_count(diff, width),
        PermissionPreview::Json(arguments) => json_preview_lines(arguments, width).len(),
    }
}

pub(super) fn cached_permission_preview_line_count(
    cache: &RefCell<Option<PermissionDiffRenderCache>>,
    request: cagent_agent::protocol::InteractionRequestId,
    preview: PermissionPreview<'_>,
    width: u16,
) -> usize {
    match preview {
        PermissionPreview::Diff(diff) => {
            with_cached_permission_diff_layout(cache, request, diff, width, |layout| {
                layout.content_rows()
            })
        }
        PermissionPreview::Json(arguments) => json_preview_lines(arguments, width).len(),
    }
}

pub(super) fn render_permission_preview_window(
    preview: PermissionPreview<'_>,
    width: u16,
    scroll: usize,
    viewport_rows: usize,
) -> Vec<Line<'static>> {
    match preview {
        PermissionPreview::Diff(diff) => {
            render_diff_preview_window(diff, width, scroll, viewport_rows)
        }
        PermissionPreview::Json(arguments) => json_preview_lines(arguments, width)
            .into_iter()
            .skip(scroll)
            .take(viewport_rows)
            .collect(),
    }
}

pub(super) fn render_cached_permission_preview_window(
    cache: &RefCell<Option<PermissionDiffRenderCache>>,
    request: cagent_agent::protocol::InteractionRequestId,
    preview: PermissionPreview<'_>,
    width: u16,
    scroll: usize,
    viewport_rows: usize,
) -> Vec<Line<'static>> {
    match preview {
        PermissionPreview::Diff(diff) => {
            with_cached_permission_diff_layout(cache, request, diff, width, |layout| {
                layout.render_window(diff, scroll, viewport_rows)
            })
        }
        PermissionPreview::Json(arguments) => json_preview_lines(arguments, width)
            .into_iter()
            .skip(scroll)
            .take(viewport_rows)
            .collect(),
    }
}

pub(super) fn render_permission_scroll_window<F>(
    state: &ScrollViewState,
    capacity: usize,
    render_window: F,
) -> ScrollViewLayout
where
    F: FnOnce(usize, usize) -> Vec<Line<'static>>,
{
    let visible_rows = ScrollViewMetrics::new(state.content_rows, capacity)
        .visible_range(state.offset)
        .len();
    let mut rows = render_window(state.offset, visible_rows).into_iter();
    ScrollViewWidget::new(capacity).render(state, |_| rows.next().unwrap_or_default())
}
