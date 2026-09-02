//! Inferring a project description from its `README.md`.
//!
//! The fallback source when `.cartog.toml` declares no `[project]
//! description`. Read from disk rather than from the code graph on purpose:
//! the markdown extractor stores heading *structure*, not retrievable prose,
//! and its bodies are truncated and redaction-filtered on the way into the
//! RAG schema. The file is the only place the actual paragraph lives.
//!
//! Everything here is best-effort: a missing, unreadable, or prose-free README
//! is a normal state, never an error.

use std::path::Path;

use crate::model::{Description, DescriptionSource};

/// Hard cap on a stored description, in characters.
///
/// Enforced both here and at write time so one pathological README cannot grow
/// the registry file. The config side caps `[project] description` at the same
/// number (and rejects rather than truncates, since there the user chose the
/// length).
pub const DESCRIPTION_MAX_CHARS: usize = 280;

/// Ceiling on how much of a README is read.
///
/// Bounds the cost of a pathological file (a generated changelog, a vendored
/// blob named `README`). A first paragraph beyond this point yields `None`,
/// which is the same normal "no description" state as an absent README.
const MAX_README_BYTES: usize = 64 * 1024;

/// Candidate README filenames, in priority order. Exact names, never a
/// directory scan: a scan would make the result depend on readdir order.
const CANDIDATES: [&str; 3] = ["README.md", "README.markdown", "README"];

/// The first prose paragraph of `<root>`'s README, as plain text.
///
/// Returns `None` when no README exists, none can be read, or the file holds
/// no prose paragraph (badges and headings only). The text is stripped of
/// markdown inline markup, collapsed to single spaces, and truncated to
/// [`DESCRIPTION_MAX_CHARS`] at a word boundary.
#[must_use]
pub fn readme_description(root: &Path) -> Option<Description> {
    let text = CANDIDATES
        .iter()
        .find_map(|name| read_head(&root.join(name)))?;
    let paragraph = first_prose_paragraph(&text)?;
    let cleaned = strip_inline_markup(&paragraph);
    let capped = truncate_at_word_boundary(&cleaned, DESCRIPTION_MAX_CHARS);
    if capped.is_empty() {
        return None;
    }
    Some(Description {
        text: capped,
        source: DescriptionSource::Readme,
    })
}

/// Read at most [`MAX_README_BYTES`] of `path`, lossily decoded.
///
/// Reads bytes rather than `read_to_string` so a huge or non-UTF-8 file costs
/// one bounded read instead of loading it whole to then reject it.
fn read_head(path: &Path) -> Option<String> {
    use std::io::Read;

    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_README_BYTES as u64)
        .read_to_end(&mut buf)
        .ok()?;
    let text = String::from_utf8_lossy(&buf);
    // A BOM is not content: left in place it hid the leading `#` from
    // `is_heading`, and the title was stored as the description.
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}

/// The first run of consecutive prose lines, joined with single spaces.
///
/// Prose = everything [`is_structural`] rejects. A fenced code block is
/// skipped whole, since its contents are arbitrary text that would otherwise
/// read as prose.
fn first_prose_paragraph(text: &str) -> Option<String> {
    let mut lines = text.lines().peekable();
    let mut paragraph: Vec<&str> = Vec::new();
    // The marker family of the open fence, so a ``` inside a ~~~ block does
    // not close it.
    let mut fence: Option<char> = None;
    let mut html: Option<HtmlBlock> = None;

    while let Some(line) = lines.next() {
        let trimmed = line.trim();

        match (fence, fence_marker(trimmed)) {
            (None, Some(marker)) => {
                fence = Some(marker);
                continue;
            }
            (Some(open), Some(marker)) if open == marker => {
                fence = None;
                continue;
            }
            (Some(_), _) => continue,
            (None, None) => {}
        }

        // An HTML block spans lines, and only its first one starts with `<`:
        // an unfiltered inner line read as prose, so a `<!-- TODO -->` note or
        // a centered logo's alt text became the project's description.
        match html {
            Some(block) => {
                if block.closes_on(trimmed) {
                    html = None;
                }
                continue;
            }
            None => {
                if let Some(block) = HtmlBlock::opened_by(trimmed) {
                    html = Some(block);
                    continue;
                }
            }
        }

        if trimmed.is_empty() || is_structural(trimmed) {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        // A setext underline turns the line above it into a heading, so what
        // looked like prose was a title: discard it and keep looking.
        if is_setext_underline(lines.peek().map(|l| l.trim()).unwrap_or("")) {
            paragraph.clear();
            continue;
        }
        paragraph.push(trimmed);
    }

    if paragraph.is_empty() {
        return None;
    }
    Some(paragraph.join(" "))
}

/// A multi-line HTML region being skipped, per CommonMark's HTML-block rules.
#[derive(Clone, Copy)]
enum HtmlBlock {
    /// Opened by `<!--`, closed by the line containing `-->`.
    Comment,
    /// Opened by any other `<`-leading line, closed by the next blank line.
    Element,
}

impl HtmlBlock {
    /// The block a `<`-leading line opens.
    ///
    /// An `Element` block runs to the next blank line, which is CommonMark's
    /// rule and also why a self-contained `<p>text</p>` needs no special case:
    /// the line after it is blank, so the block ends there either way.
    fn opened_by(trimmed: &str) -> Option<Self> {
        if let Some(rest) = trimmed.strip_prefix("<!--") {
            return (!rest.contains("-->")).then_some(Self::Comment);
        }
        trimmed.starts_with('<').then_some(Self::Element)
    }

    /// True when `trimmed` is the block's last line.
    fn closes_on(self, trimmed: &str) -> bool {
        match self {
            Self::Comment => trimmed.contains("-->"),
            Self::Element => trimmed.is_empty(),
        }
    }
}

/// The fence character of a code-fence line (``` or ~~~), if any.
fn fence_marker(trimmed: &str) -> Option<char> {
    ['`', '~']
        .into_iter()
        .find(|marker| trimmed.starts_with(&marker.to_string().repeat(3)))
}

/// Lines that are markdown *structure*, not prose.
///
/// Rejecting a badge-only line is what makes the common README shape work: the
/// build/coverage/crates.io row sits above the paragraph a reader actually
/// wants, and an unfiltered "first non-blank line" returns the badges.
fn is_structural(trimmed: &str) -> bool {
    is_heading(trimmed)
        || is_setext_underline(trimmed)
        || trimmed.starts_with('|')
        || trimmed.starts_with('>')
        || is_list_item(trimmed)
        || is_horizontal_rule(trimmed)
        || is_badge_only(trimmed)
}

fn is_heading(trimmed: &str) -> bool {
    trimmed.starts_with('#')
}

/// `===` / `---` under a line of text: a setext heading underline.
fn is_setext_underline(trimmed: &str) -> bool {
    !trimmed.is_empty()
        && (trimmed.chars().all(|c| c == '=') || trimmed.chars().all(|c| c == '-'))
        && trimmed.len() >= 2
}

fn is_list_item(trimmed: &str) -> bool {
    for bullet in ["- ", "* ", "+ "] {
        if trimmed.starts_with(bullet) {
            return true;
        }
    }
    // `1. ` / `12) ` — an ordered list marker.
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return false;
    }
    let rest = &trimmed[digits.len()..];
    rest.starts_with(". ") || rest.starts_with(") ")
}

fn is_horizontal_rule(trimmed: &str) -> bool {
    let stripped: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    stripped.len() >= 3
        && (stripped.chars().all(|c| c == '-')
            || stripped.chars().all(|c| c == '*')
            || stripped.chars().all(|c| c == '_'))
}

/// A line made up only of images and linked images: a badge row.
///
/// Decided by removing every image/link construct and checking nothing but
/// punctuation is left, so a badge row with a stray separator still counts.
fn is_badge_only(trimmed: &str) -> bool {
    if !trimmed.starts_with("![") && !trimmed.starts_with("[![") {
        return false;
    }
    let residue = strip_inline_markup(trimmed);
    residue.chars().all(|c| !c.is_alphanumeric())
}

/// Reduce markdown inline markup to plain text.
///
/// Order matters: images go before links (an image inside a link would
/// otherwise leave its alt text behind), and links before emphasis (a `*` in a
/// URL must not be read as emphasis).
///
/// Emphasis stripping is deliberately conservative about identifiers — a code
/// span is literal, an intraword marker is literal, and a run of two or more
/// underscores is literal, so `__init__` survives while `__bold__` keeps its
/// markers. See [`remove_emphasis`]. `<...>` is only markup when it opens like
/// a real tag, so `< 1 MB` survives; see [`remove_html_tags`].
fn strip_inline_markup(text: &str) -> String {
    let s = remove_images(text);
    let s = unwrap_links(&s);
    let s = remove_html_tags(&s);
    let s = remove_emphasis(&s);
    collapse_whitespace(&s)
}

/// Drop `![alt](url)` and `![alt][ref]` entirely — an image carries no prose.
fn remove_images(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(close) = after.find(']') else {
            // Unterminated: keep the literal so nothing is silently eaten.
            out.push_str(&rest[start..]);
            return out;
        };
        let tail = &after[close + 1..];
        rest = match skip_link_target(tail) {
            Some(remaining) => remaining,
            None => tail,
        };
    }
    out.push_str(rest);
    out
}

/// Rewrite `[text](url)` / `[text][ref]` / `[text]` to `text`.
fn unwrap_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(close) = after.find(']') else {
            out.push_str(&rest[start..]);
            return out;
        };
        out.push_str(&after[..close]);
        let tail = &after[close + 1..];
        rest = skip_link_target(tail).unwrap_or(tail);
    }
    out.push_str(rest);
    out
}

/// Skip a `(...)` or `[...]` link target at the start of `tail`.
///
/// Returns `None` when there is none, so the caller can leave `tail` alone
/// (a shortcut reference link, `[text]`, has no target).
fn skip_link_target(tail: &str) -> Option<&str> {
    for (open, close) in [('(', ')'), ('[', ']')] {
        if let Some(inner) = tail.strip_prefix(open) {
            // An unterminated target consumes nothing rather than the rest of
            // the line.
            let end = inner.find(close)?;
            return Some(&inner[end + 1..]);
        }
    }
    None
}

/// Drop `<...>` spans that open like a real tag: inline HTML carries
/// formatting, not prose.
///
/// A `<` counts as a tag opener only when the next character is an ASCII
/// letter, `/`, or `!` — HTML has no other shape. Treating every `<...>` span
/// as markup ate the middle of `inputs < 1 MB and repos > 10k files`. The
/// documented cost is that an `<path>` placeholder still looks like a tag and
/// is stripped, which beats mangling a sentence.
fn remove_html_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        if !opens_a_tag(after) {
            // Not markup: emit through the `<` and keep scanning after it.
            out.push_str(&rest[..=start]);
            rest = after;
            continue;
        }
        let Some(close) = after.find('>') else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..start]);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// True when `after` (the text following a `<`) starts a tag name, a closing
/// tag, or a declaration/comment.
fn opens_a_tag(after: &str) -> bool {
    matches!(after.chars().next(), Some(c) if c.is_ascii_alphabetic() || c == '/' || c == '!')
}

/// Remove emphasis and code-span markers, keeping the text between them.
///
/// Two rules, in this order:
///
/// 1. A backtick code span is literal. Only its delimiters go; every `*`/`_`
///    inside survives, because a code span is exactly where a README writes
///    `__init__` or `*args`.
/// 2. Outside a span, a run of `*`/`_` is a delimiter unless the characters on
///    both sides of the *whole run* are alphanumeric. Markdown treats such an
///    intraword marker as a literal and so must this: an unconditional filter
///    turned `ffmpeg_utils` into `ffmpegutils` and `2*3` into `23`.
/// 3. A run of two or more underscores is never a delimiter, so a bare
///    `__init__` survives. `__bold__` therefore keeps its markers — a
///    deliberate trade: markdown authors write `**bold**` far more often than
///    `__bold__`, and a mangled identifier is the worse failure here.
fn remove_emphasis(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            i = copy_code_span(&chars, i, &mut out);
            continue;
        }
        if matches!(c, '*' | '_') {
            let run_end = run_end(&chars, i);
            if is_delimiter_run(&chars, i, run_end) {
                i = run_end;
                continue;
            }
            out.extend(&chars[i..run_end]);
            i = run_end;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Copy the code span opening at `start` verbatim (minus its backticks) and
/// return the index just past it.
///
/// An unterminated opener is dropped like a stray marker, matching how the rest
/// of this module degrades rather than echoing half-parsed markup.
fn copy_code_span(chars: &[char], start: usize, out: &mut String) -> usize {
    let open_end = run_end(chars, start);
    let fence = &chars[start..open_end];
    let mut i = open_end;
    while i < chars.len() {
        if chars[i] == '`' {
            let close_end = run_end(chars, i);
            if close_end - i == fence.len() {
                out.extend(&chars[open_end..i]);
                return close_end;
            }
            i = close_end;
            continue;
        }
        i += 1;
    }
    // Unterminated: keep the content, drop the opener.
    out.extend(&chars[open_end..]);
    chars.len()
}

/// Index just past the run of `chars[start]` repeated.
fn run_end(chars: &[char], start: usize) -> usize {
    let c = chars[start];
    let mut end = start;
    while end < chars.len() && chars[end] == c {
        end += 1;
    }
    end
}

/// True when the marker run `chars[start..end]` should be stripped.
///
/// A run is literal when it is intraword (alphanumeric on both outer sides), or
/// when it is two-or-more underscores — see rule 3 on [`remove_emphasis`].
fn is_delimiter_run(chars: &[char], start: usize, end: usize) -> bool {
    if chars[start] == '_' && end - start >= 2 {
        return false;
    }
    !is_intraword_run(chars, start, end)
}

/// True when the marker run `chars[start..end]` has an alphanumeric character
/// on both of its outer sides.
fn is_intraword_run(chars: &[char], start: usize, end: usize) -> bool {
    let before = start
        .checked_sub(1)
        .and_then(|i| chars.get(i))
        .is_some_and(|c| c.is_alphanumeric());
    let after = chars.get(end).is_some_and(|c| c.is_alphanumeric());
    before && after
}

/// Collapse every whitespace run (and any control character) to one space.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        // Control characters are removed outright: they corrupt every
        // single-line rendering surface this value flows into.
        if c.is_control() {
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

/// Truncate to `max` chars at the last word boundary, appending `…`.
///
/// The ellipsis counts toward the cap, so the result is never longer than
/// `max` — the stored value has a hard ceiling, not a ceiling plus one.
pub(crate) fn truncate_at_word_boundary(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    // Reserve one char for the ellipsis.
    let budget = max.saturating_sub(1);
    let head: String = text.chars().take(budget).collect();
    let cut = match head.rfind(' ') {
        Some(idx) => &head[..idx],
        // A single word longer than the budget: hard cut.
        None => head.as_str(),
    };
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project root holding one README.
    fn with_readme(name: &str, body: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join(name), body).unwrap();
        dir
    }

    fn describe(name: &str, body: &str) -> Option<Description> {
        let dir = with_readme(name, body);
        readme_description(dir.path())
    }

    #[test]
    fn the_first_prose_paragraph_wins_over_headings_and_badges() {
        let body = "\
# svc-billing

[![build](https://img.shields.io/badge/build-passing-green)](https://ci.example)
[![crates.io](https://img.shields.io/crates/v/x.svg)](https://crates.io/crates/x)

Invoice generation and payment reconciliation.

More detail nobody asked for.
";
        let d = describe("README.md", body).expect("a description");
        assert_eq!(d.text, "Invoice generation and payment reconciliation.");
        assert_eq!(d.source, DescriptionSource::Readme);
    }

    #[test]
    fn a_readme_of_only_badges_and_headings_has_no_description() {
        let body = "\
# svc-billing

![badge](https://img.shields.io/x.svg)

## Install

### Usage
";
        assert!(describe("README.md", body).is_none());
    }

    #[test]
    fn a_missing_readme_has_no_description() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(readme_description(dir.path()).is_none());
    }

    #[test]
    fn a_readme_without_an_extension_is_found() {
        let d = describe("README", "A plain-named readme.").expect("a description");
        assert_eq!(d.text, "A plain-named readme.");
    }

    #[test]
    fn readme_md_wins_over_a_lower_priority_candidate() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("README"), "The extensionless one.").unwrap();
        std::fs::write(dir.path().join("README.md"), "The markdown one.").unwrap();

        let d = readme_description(dir.path()).unwrap();

        assert_eq!(d.text, "The markdown one.");
    }

    #[test]
    fn a_long_paragraph_is_truncated_at_a_word_boundary() {
        let body = "word ".repeat(200);
        let d = describe("README.md", &body).expect("a description");

        let chars = d.text.chars().count();
        assert!(
            chars <= DESCRIPTION_MAX_CHARS,
            "{chars} chars is over the cap"
        );
        assert!(d.text.ends_with('…'), "a truncated value must be marked");
        assert!(
            !d.text.contains("wor…"),
            "the cut must fall on a word boundary: {}",
            d.text
        );
    }

    #[test]
    fn a_paragraph_exactly_at_the_cap_is_not_truncated() {
        let body = "a".repeat(DESCRIPTION_MAX_CHARS);
        let d = describe("README.md", &body).unwrap();
        assert_eq!(d.text.chars().count(), DESCRIPTION_MAX_CHARS);
        assert!(!d.text.ends_with('…'));
    }

    #[test]
    fn one_unbroken_word_over_the_cap_is_hard_cut() {
        let body = "x".repeat(400);
        let d = describe("README.md", &body).unwrap();
        assert_eq!(d.text.chars().count(), DESCRIPTION_MAX_CHARS);
        assert!(d.text.ends_with('…'));
    }

    #[test]
    fn inline_markup_is_stripped_to_plain_text() {
        let body = "A **fast** `code` graph for [agents](https://example.com) and _humans_.";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "A fast code graph for agents and humans.");
    }

    #[test]
    fn a_reference_link_keeps_its_text_and_drops_the_reference() {
        let body = "Built on [tree-sitter][ts] parsers.";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Built on tree-sitter parsers.");
    }

    #[test]
    fn an_inline_image_is_removed_entirely() {
        let body = "Fast ![logo](logo.png) indexing.";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Fast indexing.");
    }

    #[test]
    fn inline_html_tags_are_removed() {
        let body = "A <b>fast</b> indexer.";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "A fast indexer.");
    }

    #[test]
    fn a_paragraph_split_over_two_lines_is_joined_with_one_space() {
        let body = "# Title\n\nInvoice generation\nand payment reconciliation.\n";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Invoice generation and payment reconciliation.");
    }

    #[test]
    fn control_characters_are_removed() {
        let body = "Invoice\u{7}generation\u{0}here.";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Invoicegenerationhere.");
    }

    #[test]
    fn a_fenced_code_block_is_skipped_whole() {
        let body = "# Title\n\n```sh\ncargo install cartog\n```\n\nThe real summary.\n";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "The real summary.");
    }

    #[test]
    fn a_tilde_fenced_code_block_is_skipped_whole() {
        let body = "~~~\nnot prose\n~~~\n\nThe real summary.\n";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "The real summary.");
    }

    #[test]
    fn a_setext_heading_is_not_mistaken_for_prose() {
        let body = "svc-billing\n===========\n\nInvoice generation.\n";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Invoice generation.");
    }

    #[test]
    fn a_list_a_table_and_a_blockquote_are_all_skipped() {
        let body = "\
- a bullet
* another
1. numbered

| col | col |
|-----|-----|

> quoted

The real summary.
";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "The real summary.");
    }

    #[test]
    fn a_leading_html_block_is_skipped() {
        let body = "<p align=\"center\">\n<img src=\"logo.png\">\n</p>\n\nThe real summary.\n";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "The real summary.");
    }

    #[test]
    fn a_horizontal_rule_is_skipped() {
        let body = "***\n\nThe real summary.\n";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "The real summary.");
    }

    #[test]
    fn a_first_paragraph_beyond_the_read_ceiling_is_not_found() {
        // The bounded read is what keeps a pathological README from costing an
        // index pass: past the ceiling there is no description, which is the
        // same normal state as no README at all.
        //
        // Sized against a literal 64 KiB, not against `MAX_README_BYTES` — a
        // fixture derived from the constant moves with it and would pass at any
        // ceiling, asserting nothing.
        let filler = "#\n".repeat(64 * 1024); // structural, never prose
        let body = format!("{filler}\nThe summary nobody will see.\n");
        assert!(body.len() > 64 * 1024);

        assert!(describe("README.md", &body).is_none());
    }

    #[test]
    fn a_paragraph_inside_the_read_ceiling_is_still_found_in_a_huge_file() {
        // The complement: the bound must not defeat a normal README that simply
        // has a long tail.
        let body = format!("# T\n\nThe summary.\n\n{}", "x".repeat(256 * 1024));
        let d = describe("README.md", &body).unwrap();
        assert_eq!(d.text, "The summary.");
    }

    #[test]
    fn a_non_utf8_readme_degrades_instead_of_failing() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("README.md"), b"Caf\xff summary.").unwrap();
        // Lossy decoding: the replacement char survives, the call does not fail.
        let d = readme_description(dir.path()).expect("lossy decoding must still yield prose");
        assert!(d.text.contains("summary"));
    }

    #[test]
    fn a_directory_named_readme_md_yields_no_description() {
        // An I/O error is a normal "no description", never a failure.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("README.md")).unwrap();
        assert!(readme_description(dir.path()).is_none());
    }

    #[test]
    fn comparison_operators_are_not_mistaken_for_html_tags() {
        // A `<` opening a tag is followed by a letter, `/`, or `!` — never a
        // space or a digit. Treating every `<...>` span as markup ate the middle
        // of any sentence comparing two numbers.
        let body = "Fast for inputs < 1 MB and repos > 10k files, on Python >= 3.8 where a < b.";
        let d = describe("README.md", body).unwrap();
        assert_eq!(
            d.text,
            "Fast for inputs < 1 MB and repos > 10k files, on Python >= 3.8 where a < b."
        );
    }

    #[test]
    fn an_angle_bracket_placeholder_is_stripped_like_a_tag() {
        // Documented consequence of the rule above: `<path>` is shaped exactly
        // like a tag, so it goes. Losing a placeholder beats mangling prose.
        let body = "Run cartog index <path> to build the graph.";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Run cartog index to build the graph.");
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_the_first_heading() {
        // A BOM before `# Title` made the heading fail `is_heading`, so the
        // title itself (BOM and `#` included) was stored as the description.
        let dir = tempfile::TempDir::new().unwrap();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"# svc\n\nReal summary.\n");
        std::fs::write(dir.path().join("README.md"), bytes).unwrap();

        let d = readme_description(dir.path()).expect("a description");

        assert_eq!(d.text, "Real summary.");
    }

    #[test]
    fn a_multi_line_html_comment_is_skipped_whole() {
        // Only the opening line started with `<`, so the comment body read as
        // prose and became the project's advertised description.
        let body = "<!--\nTODO rewrite\n-->\n\n# proj\n\nReal summary.\n";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Real summary.");
    }

    #[test]
    fn a_multi_line_html_block_is_skipped_to_the_next_blank_line() {
        let body = "<p align=\"center\">\n  <b>proj</b> logo\n</p>\n\nReal summary.\n";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Real summary.");
    }

    #[test]
    fn an_html_block_swallows_prose_on_its_own_line_until_a_blank_line() {
        // The block, not the individual `<`-leading lines, is what gets
        // skipped: a text-only inner line must go with it.
        let body = "<div>\nnot the description\n</div>\n\nReal summary.\n";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Real summary.");
    }

    #[test]
    fn a_single_line_html_element_is_still_skipped() {
        let body = "<p>ignored</p>\n\nReal summary.\n";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Real summary.");
    }

    #[test]
    fn a_whitespace_only_paragraph_yields_no_description() {
        assert!(describe("README.md", "# T\n\n   \t  \n").is_none());
    }

    #[test]
    fn intraword_underscores_and_asterisks_survive_emphasis_stripping() {
        // Identifiers are the whole point of a code project's README: an
        // unconditional `*`/`_` filter renamed every symbol it touched.
        let body = "Wrapping `ffmpeg_utils` and my_project, where 2*3 and a_b_c appear.";
        let d = describe("README.md", body).unwrap();
        assert_eq!(
            d.text,
            "Wrapping ffmpeg_utils and my_project, where 2*3 and a_b_c appear."
        );
    }

    #[test]
    fn a_bare_dunder_survives_at_the_cost_of_double_underscore_bold() {
        // A bare `__x__` is shaped exactly like `__bold__`, so one of the two
        // has to lose. Deliberate choice: identifiers win, since authors write
        // `**bold**` far more often than `__bold__`.
        let body = "Call __init__, not __strong__.";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Call __init__, not __strong__.");
    }

    #[test]
    fn a_code_span_keeps_every_marker_inside_it() {
        let body = "Call `__init__` and `*args` and `**kwargs`.";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "Call __init__ and *args and **kwargs.");
    }

    #[test]
    fn real_emphasis_delimiters_are_still_stripped() {
        // `__strong__` is excluded on purpose: see the dunder test above.
        let body = "A **bold** and *em* and _em_ summary.";
        let d = describe("README.md", body).unwrap();
        assert_eq!(d.text, "A bold and em and em summary.");
    }
}
