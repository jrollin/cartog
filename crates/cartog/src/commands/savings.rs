//! Token-savings report rendering for `cartog stats --savings`.

use super::*;

/// Project label shown above the savings report. Strips the SQLite filename
/// and walks up to the closest meaningful directory (typically the git root)
/// so users see `cartog · my-project · 5 queries` rather than the full path.
///
/// Returns an empty string when nothing recognizable can be extracted (no
/// parent, non-UTF-8 path component). The caller renders a header without a
/// scope segment in that case — see [`render_savings`].
pub(super) fn savings_scope_label(db_path: &Path) -> String {
    db_path
        .parent()
        .and_then(|p| {
            // `.../<project>/.cartog/db.sqlite` → walk past `.cartog` to the
            // project dir. `.../<project>/.cartog.db` → just take the parent.
            if p.file_name().is_some_and(|n| n == ".cartog") {
                p.parent()
            } else {
                Some(p)
            }
        })
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(|n| n.to_string())
        .unwrap_or_default()
}

/// Format a token count as `~1.4k`, `~28.5k`, etc. Matches the CCE-style
/// compact display so the table stays narrow. Counts under 1,000 render as
/// the raw integer.
fn fmt_tokens(n: u64) -> String {
    if n < 1_000 {
        format!("{n}")
    } else if n < 1_000_000 {
        format!("~{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("~{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Render the savings report — header / bar / with-vs-without table / per-tool
/// breakdown / footer. Pulled out of `cmd_stats` so the formatter is unit-
/// testable independent of the DB.
pub(super) fn render_savings(scope: &str, r: &cartog_db::SavingsReport) -> String {
    use cartog_db::{TOKENS_PER_QUERY_CARTOG, TOKENS_PER_QUERY_GREP};

    let mut out = String::new();
    if r.total_queries == 0 {
        out.push_str(
            "No queries logged yet. Run `cartog search`, `cartog refs`, … or \
             point an MCP-aware editor at this index, then re-run.\n",
        );
        return out;
    }

    // Header: `cartog · my-project · N queries`
    if scope.is_empty() {
        out.push_str(&format!("cartog · {} queries\n\n", r.total_queries));
    } else {
        out.push_str(&format!(
            "cartog · {scope} · {} queries\n\n",
            r.total_queries
        ));
    }

    // 10-cell bar with round-to-nearest. `percent_saved` is 0..=100; mapping
    // `(p + 5) / 10` gives [0,4]→0 cells, [5,14]→1 cell, …, [95,100]→10
    // cells, so the bar covers the full range. Previously `(p / 10).min(10)`
    // capped at 9 cells (because `percent_saved` was clamped to 99), making
    // 90% and 99% visually identical.
    let filled = ((r.percent_saved as usize + 5) / 10).min(10);
    let bar: String = "█".repeat(filled) + &"░".repeat(10 - filled);
    out.push_str(&format!("{bar}  ~{}% tokens saved\n\n", r.percent_saved));

    // With/without/saved table. Column widths chosen so the numbers line up
    // for any plausible value: `Without cartog  ~999.9k tokens   (~1,700 / query)`.
    let with = fmt_tokens(r.tokens_used_cartog);
    let without = fmt_tokens(r.tokens_used_grep);
    let saved = fmt_tokens(r.estimated_tokens_saved);
    out.push_str(&format!(
        "Without cartog  {without:>7} tokens   (~{TOKENS_PER_QUERY_GREP} / query)\n"
    ));
    out.push_str(&format!(
        "With cartog     {with:>7} tokens   (~{TOKENS_PER_QUERY_CARTOG} / query)\n"
    ));
    out.push_str("──────────────────────────────────────────────\n");
    out.push_str(&format!(
        "Saved           {saved:>7} tokens   (~{} / query)\n\n",
        r.baseline_delta
    ));

    // Per-tool counts with annotation so the reader knows the numbers are
    // call counts, not per-tool token savings (the multiplier is flat).
    out.push_str("By tool (call counts):\n");
    for (tool, count) in &r.by_tool {
        out.push_str(&format!("  {count:>4}  {tool}\n"));
    }

    if r.by_source.len() > 1 {
        out.push_str("\nBy source:\n");
        for (source, count) in &r.by_source {
            out.push_str(&format!("  {count:>4}  {source}\n"));
        }
    }

    out.push_str(&format!(
        "\nBaseline: ~{TOKENS_PER_QUERY_GREP} tokens for an equivalent grep+read sweep \
         vs cartog's ~{TOKENS_PER_QUERY_CARTOG}.\n\
         Measured across 13 benchmark scenarios (see crates/cartog/benches/queries.rs).\n"
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fmt_tokens_under_thousand_renders_raw_integer() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(1), "1");
        assert_eq!(fmt_tokens(999), "999");
    }

    #[test]
    fn fmt_tokens_thousands_renders_one_decimal_k() {
        assert_eq!(fmt_tokens(1_000), "~1.0k");
        assert_eq!(fmt_tokens(1_420), "~1.4k");
        assert_eq!(fmt_tokens(8_500), "~8.5k");
        assert_eq!(fmt_tokens(999_999), "~1000.0k");
    }

    #[test]
    fn fmt_tokens_millions_renders_one_decimal_m() {
        assert_eq!(fmt_tokens(1_000_000), "~1.0M");
        assert_eq!(fmt_tokens(2_500_000), "~2.5M");
    }

    #[test]
    fn fmt_tokens_handles_u64_max_without_panicking() {
        // Cosmetic — f64 loses precision near u64::MAX, but the call must
        // not overflow or panic. Anchor a substring to catch obvious breakage.
        let s = fmt_tokens(u64::MAX);
        assert!(s.starts_with("~"));
        assert!(s.ends_with("M"));
    }

    #[test]
    fn savings_scope_label_handles_cartog_dir_layout() {
        // The canonical layout: `<project>/.cartog/db.sqlite` — walk past
        // `.cartog` to get the project dir.
        assert_eq!(
            savings_scope_label(Path::new("/home/user/myproject/.cartog/db.sqlite")),
            "myproject"
        );
    }

    #[test]
    fn savings_scope_label_handles_legacy_layout() {
        // The legacy layout: `<project>/.cartog.db` directly under the
        // project root — parent is the project dir.
        assert_eq!(
            savings_scope_label(Path::new("/home/user/myproject/.cartog.db")),
            "myproject"
        );
    }

    #[test]
    fn savings_scope_label_empty_on_unrecognizable_path() {
        // Bare filename with no parent. render_savings drops the segment.
        assert_eq!(savings_scope_label(Path::new("db.sqlite")), "");
        // Root path — no parent above `.cartog`.
        assert_eq!(savings_scope_label(Path::new("/.cartog/db.sqlite")), "");
    }

    #[test]
    fn render_savings_zero_queries_emits_helpful_hint() {
        let report = cartog_db::SavingsReport {
            by_tool: Vec::new(),
            by_source: Vec::new(),
            total_queries: 0,
            tokens_used_cartog: 0,
            tokens_used_grep: 0,
            estimated_tokens_saved: 0,
            percent_saved: 0,
            baseline_delta: cartog_db::TOKENS_SAVED_PER_QUERY,
        };
        let out = render_savings("myproject", &report);
        assert!(out.contains("No queries logged yet"));
        assert!(out.contains("cartog search"));
    }

    #[test]
    fn render_savings_includes_header_bar_and_breakdown() {
        let report = cartog_db::SavingsReport {
            by_tool: vec![("search".to_string(), 3), ("refs".to_string(), 1)],
            by_source: vec![("cli".to_string(), 4)],
            total_queries: 4,
            tokens_used_cartog: 1_120,
            tokens_used_grep: 6_800,
            estimated_tokens_saved: 5_680,
            percent_saved: 83,
            baseline_delta: cartog_db::TOKENS_SAVED_PER_QUERY,
        };
        let out = render_savings("myproject", &report);
        // Header carries scope + query count.
        assert!(out.contains("cartog · myproject · 4 queries"));
        // ~83% → round-to-nearest gives 8 cells filled, 2 empty.
        assert!(out.contains("████████░░"));
        assert!(out.contains("~83% tokens saved"));
        // With / without / saved rows present and formatted.
        assert!(out.contains("Without cartog"));
        assert!(out.contains("With cartog"));
        assert!(out.contains("Saved"));
        // Per-tool block labelled as call counts (no per-tool token figures).
        assert!(out.contains("By tool (call counts):"));
        assert!(out.contains("3  search"));
    }

    #[test]
    fn render_savings_bar_at_99_percent_shows_full_ten_cells() {
        // Regression for F4: previously `99 / 10 = 9` made 90% and 99%
        // indistinguishable. Round-to-nearest fixes this.
        let report = cartog_db::SavingsReport {
            by_tool: vec![("search".into(), 1)],
            by_source: vec![("cli".into(), 1)],
            total_queries: 1,
            tokens_used_cartog: 17,
            tokens_used_grep: 1_700,
            estimated_tokens_saved: 1_683,
            percent_saved: 99,
            baseline_delta: 1_420,
        };
        let out = render_savings("p", &report);
        assert!(
            out.contains("██████████"),
            "99% must render 10 filled cells, got: {out}"
        );
    }

    #[test]
    fn render_savings_empty_scope_drops_label_segment() {
        let report = cartog_db::SavingsReport {
            by_tool: vec![("search".into(), 1)],
            by_source: vec![("cli".into(), 1)],
            total_queries: 1,
            tokens_used_cartog: 280,
            tokens_used_grep: 1_700,
            estimated_tokens_saved: 1_420,
            percent_saved: 83,
            baseline_delta: 1_420,
        };
        let out = render_savings("", &report);
        // Falls back to `cartog · N queries` without the scope segment.
        assert!(out.starts_with("cartog · 1 queries"), "got: {out}");
    }
}
