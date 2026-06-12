//! Single-file-component (SFC) extractors for Vue, Svelte, and Astro.
//!
//! Each SFC format wraps one or more embedded JS/TS script regions in a
//! template/style envelope. We parse the envelope with its own tree-sitter
//! grammar, locate every script region, slice its source, delegate to the
//! shared JS/TS extractor (`js_shared::extract`), then remap the byte/line
//! offsets back to the full SFC file so the indexer's later
//! `source[start_byte..end_byte]` Merkle re-slice stays exact.
//!
//! TS-family scripts use the TSX grammar (a superset of plain TS that also
//! parses JSX), so JSX inside an SFC script is handled for free: the delegated
//! extractor emits component-usage edges (see `js_shared::is_jsx_component`).

use anyhow::Result;
use tree_sitter::{Language, Node, Parser};

use super::{js_shared, ExtractionResult, Extractor};

/// One embedded script region located in the envelope tree, described purely by
/// offsets into the full SFC source so it doesn't borrow the parse tree.
struct ScriptSpan {
    /// Byte offset in the full SFC source where the script content begins.
    content_start_byte: usize,
    /// Byte offset in the full SFC source where the script content ends.
    content_end_byte: usize,
    /// 0-based row in the full SFC source where the script content begins.
    content_start_row: usize,
    /// `true` → run the TypeScript extractor; `false` → JavaScript.
    is_ts: bool,
}

/// Shift slice-relative coordinates back to the full SFC file. ids/file_path/
/// signature/docstring/target ids are already correct (delegate got the SFC
/// path; text fields are frozen slices), so only offsets and lines move.
fn remap_offsets(result: &mut ExtractionResult, byte_delta: u32, row_delta: u32) {
    for s in &mut result.symbols {
        s.start_byte += byte_delta;
        s.end_byte += byte_delta;
        s.start_line += row_delta;
        s.end_line += row_delta;
    }
    for e in &mut result.edges {
        e.line += row_delta;
    }
}

/// True when a `<script>` start-tag declares a TypeScript `lang`
/// (`ts`/`tsx`/`typescript`). Parses the `lang` attribute's value rather than
/// substring-matching the tag text, so one rule covers Vue's `script_lang` node
/// and Svelte's generic `attribute` without misfiring on `data-lang="ts"` or on
/// whitespace around `=`. Both TS and TSX route to the TSX grammar regardless;
/// the distinction here is only TS-family vs JS.
fn start_tag_is_ts(start_tag_text: &str) -> bool {
    matches!(
        lang_attr_value(start_tag_text).as_deref(),
        Some("ts" | "tsx" | "typescript")
    )
}

/// The lowercased value of the `lang` attribute in a start-tag, if present.
/// Matches `lang` as a whole attribute name (not a `*-lang` suffix), tolerates
/// whitespace around `=`, and accepts single, double, or unquoted values.
fn lang_attr_value(start_tag_text: &str) -> Option<String> {
    let lower = start_tag_text.to_ascii_lowercase();
    let mut rest = lower.as_str();
    loop {
        let at = rest.find("lang")?;
        // `lang` must be a standalone attribute name: preceded by whitespace or
        // `<`/tag boundary, not the tail of `data-lang`/`xml:lang`.
        let preceded_ok = rest[..at]
            .chars()
            .last()
            .map_or(true, |c| c.is_whitespace());
        let after = &rest[at + 4..];
        let after_trimmed = after.trim_start();
        if preceded_ok {
            if let Some(eq) = after_trimmed.strip_prefix('=') {
                let val = eq.trim_start();
                let value = if let Some(v) = val.strip_prefix('"') {
                    v.split('"').next()
                } else if let Some(v) = val.strip_prefix('\'') {
                    v.split('\'').next()
                } else {
                    val.split([' ', '\t', '\n', '>', '/']).next()
                };
                return value.map(str::to_string);
            }
        }
        // Advance past this `lang` occurrence and keep scanning.
        rest = &rest[at + 4..];
    }
}

/// Build a `ScriptSpan` from a `script_element` (its `raw_text` content +
/// `lang` from the `start_tag`). `None` if it has no content node.
fn span_from_script_element(script_element: Node, source: &str) -> Option<ScriptSpan> {
    let content = child_of_kind(script_element, "raw_text")?;
    let tag_text = child_of_kind(script_element, "start_tag")
        .and_then(|t| source.get(t.start_byte()..t.end_byte()))
        .unwrap_or("");
    Some(ScriptSpan {
        content_start_byte: content.start_byte(),
        content_end_byte: content.end_byte(),
        content_start_row: content.start_position().row,
        is_ts: start_tag_is_ts(tag_text),
    })
}

/// First direct child of `node` whose kind is `kind`.
fn child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    node.children(&mut node.walk()).find(|n| n.kind() == kind)
}

/// All `script_element` children of `root` as spans (Vue/Svelte shape).
fn collect_script_elements(root: Node, source: &str) -> Vec<ScriptSpan> {
    let mut spans = Vec::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "script_element" {
            spans.extend(span_from_script_element(child, source));
        }
    }
    spans
}

/// Astro (`document` root): the `---` frontmatter (always TS) plus any
/// client-side `<script>` blocks.
fn find_astro_scripts(root: Node, source: &str) -> Vec<ScriptSpan> {
    let mut spans = Vec::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "frontmatter" => {
                if let Some(block) = child_of_kind(child, "frontmatter_js_block") {
                    spans.push(ScriptSpan {
                        content_start_byte: block.start_byte(),
                        content_end_byte: block.end_byte(),
                        content_start_row: block.start_position().row,
                        is_ts: true,
                    });
                }
            }
            "script_element" => spans.extend(span_from_script_element(child, source)),
            _ => {}
        }
    }
    spans
}

/// Parses the envelope, locates script regions, delegates per region to JS/TS,
/// and remaps offsets. Owns its parsers/queries so they're built once and
/// reused across files (the [`Extractor`] trait takes `&mut self` for this).
struct SfcExtractor {
    envelope: Parser,
    ts_parser: Parser,
    ts_queries: js_shared::JsQueries,
    js_parser: Parser,
    js_queries: js_shared::JsQueries,
    find_scripts: fn(Node, &str) -> Vec<ScriptSpan>,
}

impl SfcExtractor {
    fn new(envelope_lang: Language, find_scripts: fn(Node, &str) -> Vec<ScriptSpan>) -> Self {
        let mut envelope = Parser::new();
        envelope
            .set_language(&envelope_lang)
            .expect("SFC envelope grammar should always load");

        // TSX, not plain TS: SFC scripts may contain JSX (Vue/Astro render
        // functions, TSX-style setup). TSX is a strict superset that parses
        // plain TS too, and its grammar carries the `jsx_*` nodes the
        // component-usage query needs (the non-TSX TS grammar lacks them).
        let ts_lang = Language::new(tree_sitter_typescript::LANGUAGE_TSX);
        let mut ts_parser = Parser::new();
        ts_parser
            .set_language(&ts_lang)
            .expect("TSX grammar should always load");
        let ts_queries = js_shared::JsQueries::new(&ts_lang);

        let js_lang = Language::new(tree_sitter_javascript::LANGUAGE);
        let mut js_parser = Parser::new();
        js_parser
            .set_language(&js_lang)
            .expect("JavaScript grammar should always load");
        let js_queries = js_shared::JsQueries::new(&js_lang);

        Self {
            envelope,
            ts_parser,
            ts_queries,
            js_parser,
            js_queries,
            find_scripts,
        }
    }

    fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
        // Envelope parse failure on junk input degrades to empty, never errors.
        let Some(tree) = self.envelope.parse(source, None) else {
            return Ok(ExtractionResult::default());
        };
        let spans = (self.find_scripts)(tree.root_node(), source);

        let mut out = ExtractionResult::default();
        for span in spans {
            let Some(slice) = source.get(span.content_start_byte..span.content_end_byte) else {
                continue;
            };
            let (parser, queries) = if span.is_ts {
                (&mut self.ts_parser, &self.ts_queries)
            } else {
                (&mut self.js_parser, &self.js_queries)
            };
            let mut sub = js_shared::extract(parser, queries, slice, file_path)?;
            remap_offsets(
                &mut sub,
                span.content_start_byte as u32,
                span.content_start_row as u32,
            );

            // The indexer re-slices `source[start_byte..end_byte]` from the full
            // SFC file for Merkle hashing; a remapped offset that falls out of
            // range (or off a char boundary) would silently hash the wrong/empty
            // bytes there. Drop such a symbol instead, in release as in debug.
            sub.symbols.retain(|s| {
                let in_range = source
                    .get(s.start_byte as usize..s.end_byte as usize)
                    .is_some();
                if !in_range {
                    tracing::warn!(
                        file = file_path,
                        symbol = %s.id,
                        "SFC offset remap out of range; skipping symbol"
                    );
                }
                in_range
            });

            out.symbols.append(&mut sub.symbols);
            out.edges.append(&mut sub.edges);
        }
        Ok(out)
    }
}

macro_rules! sfc_wrapper {
    ($name:ident, $lang:expr, $find:expr, $doc:literal) => {
        #[doc = $doc]
        pub struct $name(SfcExtractor);

        impl $name {
            pub fn new() -> Self {
                Self(SfcExtractor::new($lang, $find))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Extractor for $name {
            fn extract(&mut self, source: &str, file_path: &str) -> Result<ExtractionResult> {
                self.0.extract(source, file_path)
            }
        }
    };
}

// Vue's grammar exports the legacy `language()` (returns a `Language` directly);
// Svelte/Astro export a modern `LANGUAGE: LanguageFn` wrapped via `Language::new`.
// Vue (`component` root) and Svelte (`document` root, incl. `context="module"`)
// share the `<script>`-element shape, so both use `collect_script_elements`.
sfc_wrapper!(
    VueExtractor,
    tree_sitter_vue_updated::language(),
    collect_script_elements,
    "Extractor for Vue single-file components (`.vue`)."
);
sfc_wrapper!(
    SvelteExtractor,
    Language::new(tree_sitter_svelte_ng::LANGUAGE),
    collect_script_elements,
    "Extractor for Svelte components (`.svelte`)."
);
sfc_wrapper!(
    AstroExtractor,
    Language::new(tree_sitter_astro_next::LANGUAGE),
    find_astro_scripts,
    "Extractor for Astro components (`.astro`)."
);

#[cfg(test)]
mod tests {
    use super::*;
    use cartog_core::{EdgeKind, SymbolKind};

    fn body<'a>(result: &ExtractionResult, name: &str, source: &'a str) -> &'a str {
        let s = result
            .symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("symbol {name} not found"));
        &source[s.start_byte as usize..s.end_byte as usize]
    }

    #[test]
    fn vue_extracts_symbol_with_remapped_offsets() {
        let src = "<template><p>hi</p></template>\n<script setup>\nfunction greet() { return 1; }\n</script>\n";
        let result = VueExtractor::new().extract(src, "App.vue").unwrap();
        let greet = result
            .symbols
            .iter()
            .find(|s| s.name == "greet")
            .expect("greet symbol");
        assert_eq!(greet.kind, SymbolKind::Function);
        // Byte offsets must index the FULL file, not the script slice.
        assert_eq!(
            body(&result, "greet", src),
            "function greet() { return 1; }"
        );
        // The function lives on line 3 of the full file.
        assert_eq!(greet.start_line, 3);
    }

    #[test]
    fn vue_edge_line_is_remapped() {
        let src = "<template/>\n<script setup>\nfunction run() { helper(); }\n</script>\n";
        let result = VueExtractor::new().extract(src, "App.vue").unwrap();
        let call = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target_name == "helper")
            .expect("call edge to helper");
        assert_eq!(call.line, 3);
    }

    #[test]
    fn vue_multiple_script_blocks_both_contribute() {
        let src = "<script setup>\nfunction inSetup() {}\n</script>\n<script>\nfunction inPlain() {}\n</script>\n";
        let result = VueExtractor::new().extract(src, "App.vue").unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"inSetup"), "got {names:?}");
        assert!(names.contains(&"inPlain"), "got {names:?}");
        assert_eq!(body(&result, "inSetup", src), "function inSetup() {}");
        assert_eq!(body(&result, "inPlain", src), "function inPlain() {}");
    }

    #[test]
    fn vue_lang_ts_enables_typescript_constructs() {
        let ts = "<script setup lang=\"ts\">\ninterface Props { id: number; }\n</script>\n";
        let result = VueExtractor::new().extract(ts, "App.vue").unwrap();
        assert!(
            result
                .symbols
                .iter()
                .any(|s| s.name == "Props" && s.kind == SymbolKind::Interface),
            "lang=ts should parse a TS interface"
        );

        // Without lang=ts the JS grammar can't parse `interface`, so no symbol.
        let js = "<script setup>\ninterface Props { id: number; }\n</script>\n";
        let result = VueExtractor::new().extract(js, "App.vue").unwrap();
        assert!(
            !result.symbols.iter().any(|s| s.name == "Props"),
            "default JS script should not yield a TS interface"
        );
    }

    #[test]
    fn vue_multibyte_template_keeps_byte_offsets_exact() {
        // Emoji + accented text before the script make byte != char offset.
        let src = "<template><p>héllo 🎉 wörld</p></template>\n<script setup>\nfunction greet() { return 1; }\n</script>\n";
        let result = VueExtractor::new().extract(src, "App.vue").unwrap();
        assert_eq!(
            body(&result, "greet", src),
            "function greet() { return 1; }"
        );
    }

    #[test]
    fn svelte_extracts_and_remaps() {
        let src = "<script lang=\"ts\">\nfunction tick() { advance(); }\n</script>\n<p>x</p>\n";
        let result = SvelteExtractor::new().extract(src, "App.svelte").unwrap();
        assert_eq!(body(&result, "tick", src), "function tick() { advance(); }");
        let edge = result
            .edges
            .iter()
            .find(|e| e.target_name == "advance")
            .expect("call edge");
        assert_eq!(edge.line, 2);
    }

    #[test]
    fn astro_extracts_frontmatter() {
        let src = "---\nimport { db } from './db';\nfunction load() { return query(); }\n---\n<div>x</div>\n";
        let result = AstroExtractor::new().extract(src, "App.astro").unwrap();
        assert_eq!(
            body(&result, "load", src),
            "function load() { return query(); }"
        );
        // Frontmatter starts at line 2 (line 1 is the opening `---`).
        let load = result.symbols.iter().find(|s| s.name == "load").unwrap();
        assert_eq!(load.start_line, 3);
    }

    #[test]
    fn empty_and_junk_degrade_to_empty() {
        for ext in [
            &mut VueExtractor::new() as &mut dyn Extractor,
            &mut SvelteExtractor::new(),
            &mut AstroExtractor::new(),
        ] {
            assert!(ext.extract("", "x.vue").unwrap().symbols.is_empty());
            assert!(ext
                .extract("<<< not real >>>", "x.vue")
                .unwrap()
                .symbols
                .is_empty());
        }
    }

    #[test]
    fn ts_script_with_jsx_keeps_symbols_and_emits_component_edge() {
        // Regression: the SFC TS path used the non-TSX grammar, so JSX in a
        // `lang="ts"` script collapsed the function to an ERROR node (0 symbols).
        let src =
            "<script setup lang=\"ts\">\nfunction render() { return <Counter />; }\n</script>\n";
        let result = VueExtractor::new().extract(src, "App.vue").unwrap();
        assert!(
            result.symbols.iter().any(|s| s.name == "render"),
            "render must survive a JSX-bearing TS script"
        );
        assert!(
            result
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::Calls && e.target_name == "Counter"),
            "JSX usage in a TS script must emit a component edge"
        );
    }

    #[test]
    fn ts_script_still_extracts_typescript_constructs() {
        // TSX is a superset: plain-TS constructs must still parse.
        let src = "<script setup lang=\"ts\">\ninterface Props { id: number; }\n</script>\n";
        let result = VueExtractor::new().extract(src, "App.vue").unwrap();
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "Props" && s.kind == SymbolKind::Interface));
    }

    #[test]
    fn lang_attr_detection_is_robust() {
        // Whitespace around `=` (valid attr syntax) must still detect TS.
        assert!(start_tag_is_ts("<script setup lang = \"ts\">"));
        assert!(start_tag_is_ts("<script lang='typescript'>"));
        assert!(start_tag_is_ts("<script lang=\"tsx\">"));
        // A `*-lang` suffix attribute must NOT be read as the `lang` attribute.
        assert!(!start_tag_is_ts("<script data-lang=\"ts\">"));
        // No lang attribute → JS.
        assert!(!start_tag_is_ts("<script setup>"));
        assert!(!start_tag_is_ts("<script lang=\"js\">"));
    }

    #[test]
    fn lang_tsx_script_parses_typescript() {
        // `lang="tsx"` previously misrouted to the JS grammar, dropping TS types.
        let src = "<script setup lang=\"tsx\">\ninterface P { id: number; }\n</script>\n";
        let result = VueExtractor::new().extract(src, "App.vue").unwrap();
        assert!(result
            .symbols
            .iter()
            .any(|s| s.name == "P" && s.kind == SymbolKind::Interface));
    }
}
