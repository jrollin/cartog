//! Rule text: the embedded body, cartog's ownership marker, and the per-agent render.

use super::catalogue::RuleFormat;

/// Rule body shared by every agent. `docs/agent-snippet.md` carries the same text
/// (`scripts/check-rule-copies.sh` keeps them equal).
pub const RULE_BODY: &str = include_str!("body.md");

const MARKER_PREFIX: &str = "<!-- Managed by cartog";

const MARKER_LINE: &str = "<!-- Managed by cartog: `cartog ide --rules` updates this file \
and `cartog uninstall` deletes it. Delete this line to keep your own edits. -->";

/// Full rule file for one agent: frontmatter when it needs one, the marker, then the body.
pub fn render(format: &RuleFormat) -> String {
    let mut out = String::with_capacity(RULE_BODY.len() + 256);
    if let Some(header) = format.header {
        out.push_str("---\n");
        out.push_str(header);
        out.push_str("\n---\n");
    }
    out.push_str(MARKER_LINE);
    out.push_str("\n\n");
    // A Windows checkout with `core.autocrlf` embeds a CRLF body.
    out.push_str(&RULE_BODY.replace("\r\n", "\n"));
    out
}

/// Whether a rule file holds cartog's marker line, so cartog may rewrite or delete it.
pub fn is_cartog_owned(content: &str) -> bool {
    content
        .lines()
        .any(|line| line.trim_start().starts_with(MARKER_PREFIX))
}
