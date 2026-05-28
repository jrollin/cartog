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
/// don't produce `foo_bar_py` with double underscores in some renderers.
///
/// IDs that would start with a digit get a leading `n_` so they remain
/// valid identifiers.
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

/// Escape a string for use inside a Mermaid node label `["..."]`. Double
/// quotes are the only character that must be escaped; Mermaid treats `\"`
/// as a literal quote.
pub fn escape_label(raw: &str) -> String {
    raw.replace('"', "\\\"")
}

/// Render a hierarchy as `graph TD`, one `child --> parent` edge per pair.
/// Empty input yields the bare header so the output is always a valid
/// Mermaid document.
pub fn render_hierarchy(pairs: &[(String, String)]) -> String {
    let mut out = String::from("graph TD\n");
    if pairs.is_empty() {
        return out;
    }
    for (child, parent) in pairs {
        out.push_str(&format!(
            "    {}[\"{}\"] --> {}[\"{}\"]\n",
            sanitize_id(child),
            escape_label(child),
            sanitize_id(parent),
            escape_label(parent),
        ));
    }
    out
}

/// Render file-level imports as `graph LR`. The file is the root; each
/// imported symbol is a target node. Multiple imports from the same file
/// stay distinct nodes (Mermaid dedupes by ID).
pub fn render_deps(file: &str, targets: &[(String, u32)]) -> String {
    let mut out = String::from("graph LR\n");
    let file_id = sanitize_id(file);
    out.push_str(&format!("    {file_id}[\"{}\"]\n", escape_label(file)));
    for (target, line) in targets {
        out.push_str(&format!(
            "    {file_id} --> {}[\"{} (L{line})\"]\n",
            sanitize_id(target),
            escape_label(target),
        ));
    }
    out
}

/// Render a flat file list as a Mermaid `graph TD` rooted at "Repo".
/// Useful for `cartog map --mermaid`. Top symbols (per-file) are rendered
/// as leaf nodes under each file when supplied.
///
/// `files`: ordered file paths.
/// `symbols_by_file`: optional per-file `(name, kind)` leaf annotations.
///   Empty / missing entries are skipped.
pub fn render_map(files: &[String], symbols_by_file: &[(String, Vec<(String, String)>)]) -> String {
    let mut out = String::from("graph TD\n    repo[\"Repo\"]\n");
    for file in files {
        let fid = sanitize_id(file);
        out.push_str(&format!("    repo --> {fid}[\"{}\"]\n", escape_label(file)));
    }
    for (file, syms) in symbols_by_file {
        let fid = sanitize_id(file);
        for (name, kind) in syms {
            // Combine file + symbol into a unique sym ID so the same name in
            // different files doesn't collide.
            let sid = sanitize_id(&format!("{file}::{name}"));
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
    fn escape_label_doubles_internal_quotes() {
        assert_eq!(escape_label(r#"foo"bar"#), r#"foo\"bar"#);
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
    fn render_hierarchy_empty_still_valid_mermaid() {
        let out = render_hierarchy(&[]);
        assert_eq!(out, "graph TD\n");
    }

    #[test]
    fn render_deps_roots_at_file_with_imports_as_leaves() {
        let out = render_deps("auth/service.py", &[("validate_token".into(), 5)]);
        assert!(out.starts_with("graph LR\n"));
        assert!(out.contains("auth_service_py[\"auth/service.py\"]"));
        assert!(out.contains("validate_token[\"validate_token (L5)\"]"));
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
        assert!(out.contains("repo --> src_auth_py[\"src/auth.py\"]"));
        // Symbol leaf uses a `file::name` sanitized ID to avoid collisions.
        assert!(out.contains("[\"login (function)\"]"));
    }
}
