//! Markdown document extractor for cartog.
//!
//! Splits Markdown files into sections by headings, producing one [`Symbol`]
//! per section (kind = `Document`). Large sections and files without headings
//! are chunked at paragraph boundaries.

use anyhow::Result;
use cartog_core::{Symbol, SymbolKind};

use crate::ExtractionResult;

/// Maximum chunk size in bytes. Fits within the indexer's `MAX_CONTENT_BYTES`
/// (2048) and gives `compact_embedding_text` (500 bytes) meaningful content.
const MAX_CHUNK_BYTES: usize = 1500;

/// Returns true if the line is a Markdown ATX heading (`# …`, `## …`, etc.).
fn is_heading(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('#') && {
        // Must be 1-6 '#' followed by a space or end of line
        let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
        (1..=6).contains(&hashes)
            && trimmed
                .as_bytes()
                .get(hashes)
                .is_none_or(|&b| b == b' ' || b == b'\t')
    }
}

/// Extract the heading text (without the `#` prefix).
/// Returns the file stem as fallback if the heading text is empty (e.g. bare `#`).
fn heading_text<'a>(line: &'a str, fallback: &'a str) -> &'a str {
    let trimmed = line.trim_start();
    let after_hashes = trimmed.trim_start_matches('#');
    let text = after_hashes.trim();
    if text.is_empty() {
        fallback
    } else {
        text
    }
}

/// A raw section: heading line (if any) + body text, with byte offsets.
struct Section<'a> {
    heading: Option<&'a str>,
    start_byte: usize,
    end_byte: usize,
}

/// Split source into sections at heading boundaries.
fn split_by_headings(source: &str) -> Vec<Section<'_>> {
    let mut sections: Vec<Section<'_>> = Vec::new();
    let mut current_heading: Option<&str> = None;
    let mut section_start: usize = 0;

    for (byte_offset, line) in LineByteOffsets::new(source) {
        if is_heading(line) {
            // Close previous section
            if byte_offset > section_start {
                sections.push(Section {
                    heading: current_heading,
                    start_byte: section_start,
                    end_byte: byte_offset,
                });
            }
            current_heading = Some(line);
            section_start = byte_offset;
        }
    }

    // Close final section
    if section_start < source.len() {
        sections.push(Section {
            heading: current_heading,
            start_byte: section_start,
            end_byte: source.len(),
        });
    }

    sections
}

/// Iterator over lines with their byte offsets in the source.
struct LineByteOffsets<'a> {
    source: &'a str,
    offset: usize,
}

impl<'a> LineByteOffsets<'a> {
    fn new(source: &'a str) -> Self {
        Self { source, offset: 0 }
    }
}

impl<'a> Iterator for LineByteOffsets<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset > self.source.len() {
            return None;
        }
        let remaining = &self.source[self.offset..];
        if remaining.is_empty() {
            self.offset = self.source.len() + 1; // exhaust
            return None;
        }
        let line_end = remaining.find('\n').unwrap_or(remaining.len());
        let line = &remaining[..line_end];
        let current_offset = self.offset;
        self.offset += line_end + 1; // skip past '\n'
        Some((current_offset, line))
    }
}

/// Find paragraph break positions (`\n\n`) in a slice, returning byte offsets
/// (relative to the slice start) pointing just past the second newline.
fn paragraph_breaks(slice: &str) -> Vec<usize> {
    let bytes = slice.as_bytes();
    let mut breaks = Vec::new();
    for i in 1..bytes.len() {
        if bytes[i] == b'\n' && bytes[i - 1] == b'\n' {
            breaks.push(i + 1);
        }
    }
    breaks
}

/// Sub-chunk a byte range at paragraph boundaries (`\n\n`).
///
/// Produces chunks of *at most* `MAX_CHUNK_BYTES`. When the next paragraph
/// break would push the current chunk over the limit, the chunk is emitted
/// at the previous break point.
///
/// Returns a list of `(start_byte, end_byte)` pairs (absolute offsets).
fn chunk_at_paragraphs(source: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let slice = &source[start..end];
    if slice.trim().is_empty() {
        return Vec::new();
    }
    if end - start <= MAX_CHUNK_BYTES {
        return vec![(start, end)];
    }

    let breaks = paragraph_breaks(slice);
    if breaks.is_empty() {
        // No paragraph breaks — return the whole range as a single chunk.
        return vec![(start, end)];
    }

    let mut chunks = Vec::new();
    let mut chunk_start = 0usize; // relative to slice
    let mut last_break = 0usize;

    for &brk in &breaks {
        if brk - chunk_start > MAX_CHUNK_BYTES && last_break > chunk_start {
            // Adding content up to `brk` would exceed the limit.
            // Emit everything up to the previous break.
            chunks.push((start + chunk_start, start + last_break));
            chunk_start = last_break;
        }
        last_break = brk;
    }

    // Emit remaining content
    if chunk_start < slice.len() {
        let remaining = &slice[chunk_start..];
        if !remaining.trim().is_empty() {
            chunks.push((start + chunk_start, end));
        }
    }

    chunks
}

/// Count the number of `\n` in `source[..byte_offset]`.
///
/// Operates on raw bytes, so `byte_offset` may land inside a multi-byte
/// UTF-8 character without panicking. Callers such as `ce.saturating_sub(1)`
/// can produce offsets that are not on a char boundary — for example when
/// a section ends with a zero-width space (U+200B, 3 bytes).
fn line_number_at(source: &str, byte_offset: usize) -> u32 {
    let end = byte_offset.min(source.len());
    source.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as u32
        + 1
}

#[derive(Default)]
pub struct MarkdownExtractor;

impl MarkdownExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl crate::Extractor for MarkdownExtractor {
    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        if source.trim().is_empty() {
            return Ok(ExtractionResult::default());
        }

        let file_stem = std::path::Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(file_path);

        let sections = split_by_headings(source);
        let has_headings = sections.iter().any(|s| s.heading.is_some());

        let mut symbols = Vec::new();

        if has_headings {
            for section in &sections {
                let name = section
                    .heading
                    .map(|h| heading_text(h, file_stem))
                    .unwrap_or("preamble");

                let sig = section.heading.map(|h| h.trim().to_string());

                let chunks = chunk_at_paragraphs(source, section.start_byte, section.end_byte);
                if chunks.len() <= 1 {
                    // Single chunk for this section
                    let content = &source[section.start_byte..section.end_byte];
                    if content.trim().is_empty() {
                        continue;
                    }
                    let start_line = line_number_at(source, section.start_byte);
                    let end_line = line_number_at(source, section.end_byte.saturating_sub(1));
                    let sym = Symbol::new(
                        name,
                        SymbolKind::Document,
                        file_path,
                        start_line,
                        end_line,
                        section.start_byte as u32,
                        section.end_byte as u32,
                        None,
                    )
                    .with_signature(sig);
                    symbols.push(sym);
                } else {
                    // Sub-chunked large section
                    for (i, &(cs, ce)) in chunks.iter().enumerate() {
                        let chunk_name = if i == 0 {
                            name.to_string()
                        } else {
                            format!("{name}_part_{}", i + 1)
                        };
                        let start_line = line_number_at(source, cs);
                        let end_line = line_number_at(source, ce.saturating_sub(1));
                        let sym = Symbol::new(
                            &chunk_name,
                            SymbolKind::Document,
                            file_path,
                            start_line,
                            end_line,
                            cs as u32,
                            ce as u32,
                            None,
                        )
                        .with_signature(sig.clone());
                        symbols.push(sym);
                    }
                }
            }
        } else {
            // No headings — fixed-size chunks at paragraph boundaries
            let chunks = chunk_at_paragraphs(source, 0, source.len());
            for (i, &(cs, ce)) in chunks.iter().enumerate() {
                let chunk_name = if chunks.len() == 1 {
                    file_stem.to_string()
                } else {
                    format!("chunk_{}", i + 1)
                };
                let start_line = line_number_at(source, cs);
                let end_line = line_number_at(source, ce.saturating_sub(1));
                let sym = Symbol::new(
                    &chunk_name,
                    SymbolKind::Document,
                    file_path,
                    start_line,
                    end_line,
                    cs as u32,
                    ce as u32,
                    None,
                );
                symbols.push(sym);
            }
        }

        Ok(ExtractionResult {
            symbols,
            edges: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Extractor;

    #[test]
    fn test_heading_based_splitting() {
        let source =
            "# Title\n\nIntro text.\n\n## Section A\n\nContent A.\n\n## Section B\n\nContent B.\n";
        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(source, "doc.md").unwrap();

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Title", "Section A", "Section B"]);

        for sym in &result.symbols {
            assert_eq!(sym.kind, SymbolKind::Document);
            assert_eq!(sym.file_path, "doc.md");
            // Byte offsets should point to valid source slices
            let content = &source[sym.start_byte as usize..sym.end_byte as usize];
            assert!(!content.trim().is_empty());
        }
    }

    #[test]
    fn test_mixed_heading_levels() {
        let source = "# H1\n\nPara.\n\n### H3\n\nDeep.\n\n## H2\n\nMid.\n";
        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(source, "doc.md").unwrap();

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["H1", "H3", "H2"]);
    }

    #[test]
    fn test_no_headings_fallback() {
        let source = "Just some plain text.\n\nAnother paragraph.\n\nThird paragraph.\n";
        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(source, "notes.md").unwrap();

        assert!(!result.symbols.is_empty());
        // Single small chunk → uses file stem as name
        assert_eq!(result.symbols[0].name, "notes");
        assert_eq!(result.symbols[0].kind, SymbolKind::Document);
    }

    #[test]
    fn test_no_headings_large_file_chunks() {
        // Build a large doc without headings
        let para = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(10);
        let source = format!("{para}\n\n{para}\n\n{para}\n\n{para}\n\n{para}\n\n{para}\n\n{para}\n\n{para}\n\n{para}\n\n{para}\n");
        assert!(source.len() > MAX_CHUNK_BYTES);

        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(&source, "big.md").unwrap();

        assert!(result.symbols.len() > 1, "should produce multiple chunks");
        for sym in &result.symbols {
            assert_eq!(sym.kind, SymbolKind::Document);
            assert!(sym.name.starts_with("chunk_"));
        }
    }

    #[test]
    fn test_empty_file() {
        let mut ext = MarkdownExtractor::new();
        let result = ext.extract("", "empty.md").unwrap();
        assert!(result.symbols.is_empty());

        let result2 = ext.extract("   \n  \n  ", "blank.md").unwrap();
        assert!(result2.symbols.is_empty());
    }

    #[test]
    fn test_headings_only_no_body() {
        let source = "# Title\n## Section\n### Sub\n";
        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(source, "headers.md").unwrap();

        // Each heading is its own section (even without body text)
        assert!(!result.symbols.is_empty());
        for sym in &result.symbols {
            assert_eq!(sym.kind, SymbolKind::Document);
        }
    }

    #[test]
    fn test_byte_offsets_match_source() {
        let source = "# First\n\nHello world.\n\n# Second\n\nGoodbye.\n";
        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(source, "test.md").unwrap();

        assert_eq!(result.symbols.len(), 2);

        let first = &result.symbols[0];
        let first_content = &source[first.start_byte as usize..first.end_byte as usize];
        assert!(first_content.starts_with("# First"));
        assert!(first_content.contains("Hello world."));

        let second = &result.symbols[1];
        let second_content = &source[second.start_byte as usize..second.end_byte as usize];
        assert!(second_content.starts_with("# Second"));
        assert!(second_content.contains("Goodbye."));
    }

    #[test]
    fn test_signature_is_heading_line() {
        let source = "## Authentication\n\nUse JWT tokens.\n";
        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(source, "doc.md").unwrap();

        assert_eq!(
            result.symbols[0].signature.as_deref(),
            Some("## Authentication")
        );
    }

    #[test]
    fn test_preamble_before_first_heading() {
        let source = "Some intro text.\n\n# Title\n\nBody.\n";
        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(source, "doc.md").unwrap();

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["preamble", "Title"]);
    }

    #[test]
    fn test_no_edges() {
        let source = "# Title\n\nContent.\n";
        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(source, "doc.md").unwrap();
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_is_heading() {
        assert!(is_heading("# Title"));
        assert!(is_heading("## Sub"));
        assert!(is_heading("###### Deep"));
        assert!(is_heading("# "));
        assert!(!is_heading("####### Too deep")); // 7 hashes
        assert!(!is_heading("#not a heading")); // no space after #
        assert!(!is_heading("regular text"));
        assert!(!is_heading(""));
    }

    #[test]
    fn test_large_section_sub_chunking() {
        let para = "This is a paragraph with enough text to matter. ".repeat(20);
        let source = format!("# Big Section\n\n{para}\n\n{para}\n\n{para}\n\n{para}\n");
        assert!(source.len() > MAX_CHUNK_BYTES);

        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(&source, "doc.md").unwrap();

        // Should produce multiple symbols for this one section
        assert!(
            result.symbols.len() > 1,
            "large section should be sub-chunked, got {} symbols",
            result.symbols.len()
        );
        assert_eq!(result.symbols[0].name, "Big Section");
        assert!(result.symbols[1].name.contains("part_"));
    }

    #[test]
    fn test_chunks_respect_max_size() {
        // Build content where each paragraph is ~400 bytes, so 4 paragraphs ≈ 1600 bytes.
        // Chunks should split before exceeding MAX_CHUNK_BYTES.
        let para = "A".repeat(400);
        let source = format!("{para}\n\n{para}\n\n{para}\n\n{para}\n\n{para}\n\n{para}\n");
        assert!(source.len() > MAX_CHUNK_BYTES);

        let chunks = chunk_at_paragraphs(&source, 0, source.len());
        assert!(chunks.len() > 1, "should produce multiple chunks");
        for &(cs, ce) in &chunks[..chunks.len() - 1] {
            assert!(
                ce - cs <= MAX_CHUNK_BYTES + 500, // allow one paragraph of overshoot
                "chunk size {} exceeds limit",
                ce - cs
            );
        }
    }

    #[test]
    fn test_empty_heading_uses_fallback() {
        let source = "#\n\nSome content here.\n";
        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(source, "notes.md").unwrap();

        assert_eq!(result.symbols.len(), 1);
        // Bare `#` heading → falls back to file stem
        assert_eq!(result.symbols[0].name, "notes");
    }

    #[test]
    fn test_line_byte_offsets_iterator() {
        let source = "aaa\nbbb\nccc";
        let offsets: Vec<_> = LineByteOffsets::new(source).collect();
        assert_eq!(offsets, vec![(0, "aaa"), (4, "bbb"), (8, "ccc")]);
    }

    #[test]
    fn test_paragraph_breaks() {
        let source = "para1\n\npara2\n\npara3";
        let breaks = paragraph_breaks(source);
        assert_eq!(breaks.len(), 2);
        // Each break points past the second \n
        assert_eq!(&source[breaks[0]..breaks[0] + 5], "para2");
        assert_eq!(&source[breaks[1]..breaks[1] + 5], "para3");
    }

    #[test]
    fn test_line_number_at_offset_inside_multibyte_char() {
        // Layout: 'a' '\n' <ZWSP: E2 80 8B> 'b' — bytes 0..6, ZWSP at 2..5.
        // Offsets 3 and 4 fall inside the ZWSP and must not panic. Callers
        // such as `end_byte.saturating_sub(1)` can produce such offsets.
        let source = "a\n\u{200B}b";
        assert_eq!(source.len(), 6);
        assert_eq!(line_number_at(source, 2), 2);
        assert_eq!(line_number_at(source, 3), 2);
        assert_eq!(line_number_at(source, 4), 2);
        assert_eq!(line_number_at(source, 9999), 2);
    }

    #[test]
    fn test_extract_with_zero_width_space_at_section_end() {
        // Reproduces the Obsidian-vault panic. When the final section ends
        // with a zero-width space (U+200B, 3 bytes), `end_byte.saturating_sub(1)`
        // lands inside the ZWSP. That offset was previously fed to
        // `line_number_at`, which sliced `source[..offset]` and panicked at
        // Rust's char-boundary check.
        let source = "# A\n\nfoo\u{200B}";
        let mut ext = MarkdownExtractor::new();
        let result = ext.extract(source, "note.md").unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["A"]);
    }
}
