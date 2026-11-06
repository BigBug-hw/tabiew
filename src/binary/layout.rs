//! Format detection and dispatch.
//!
//! Inspects the file's magic bytes and delegates to the appropriate parser
//! ([`crate::binary::parquet`] or [`crate::binary::lance`]), wrapping the
//! result in a uniform [`FileLayout`] enum that the hex viewer consumes.
//!
//! `FileLayout` exposes accessor methods that abstract over the per-format
//! `Region`/`MetaNode` types, so the UI does not need to know which parser
//! produced the layout.

use std::ops::Range;

use crate::binary::{lance::LanceLayout, parquet::ParquetLayout};

/// Detected file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Parquet,
    Lance,
    Unknown,
}

/// A uniform view over a parsed file's structure, regardless of format.
///
/// Named `FileLayout` (not `Layout`) to avoid colliding with
/// `ratatui::layout::Layout` in UI modules.
#[derive(Debug, Clone)]
pub enum FileLayout {
    Parquet(ParquetLayout),
    Lance(LanceLayout),
    Unknown { error: Option<String> },
}

/// Format-agnostic region descriptor used by the UI. Each format's own
/// `Region` type carries a format-specific `RegionKind` enum; we flatten that
/// into a string label plus a stable color index so the UI can render without
/// knowing the format's kind taxonomy.
#[derive(Debug, Clone)]
pub struct RegionView {
    pub span: Range<u64>,
    pub kind_name: &'static str,
    pub label: String,
}

/// A unified region with a format-agnostic color index. Used by the hex viewer
/// for byte coloring and cursor-panel display.
#[derive(Debug, Clone)]
pub struct UnifiedRegion {
    pub span: Range<u64>,
    pub kind_name: &'static str,
    pub label: String,
    /// Stable color index (0..6) for `theme().graph(idx)`.
    pub color_idx: usize,
}

impl FileLayout {
    pub fn format(&self) -> Format {
        match self {
            FileLayout::Parquet(_) => Format::Parquet,
            FileLayout::Lance(_) => Format::Lance,
            FileLayout::Unknown { .. } => Format::Unknown,
        }
    }

    pub fn is_recognized(&self) -> bool {
        !matches!(self, FileLayout::Unknown { .. })
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            FileLayout::Parquet(p) => p.error.as_deref(),
            FileLayout::Lance(l) => l.error.as_deref(),
            FileLayout::Unknown { error } => error.as_deref(),
        }
    }

    /// Return the parsed regions as a list of format-agnostic views.
    pub fn region_views(&self) -> Vec<RegionView> {
        match self {
            FileLayout::Parquet(p) => p
                .regions
                .iter()
                .map(|r| RegionView {
                    span: r.span.clone(),
                    kind_name: parquet_kind_name(r.kind),
                    label: r.label.clone(),
                })
                .collect(),
            FileLayout::Lance(l) => l
                .regions
                .iter()
                .map(|r| RegionView {
                    span: r.span.clone(),
                    kind_name: lance_kind_name(r.kind),
                    label: r.label.clone(),
                })
                .collect(),
            FileLayout::Unknown { .. } => Vec::new(),
        }
    }

    /// Return the parsed regions as unified regions with stable color indices.
    pub fn unified_regions(&self) -> Vec<UnifiedRegion> {
        match self {
            FileLayout::Parquet(p) => p
                .regions
                .iter()
                .map(|r| UnifiedRegion {
                    span: r.span.clone(),
                    kind_name: parquet_kind_name(r.kind),
                    label: r.label.clone(),
                    color_idx: parquet_color_idx(r.kind, &r.label),
                })
                .collect(),
            FileLayout::Lance(l) => l
                .regions
                .iter()
                .map(|r| UnifiedRegion {
                    span: r.span.clone(),
                    kind_name: lance_kind_name(r.kind),
                    label: r.label.clone(),
                    color_idx: lance_color_idx(r.kind),
                })
                .collect(),
            FileLayout::Unknown { .. } => Vec::new(),
        }
    }

    /// Return a reference to the parsed tree (empty for unknown formats).
    /// Both Parquet and Lance reuse the same `MetaNode` shape.
    pub fn tree(&self) -> &[crate::binary::parquet::MetaNode] {
        match self {
            FileLayout::Parquet(p) => &p.tree,
            FileLayout::Lance(l) => &l.tree,
            FileLayout::Unknown { .. } => &[],
        }
    }
}

fn parquet_kind_name(k: crate::binary::parquet::RegionKind) -> &'static str {
    use crate::binary::parquet::RegionKind::*;
    match k {
        Magic => "magic",
        FooterLength => "footer-length",
        Metadata => "metadata",
        MetaField => "meta-field",
        RowGroup => "row-group",
        ColumnChunk => "column-chunk",
        PageHeader => "page-header",
        PageData => "page-data",
        Unknown => "unknown",
    }
}

fn lance_kind_name(k: crate::binary::lance::RegionKind) -> &'static str {
    use crate::binary::lance::RegionKind::*;
    match k {
        Magic => "magic",
        Footer => "footer",
        ColumnMetadataOffset => "column-metadata-offset",
        GlobalBufferOffset => "global-buffer-offset",
        ColumnMetadata => "column-metadata",
        ColumnMetadataField => "column-metadata-field",
        GlobalBuffer => "global-buffer",
        GlobalBufferField => "global-buffer-field",
        DataPages => "data-pages",
        DataBuffer => "data-buffer",
        Unknown => "unknown",
    }
}

/// Stable color index for Parquet regions. Mirrors the original
/// `hex_viewer::region_style` mapping so Parquet rendering is unchanged.
fn parquet_color_idx(k: crate::binary::parquet::RegionKind, label: &str) -> usize {
    use crate::binary::parquet::RegionKind::*;
    match k {
        Magic => 0,
        FooterLength => 1,
        Metadata => 2,
        MetaField => {
            let pool = [0usize, 1, 3, 4, 5];
            let slot = if label.starts_with("version") {
                0
            } else if label.starts_with("schema") {
                1
            } else if label.starts_with("num_rows") {
                2
            } else if label.starts_with("row_groups") {
                3
            } else if label.starts_with("key_value_metadata") {
                4
            } else {
                0
            };
            pool[slot % pool.len()]
        }
        RowGroup => 3,
        ColumnChunk => 4,
        PageHeader => 5,
        PageData => 0,
        Unknown => 1,
    }
}

/// Stable color index for Lance regions.
fn lance_color_idx(k: crate::binary::lance::RegionKind) -> usize {
    use crate::binary::lance::RegionKind::*;
    match k {
        Magic => 0,
        Footer => 1,
        ColumnMetadataOffset => 2,
        GlobalBufferOffset => 3,
        ColumnMetadata => 4,
        ColumnMetadataField => 5,
        GlobalBuffer => 0,
        GlobalBufferField => 1,
        DataPages => 2,
        DataBuffer => 6,
        Unknown => 3,
    }
}

/// Detect the file format from its bytes and parse it.
///
/// Detection order:
/// - Leading `PAR1` → Parquet (magic at file start).
/// - Trailing `LANC` → Lance v2 (magic at file end).
/// - Otherwise → Unknown.
pub fn dispatch(bytes: &[u8]) -> FileLayout {
    // Parquet: magic at start AND end.
    if bytes.len() >= 4 && &bytes[0..4] == crate::binary::parquet::MAGIC {
        return FileLayout::Parquet(crate::binary::parquet::parse(bytes));
    }
    // Lance v2: magic at the very end.
    if bytes.len() >= 4 {
        let tail_start = bytes.len() - 4;
        if &bytes[tail_start..] == crate::binary::lance::MAGIC {
            return FileLayout::Lance(crate::binary::lance::parse(bytes));
        }
    }
    FileLayout::Unknown {
        error: Some(format!(
            "not a recognized format (first 4 bytes = {:?}, last 4 bytes = {:?})",
            &bytes.get(0..4).unwrap_or(&[]),
            &bytes.get(bytes.len().saturating_sub(4)..).unwrap_or(&[])
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_parquet() {
        // Minimal Parquet-shaped input: PAR1 ... PAR1.
        let mut bytes = vec![0u8; 16];
        bytes[0..4].copy_from_slice(b"PAR1");
        bytes[12..16].copy_from_slice(b"PAR1");
        // footer_len at offset 8 = 0 (no metadata).
        let layout = dispatch(&bytes);
        assert_eq!(layout.format(), Format::Parquet);
        assert!(layout.is_recognized());
    }

    #[test]
    fn dispatches_lance() {
        // Minimal Lance-shaped input: 24-byte footer ending in LANC.
        let mut bytes = vec![0u8; 24];
        bytes[20..24].copy_from_slice(b"LANC");
        let layout = dispatch(&bytes);
        assert_eq!(layout.format(), Format::Lance);
        assert!(layout.is_recognized());
    }

    #[test]
    fn dispatches_unknown() {
        let bytes = b"hello world, not a binary format at all";
        let layout = dispatch(bytes);
        assert_eq!(layout.format(), Format::Unknown);
        assert!(!layout.is_recognized());
        assert!(layout.error().is_some());
    }

    #[test]
    fn dispatches_empty_as_unknown() {
        let bytes: &[u8] = &[];
        let layout = dispatch(bytes);
        assert_eq!(layout.format(), Format::Unknown);
    }

    #[test]
    fn region_views_non_empty_for_parquet() {
        let mut bytes = vec![0u8; 16];
        bytes[0..4].copy_from_slice(b"PAR1");
        bytes[12..16].copy_from_slice(b"PAR1");
        let layout = dispatch(&bytes);
        let views = layout.region_views();
        // Even a minimal Parquet has header + footer magic regions.
        assert!(!views.is_empty());
    }
}
