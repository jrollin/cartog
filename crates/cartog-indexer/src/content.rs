//! Symbol content extraction (significant body slice) for RAG embedding.

/// Find the largest byte index <= `index` that is a valid UTF-8 char boundary in `s`.
///
/// Equivalent to the nightly `str::floor_char_boundary`. Walks back at most 3 bytes.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    // UTF-8 continuation bytes have the pattern 10xxxxxx (0x80..0xBF).
    // Walk backwards until we find a byte that is NOT a continuation byte.
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Maximum content length (in bytes) stored per symbol for embedding.
///
/// BERT models have a 512-token limit. Code averages ~1 token per 2-3 chars,
/// so 2048 bytes ≈ 680-1024 tokens, truncated to 512 by the model.
/// This captures the symbol signature + leading body while halving inference time.
const MAX_CONTENT_BYTES: usize = 2048;

/// Minimum content length (in bytes) to bother embedding.
///
/// Symbols shorter than this (e.g. `import os`, `x = 1`) add noise without value.
const MIN_CONTENT_BYTES: usize = 50;

/// Extract the raw source code for a symbol and build a metadata header.
///
/// Returns `(content, header)` where `header` is a brief preamble for embedding context.
/// Returns `None` if: byte offsets are invalid, content is empty/too short,
/// or the symbol is an import (not useful for semantic search).
pub(crate) fn extract_symbol_content(
    source: &str,
    sym: &cartog_core::Symbol,
) -> Option<(String, String)> {
    // Skip imports — they don't contain searchable logic.
    if sym.kind == cartog_core::SymbolKind::Import {
        return None;
    }

    let start = sym.start_byte as usize;
    let end = sym.end_byte as usize;

    if start >= end || end > source.len() {
        return None;
    }

    // Ensure both boundaries fall on valid UTF-8 char boundaries.
    // Tree-sitter should produce valid offsets, but truncation at MAX_CONTENT_BYTES
    // can land mid-character for multi-byte content (e.g. '─' = 3 bytes).
    let safe_start = if source.is_char_boundary(start) {
        start
    } else {
        // Ceil to next char boundary
        let mut s = start;
        while s < source.len() && !source.is_char_boundary(s) {
            s += 1;
        }
        s
    };
    let truncated_end = end.min(safe_start + MAX_CONTENT_BYTES);
    let safe_end = floor_char_boundary(source, truncated_end);

    if safe_start >= safe_end {
        return None;
    }

    let raw = &source[safe_start..safe_end];
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() < MIN_CONTENT_BYTES {
        return None;
    }

    let header = format!(
        "// File: {}\n// Type: {}\n// Name: {}",
        sym.file_path, sym.kind, sym.name
    );

    Some((raw.to_string(), header))
}

/// Like [`extract_symbol_content`] but redacts secrets from the content slice.
///
/// Header is generated metadata (file/type/name); only `content` can carry a
/// secret, so only it is redacted.
pub(crate) fn extract_symbol_content_redacted(
    source: &str,
    sym: &cartog_core::Symbol,
    redact: crate::RedactionConfig,
) -> Option<(String, String)> {
    extract_symbol_content(source, sym)
        .map(|(content, header)| (redact.redact(&content).into_owned(), header))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_floor_char_boundary_ascii() {
        let s = "hello world";
        assert_eq!(floor_char_boundary(s, 5), 5);
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 100), s.len());
    }

    #[test]
    fn test_floor_char_boundary_multibyte() {
        // '─' is U+2500, encoded as 3 bytes: E2 94 80
        let s = "abc─def";
        // a=0, b=1, c=2, ─=3..6, d=6, e=7, f=8
        assert_eq!(floor_char_boundary(s, 3), 3); // start of ─
        assert_eq!(floor_char_boundary(s, 4), 3); // mid ─ → snap back
        assert_eq!(floor_char_boundary(s, 5), 3); // mid ─ → snap back
        assert_eq!(floor_char_boundary(s, 6), 6); // start of 'd'
    }

    #[test]
    fn test_extract_symbol_content_truncates_at_char_boundary() {
        // Build a source string where MAX_CONTENT_BYTES truncation lands mid-char.
        // Fill with ASCII up to MAX_CONTENT_BYTES-1, then add a 3-byte char.
        let padding = "x".repeat(MAX_CONTENT_BYTES - 1);
        let source = format!("{padding}─after");

        let sym = cartog_core::Symbol::new(
            "test_sym",
            cartog_core::SymbolKind::Function,
            "test.rb",
            1,
            100,
            0,
            source.len() as u32,
            None,
        );

        // This should NOT panic despite truncation landing inside '─'
        let result = extract_symbol_content(&source, &sym);
        assert!(result.is_some());
        let (content, _header) = result.unwrap();
        // Content should be truncated before the '─' (snapped to char boundary)
        assert_eq!(content.len(), MAX_CONTENT_BYTES - 1);
        assert!(content.is_char_boundary(content.len()));
    }
}
