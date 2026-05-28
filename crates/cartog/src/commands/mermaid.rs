//! Mermaid diagram renderers for the navigation commands that support the
//! `--mermaid` flag: `hierarchy`, `deps`, and `map`.
//!
//! Output is plain `graph TD` / `graph LR` syntax that pastes into any
//! Mermaid renderer (GitHub markdown, mermaid.live, docs sites). No external
//! crate — Mermaid's surface is small enough that a couple hundred lines of
//! string concatenation is cheaper than a dependency.

/// Mermaid node IDs are restricted to `[A-Za-z0-9_]`. Anything else (dots,
/// slashes, dashes, generics) breaks the parser. Replace each disallowed
/// byte with `_`; collapse runs of underscores so paths like `foo/bar.py`
/// don't produce `foo__bar__py` with double underscores in some renderers.
///
/// IDs that would start with a digit get a leading `n_` so they remain
/// valid identifiers.
///
/// NOTE: this function is intentionally lossy — `foo/bar.py` and `foo_bar.py`
/// both yield `foo_bar_py`. Callers that need stable, collision-free IDs
/// across distinct inputs must prefix with a namespace (e.g. `f_`, `s_`) and
/// hash the full input. See [`render_map`] / [`render_deps`] for the pattern.
pub fn sanitize_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_underscore = false;
    for ch in raw.chars() {
        let safe = ch.is_ascii_alphanumeric() || ch == '_';
        if safe {
            out.push(ch);
            last_underscore = ch == '_';
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    // Trim leading and trailing `_` (cosmetic), and guard the digit-prefix case.
    while out.ends_with('_') {
        out.pop();
    }
    while out.starts_with('_') {
        out.remove(0);
    }
    if out.is_empty() {
        return "n".into();
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert_str(0, "n_");
    }
    out
}

/// Short stable hash suffix used to disambiguate sanitized IDs that would
/// otherwise collide (`foo/bar.py` vs `foo_bar.py`). 8 hex chars of FNV-1a
/// keeps IDs readable and gives 32 bits of collision-resistance — more than
/// enough for a single diagram's worth of nodes. Avoids pulling in a real
/// hash dependency.
fn short_hash(raw: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in raw.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", h as u32 ^ (h >> 32) as u32)
}

/// Build a collision-free node ID by combining a namespace prefix, a
/// sanitized form of the input, and an 8-hex hash suffix derived from the
/// raw input. Two distinct raw strings can collide on `sanitize_id` alone,
/// but adding the hash makes the chance ~1/2^32. The sanitized portion keeps
/// the ID human-readable in the rendered diagram.
fn namespaced_id(prefix: &str, raw: &str) -> String {
    let sane = sanitize_id(raw);
    format!("{prefix}_{sane}_{}", short_hash(raw))
}

/// Escape a string for use inside a Mermaid node label `["..."]`. Mermaid
/// renders labels through DOMPurify by default (`securityLevel: strict`), so
/// `<`, `>`, and `&` must be HTML-entity-encoded or they get stripped /
/// reinterpreted as HTML tags. Double quotes also have to be escaped so they
/// don't terminate the label string. Anything else (brackets, pipes, hashes)
/// passes through the parser intact.
pub fn escape_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render a hierarchy as `graph TD`, one `child --> parent` edge per pair.
/// Duplicate pairs (the same class appearing in multiple language fixtures of
/// a polyglot repo, for example) are emitted once.
///
/// Empty input yields the bare header so the output is always a valid
/// Mermaid document; the caller is responsible for surfacing user-facing
/// "no results" messages on stderr.
pub fn render_hierarchy(pairs: &[(String, String)]) -> String {
    let mut out = String::from("graph TD\n");
    if pairs.is_empty() {
        return out;
    }
    let mut seen = std::collections::HashSet::with_capacity(pairs.len());
    for (child, parent) in pairs {
        // Dedup by the actual edge endpoints, not by string equality of the
        // tuple — two pairs of the same names yield the same edge.
        let key = (child.as_str(), parent.as_str());
        if !seen.insert(key) {
            continue;
        }
        // Class hierarchies are name-based and language-agnostic, so a bare
        // sanitize_id is acceptable here (no path component to disambiguate).
        // If two distinct classes ever produced the same sanitized name, they
        // would also collapse in the plain text output — diagram parity.
        let child_id = sanitize_id(child);
        let parent_id = sanitize_id(parent);
        out.push_str(&format!(
            "    {child_id}[\"{}\"] --> {parent_id}[\"{}\"]\n",
            escape_label(child),
            escape_label(parent),
        ));
    }
    out
}

/// Render file-level imports as `graph LR`. The file is the root; each
/// imported symbol is a target node with a `(Lline)` annotation.
///
/// Each emitted target is given a per-edge unique ID (`t_<sanitized>_<hash>`)
/// so multiple imports of the same target name (e.g. two `validate_token`
/// imports from different modules at different lines) render as distinct
/// nodes rather than collapsing into one.
pub fn render_deps(file: &str, targets: &[(String, u32)]) -> String {
    let mut out = String::from("graph LR\n");
    let file_id = namespaced_id("f", file);
    out.push_str(&format!("    {file_id}[\"{}\"]\n", escape_label(file)));
    for (target, line) in targets {
        // Include line in the hash key so two imports of the same name from
        // different lines get distinct IDs.
        let target_id = namespaced_id("t", &format!("{file}::{target}::L{line}"));
        out.push_str(&format!(
            "    {file_id} --> {target_id}[\"{} (L{line})\"]\n",
            escape_label(target),
        ));
    }
    out
}

/// Render a flat file list as a Mermaid `graph TD` rooted at "Repo".
/// Useful for `cartog map --mermaid`. Top symbols (per-file) are rendered
/// as leaf nodes under each file when supplied.
///
/// File and symbol node IDs are namespaced + hashed so distinct paths like
/// `foo/bar.py` and `foo_bar.py` (which sanitize to the same `foo_bar_py`)
/// render as separate nodes.
///
/// `files`: ordered file paths.
/// `symbols_by_file`: optional per-file `(name, kind)` leaf annotations.
///   Empty / missing entries are skipped.
pub fn render_map(files: &[String], symbols_by_file: &[(String, Vec<(String, String)>)]) -> String {
    let mut out = String::from("graph TD\n    repo[\"Repo\"]\n");
    for file in files {
        let fid = namespaced_id("f", file);
        out.push_str(&format!("    repo --> {fid}[\"{}\"]\n", escape_label(file)));
    }
    for (file, syms) in symbols_by_file {
        let fid = namespaced_id("f", file);
        for (name, kind) in syms {
            let sid = namespaced_id("s", &format!("{file}::{name}"));
            out.push_str(&format!(
                "    {fid} --> {sid}[\"{} ({})\"]\n",
                escape_label(name),
                escape_label(kind),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_punctuation_and_collapses_runs() {
        assert_eq!(sanitize_id("foo/bar.py"), "foo_bar_py");
        assert_eq!(sanitize_id("AuthService"), "AuthService");
        assert_eq!(sanitize_id("123abc"), "n_123abc");
        assert_eq!(sanitize_id("--weird--"), "weird");
        assert_eq!(sanitize_id(""), "n");
        assert_eq!(sanitize_id("..."), "n");
    }

    #[test]
    fn namespaced_id_distinguishes_inputs_that_sanitize_to_same_string() {
        // The bug this fixes: foo/bar.py and foo_bar.py sanitize to the same
        // id but represent different files in the file tree.
        let a = namespaced_id("f", "foo/bar.py");
        let b = namespaced_id("f", "foo_bar.py");
        assert_ne!(a, b, "distinct paths must produce distinct namespaced IDs");
        assert!(a.starts_with("f_foo_bar_py_"));
        assert!(b.starts_with("f_foo_bar_py_"));
    }

    #[test]
    fn escape_label_entity_encodes_mermaid_breakers() {
        assert_eq!(escape_label(r#"foo"bar"#), "foo&quot;bar");
        assert_eq!(escape_label("Vec<T>"), "Vec&lt;T&gt;");
        assert_eq!(escape_label("Result<T, E>"), "Result&lt;T, E&gt;");
        assert_eq!(escape_label("A & B"), "A &amp; B");
        // Brackets, pipes, hashes pass through (Mermaid accepts them inside
        // quoted labels).
        assert_eq!(escape_label("Dict[str, int]"), "Dict[str, int]");
        assert_eq!(escape_label("A|B"), "A|B");
    }

    #[test]
    fn render_hierarchy_emits_header_and_edges() {
        let pairs = vec![
            ("AdminService".into(), "AuthService".into()),
            ("AuthService".into(), "BaseService".into()),
        ];
        let out = render_hierarchy(&pairs);
        assert!(out.starts_with("graph TD\n"));
        assert!(out.contains("AdminService[\"AdminService\"] --> AuthService[\"AuthService\"]"));
        assert!(out.contains("AuthService[\"AuthService\"] --> BaseService[\"BaseService\"]"));
    }

    #[test]
    fn render_hierarchy_deduplicates_identical_pairs() {
        // Polyglot fixture: AuthService -> BaseService appears in py, ts, rs.
        let pairs = vec![
            ("AuthService".into(), "BaseService".into()),
            ("AuthService".into(), "BaseService".into()),
            ("AuthService".into(), "BaseService".into()),
        ];
        let out = render_hierarchy(&pairs);
        let edges = out.matches("AuthService[\"AuthService\"]").count();
        assert_eq!(edges, 1, "duplicate pairs must emit one edge: {out}");
    }

    #[test]
    fn render_hierarchy_empty_still_valid_mermaid() {
        let out = render_hierarchy(&[]);
        assert_eq!(out, "graph TD\n");
    }

    #[test]
    fn render_deps_roots_at_file_with_imports_as_leaves() {
        let out = render_deps("auth/service.py", &[("validate_token".into(), 5)]);
        assert!(out.starts_with("graph LR\n"));
        assert!(out.contains("[\"auth/service.py\"]"));
        assert!(out.contains("[\"validate_token (L5)\"]"));
    }

    #[test]
    fn render_deps_preserves_duplicate_target_names_as_distinct_nodes() {
        // Two imports of the same name at different lines: previously they
        // collided on the same Mermaid node ID and the L5 label was lost.
        let out = render_deps(
            "a.py",
            &[("validate_token".into(), 5), ("validate_token".into(), 12)],
        );
        assert!(
            out.contains("[\"validate_token (L5)\"]"),
            "L5 label must survive: {out}"
        );
        assert!(
            out.contains("[\"validate_token (L12)\"]"),
            "L12 label must survive: {out}"
        );
        // Both edges should be present (different target IDs).
        let edge_lines = out.lines().filter(|l| l.contains(" --> ")).count();
        assert_eq!(edge_lines, 2, "two distinct edges expected: {out}");
    }

    #[test]
    fn render_map_roots_at_repo_and_attaches_symbols() {
        let files = vec!["src/auth.py".to_string()];
        let syms = vec![(
            "src/auth.py".to_string(),
            vec![("login".to_string(), "function".to_string())],
        )];
        let out = render_map(&files, &syms);
        assert!(out.starts_with("graph TD\n    repo[\"Repo\"]\n"));
        assert!(out.contains("[\"src/auth.py\"]"));
        assert!(out.contains("[\"login (function)\"]"));
    }

    #[test]
    fn render_map_distinguishes_files_that_sanitize_to_the_same_id() {
        let files = vec!["foo/bar.py".to_string(), "foo_bar.py".to_string()];
        let out = render_map(&files, &[]);
        // Both files must appear with their own labels.
        assert!(out.contains("[\"foo/bar.py\"]"));
        assert!(out.contains("[\"foo_bar.py\"]"));
        // Two distinct edges from repo.
        let edges = out.lines().filter(|l| l.contains("repo --> ")).count();
        assert_eq!(edges, 2, "two distinct file edges expected: {out}");
    }
}
