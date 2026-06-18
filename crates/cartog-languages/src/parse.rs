//! Parse-safety guards: bound tree-sitter parsing so pathological input degrades
//! instead of overflowing the worker stack or hanging (both abort the index run).

use std::ops::ControlFlow;
use std::time::{Duration, Instant};
use tree_sitter::{Node, ParseOptions, Parser, Tree};

/// Max AST-walker call depth before a recursive extractor bails (one frame/level).
pub(crate) const MAX_TREE_DEPTH: usize = 600;

/// Per-parse wall-clock budget; tree-sitter's GLR parser can recurse/run unbounded
/// on adversarial input (a stack-overflow abort, not a catchable error). Normal
/// files parse in well under this; it only ever trips on pathological input.
pub(crate) const MAX_PARSE_TIME: Duration = Duration::from_secs(1);

/// RAII call-depth limiter for the manual AST walkers; error-recovery trees can be
/// walked far deeper than their node depth, so a node-depth pre-check is not enough.
pub(crate) struct RecursionGuard;

thread_local! {
    static WALK_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl RecursionGuard {
    /// Enter one recursion level, or `None` past [`MAX_TREE_DEPTH`] (caller bails).
    pub(crate) fn enter() -> Option<Self> {
        WALK_DEPTH.with(|d| {
            if d.get() >= MAX_TREE_DEPTH {
                None
            } else {
                d.set(d.get() + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for RecursionGuard {
    fn drop(&mut self) {
        WALK_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// Bail out of a recursive AST walker once the call depth hits [`MAX_TREE_DEPTH`].
/// Bind at the top of every self-recursive walker; the held guard decrements on
/// return. `guard_recursion!()` returns `()`; `guard_recursion!(expr)` returns `expr`.
macro_rules! guard_recursion {
    () => {
        let _depth_guard = match $crate::parse::RecursionGuard::enter() {
            Some(g) => g,
            None => return,
        };
    };
    ($ret:expr) => {
        let _depth_guard = match $crate::parse::RecursionGuard::enter() {
            Some(g) => g,
            None => return $ret,
        };
    };
}
pub(crate) use guard_recursion;

/// Iterative depth check (uses a `TreeCursor`, so it can't itself overflow).
pub(crate) fn tree_depth_exceeds(root: Node, limit: usize) -> bool {
    let mut cursor = root.walk();
    let mut depth = 0usize;
    loop {
        if depth > limit {
            return true;
        }
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return false;
            }
            depth -= 1;
        }
    }
}

/// Parse `source`, aborting past [`MAX_PARSE_TIME`]. Returns `None` on cancel (the
/// callback fires before the stack-blowing recursion), letting callers degrade.
pub(crate) fn parse_bounded(parser: &mut Parser, source: &str) -> Option<Tree> {
    parse_bounded_with(parser, source, MAX_PARSE_TIME)
}

/// [`parse_bounded`] with an explicit budget (tests pass a short one).
fn parse_bounded_with(parser: &mut Parser, source: &str, budget: Duration) -> Option<Tree> {
    let start = Instant::now();
    let mut on_progress = |_: &tree_sitter::ParseState| {
        if start.elapsed() > budget {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut on_progress);
    let bytes = source.as_bytes();
    parser.parse_with_options(
        &mut |offset, _| bytes.get(offset..).unwrap_or_default(),
        None,
        Some(options),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Language;

    fn kotlin_parser() -> Parser {
        let mut p = Parser::new();
        p.set_language(&Language::new(tree_sitter_kotlin_sg::LANGUAGE))
            .expect("kotlin grammar");
        p
    }

    // Kotlin input that makes tree-sitter's parser recurse/run unbounded (regression
    // for the CI stack overflow in `extractors_never_panic_on_arbitrary_source`).
    const KOTLIN_PARSE_BOMB: &str =
        "(0mf0l:!D4sE^_-Qg9A@y|\"E~=.ztkM!SJeP~@z\\I_\\-Ybf%hrP<1R(iz+&%jw}B`";

    #[test]
    fn cancels_pathological_parse_within_budget() {
        // Unbounded, this input never returns; bounded, it cancels to None.
        let tree = parse_bounded_with(
            &mut kotlin_parser(),
            KOTLIN_PARSE_BOMB,
            Duration::from_millis(50),
        );
        assert!(tree.is_none(), "pathological parse must be cancelled");
    }

    #[test]
    fn parses_normal_source_within_budget() {
        let tree = parse_bounded_with(
            &mut kotlin_parser(),
            "fun greet(name: String) = println(name)",
            MAX_PARSE_TIME,
        );
        assert!(tree.is_some_and(|t| !t.root_node().has_error()));
    }

    // TSX input whose error-recovery tree the walker descends far past its node depth.
    const TSX_RECURSION_BOMB: &str = "'T|3FG!knfh}r&}d+WxkCizKB(m>t*-]`)p}Sm7E\\KfqtxyB:o6\\1pX[Z8tt9YF'l(u+-X'-)u^q$m=]S*5JZG7e8n.qd7FNgoD@L@B\\Ds,2}pOyr\\N>UUj[5.o|^rRG|y_8p!T Um{kcy~)'2l279G],Z_$FaS}n9D'2a;:Y&\\t<mwE9zV%Ltt~\"`49I`\"mYGH0ppC.tfrfQYy1vFK>n{jrw6dr?kZOC$i`06b6Y+^u?<`+\\|9WB+S6Z?Pn%9XW4;gxi]E}e=5;x:]SnD_XTF[v/@(4%V3k|.dX)[{GWbm>.~Zy~bvR>iS0.ZKaxg,fQ%tm5>*vD:-f!=/";

    #[test]
    fn walker_degrades_on_small_stack_instead_of_overflowing() {
        // 256 KiB reliably overflows the unguarded walk; the guard caps recursion so
        // the worker thread returns and joins cleanly. Regression for the same CI test.
        let result = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut ex = crate::get_extractor("tsx").expect("tsx extractor");
                ex.extract(TSX_RECURSION_BOMB, "bomb.tsx")
                    .map(|r| r.symbols)
            })
            .expect("spawn")
            .join()
            .expect("walker must not overflow the worker stack");
        assert!(result.is_ok_and(|s| s.is_empty()));
    }

    #[test]
    fn recursion_guard_caps_depth_and_recovers() {
        let mut guards = Vec::new();
        for _ in 0..MAX_TREE_DEPTH {
            guards.push(RecursionGuard::enter().expect("under the cap"));
        }
        assert!(RecursionGuard::enter().is_none(), "cap reached");
        drop(guards);
        assert!(RecursionGuard::enter().is_some(), "recovers after unwind");
    }

    // Moderately nested VALID code (well under the cap) must still extract — guards
    // the silent-truncation regression if the cap or counter logic ever changes.
    #[test]
    fn legitimately_nested_code_is_not_truncated() {
        let depth = 100;
        let src = format!(
            "fun outer() {{ {}{} }}",
            "if (a) { ".repeat(depth),
            "}".repeat(depth)
        );
        let mut ex = crate::get_extractor("kotlin").expect("kotlin extractor");
        let result = ex.extract(&src, "nested.kt").expect("extracts");
        assert!(
            result.symbols.iter().any(|s| s.name == "outer"),
            "valid {depth}-deep nesting must still yield its symbol"
        );
    }

    // Deeply nested VALID code PAST the cap must degrade (return), never overflow —
    // arch-independent: it exercises the cap directly rather than relying on a stack
    // size that happens to overflow when unguarded.
    #[test]
    fn nesting_past_the_cap_degrades_without_overflowing() {
        let depth = MAX_TREE_DEPTH + 200;
        let src = format!("fun f() {{ {}{} }}", "{ ".repeat(depth), "}".repeat(depth));
        let extracted = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                let mut ex = crate::get_extractor("kotlin").expect("kotlin extractor");
                ex.extract(&src, "deep.kt").is_ok()
            })
            .expect("spawn")
            .join()
            .expect("must not overflow the worker stack");
        assert!(extracted, "over-cap nesting degrades, never aborts");
    }
}
