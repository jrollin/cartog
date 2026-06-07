//! Merkle-tree symbol hashing and subtree diffing for incremental re-index.

use super::*;

// ── Merkle-tree hashing ──

/// Compute content_hash and subtree_hash for all symbols in an extraction.
///
/// - content_hash = sha256(kind + name + signature + body_source)
/// - subtree_hash = sha256(content_hash + sorted(children_subtree_hashes))
///
/// Modifies symbols in-place.
pub(crate) fn compute_merkle_hashes(symbols: &mut [Symbol], source: &str) {
    use std::collections::HashMap;

    // Compute content_hash for each symbol
    for sym in symbols.iter_mut() {
        let body = source
            .get(sym.start_byte as usize..sym.end_byte as usize)
            .unwrap_or("");
        let mut hasher = Sha256::new();
        hasher.update(sym.kind.as_str().as_bytes());
        hasher.update(b":");
        hasher.update(sym.name.as_bytes());
        hasher.update(b":");
        if let Some(ref sig) = sym.signature {
            hasher.update(sig.as_bytes());
        }
        hasher.update(b":");
        hasher.update(body.as_bytes());
        sym.content_hash = Some(format!("{:x}", hasher.finalize()));
    }

    // Build parent→children map by index
    let id_to_idx: HashMap<&str, usize> = symbols
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();

    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();

    for (i, sym) in symbols.iter().enumerate() {
        if let Some(ref pid) = sym.parent_id {
            if let Some(&parent_idx) = id_to_idx.get(pid.as_str()) {
                children.entry(parent_idx).or_default().push(i);
            } else {
                roots.push(i);
            }
        } else {
            roots.push(i);
        }
    }

    // Post-order traversal to compute subtree_hash bottom-up
    let mut subtree_hashes: Vec<String> = vec![String::new(); symbols.len()];
    let mut stack: Vec<(usize, bool)> = roots.iter().rev().map(|&i| (i, false)).collect();

    while let Some((idx, visited)) = stack.pop() {
        if visited {
            // All children processed — compute subtree hash
            let mut hasher = Sha256::new();
            hasher.update(
                symbols[idx]
                    .content_hash
                    .as_deref()
                    .unwrap_or("")
                    .as_bytes(),
            );
            if let Some(kids) = children.get(&idx) {
                let mut kid_hashes: Vec<&str> =
                    kids.iter().map(|&k| subtree_hashes[k].as_str()).collect();
                kid_hashes.sort();
                for h in kid_hashes {
                    hasher.update(h.as_bytes());
                }
            }
            subtree_hashes[idx] = format!("{:x}", hasher.finalize());
        } else {
            stack.push((idx, true));
            if let Some(kids) = children.get(&idx) {
                for &kid in kids.iter().rev() {
                    stack.push((kid, false));
                }
            }
        }
    }

    // Store subtree_hash in symbols
    for (i, sym) in symbols.iter_mut().enumerate() {
        sym.subtree_hash = Some(std::mem::take(&mut subtree_hashes[i]));
    }
}

/// Result of diffing new symbols against stored hashes.
#[derive(Debug, Default)]
pub(crate) struct SymbolDiff {
    pub(crate) added: Vec<usize>,            // indices into new symbols
    pub(crate) removed: Vec<String>,         // IDs to delete from DB
    pub(crate) modified: Vec<usize>,         // indices into new symbols (content changed)
    pub(crate) children_changed: Vec<usize>, // indices: own content same, child subtree differs
    pub(crate) unchanged: usize,             // count of fully unchanged symbols
}

/// Compare newly extracted symbols against stored hashes for a file.
pub(crate) fn merkle_diff(
    new_symbols: &[Symbol],
    old_hashes: &[(String, Option<String>, Option<String>)],
) -> SymbolDiff {
    use std::collections::{HashMap, HashSet};

    let mut diff = SymbolDiff::default();

    let old_map: HashMap<&str, (&Option<String>, &Option<String>)> = old_hashes
        .iter()
        .map(|(id, ch, sh)| (id.as_str(), (ch, sh)))
        .collect();

    let new_ids: HashSet<&str> = new_symbols.iter().map(|s| s.id.as_str()).collect();

    for (i, sym) in new_symbols.iter().enumerate() {
        if let Some(&(old_ch, old_sh)) = old_map.get(sym.id.as_str()) {
            // Symbol exists in both old and new
            if sym.subtree_hash.as_ref() == old_sh.as_ref()
                && sym.content_hash.as_ref() == old_ch.as_ref()
            {
                diff.unchanged += 1;
            } else if sym.content_hash.as_ref() != old_ch.as_ref() {
                diff.modified.push(i);
            } else {
                // content same, subtree different — a child was added/modified/removed
                diff.children_changed.push(i);
            }
        } else {
            diff.added.push(i);
        }
    }

    for (old_id, _, _) in old_hashes {
        if !new_ids.contains(old_id.as_str()) {
            diff.removed.push(old_id.clone());
        }
    }

    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartog_core::SymbolKind;
    use proptest::prelude::*;

    /// `id` + the two hashes are the only fields `merkle_diff` reads.
    fn sym(id: &str, content: Option<&str>, subtree: Option<&str>) -> Symbol {
        let mut s = Symbol::new("x", SymbolKind::Function, "f.rs", 0, 0, 0, 0, None);
        s.id = id.to_string();
        s.content_hash = content.map(str::to_string);
        s.subtree_hash = subtree.map(str::to_string);
        s
    }

    /// What happens to one symbol id between the old and new extraction.
    #[derive(Debug, Clone)]
    enum Fate {
        Unchanged,
        Modified,        // content_hash differs
        ChildrenChanged, // content same, subtree differs
        Removed,         // present in old only
        Added,           // present in new only
    }

    fn fate() -> impl Strategy<Value = Fate> {
        prop_oneof![
            Just(Fate::Unchanged),
            Just(Fate::Modified),
            Just(Fate::ChildrenChanged),
            Just(Fate::Removed),
            Just(Fate::Added),
        ]
    }

    /// Distinct ids, each with an independent fate. A `btree_map` keeps the
    /// materialized order deterministic so saved regression seeds replay the
    /// same case (a `hash_map` would iterate in per-process order).
    fn scenario() -> impl Strategy<Value = Vec<(String, Fate)>> {
        proptest::collection::btree_map("[a-z]{1,5}", fate(), 0..12)
            .prop_map(|m| m.into_iter().collect())
    }

    proptest! {
        /// Model-based: build old/new inputs from per-id fates, assert the diff
        /// recovers exactly those fates.
        #[test]
        fn merkle_diff_recovers_fates(scn in scenario()) {
            let mut old_hashes = Vec::new();
            let mut new_symbols = Vec::new();

            for (id, f) in &scn {
                match f {
                    Fate::Unchanged => {
                        old_hashes.push((id.clone(), Some("c".into()), Some("s".into())));
                        new_symbols.push(sym(id, Some("c"), Some("s")));
                    }
                    Fate::Modified => {
                        old_hashes.push((id.clone(), Some("c_old".into()), Some("s_old".into())));
                        new_symbols.push(sym(id, Some("c_new"), Some("s_new")));
                    }
                    Fate::ChildrenChanged => {
                        old_hashes.push((id.clone(), Some("c".into()), Some("s_old".into())));
                        new_symbols.push(sym(id, Some("c"), Some("s_new")));
                    }
                    Fate::Removed => {
                        old_hashes.push((id.clone(), Some("c".into()), Some("s".into())));
                    }
                    Fate::Added => {
                        new_symbols.push(sym(id, Some("c"), Some("s")));
                    }
                }
            }

            let diff = merkle_diff(&new_symbols, &old_hashes);

            // Partition: buckets + unchanged account for every new symbol.
            let bucketed = diff.added.len() + diff.modified.len() + diff.children_changed.len();
            prop_assert_eq!(
                bucketed + diff.unchanged,
                new_symbols.len(),
                "buckets + unchanged must account for every new symbol"
            );

            // Index buckets are disjoint sets of valid new-symbol indices.
            let mut all_idx: Vec<usize> = diff.added.iter()
                .chain(&diff.modified)
                .chain(&diff.children_changed)
                .copied()
                .collect();
            let total = all_idx.len();
            all_idx.sort_unstable();
            all_idx.dedup();
            prop_assert_eq!(all_idx.len(), total, "index buckets overlap");
            prop_assert!(all_idx.iter().all(|&i| i < new_symbols.len()), "index out of range");

            // removed = exactly the old ids absent from new ids.
            let new_ids: std::collections::HashSet<&str> =
                new_symbols.iter().map(|s| s.id.as_str()).collect();
            let mut expected_removed: Vec<&String> = scn.iter()
                .filter(|(id, _)| !new_ids.contains(id.as_str()))
                .map(|(id, _)| id)
                .collect();
            expected_removed.sort();
            let mut got_removed = diff.removed.clone();
            got_removed.sort();
            prop_assert_eq!(&got_removed, &expected_removed.into_iter().cloned().collect::<Vec<_>>());

            // Classification: each new symbol's bucket matches its fate.
            let in_added: std::collections::HashSet<usize> = diff.added.iter().copied().collect();
            let in_modified: std::collections::HashSet<usize> = diff.modified.iter().copied().collect();
            let in_children: std::collections::HashSet<usize> = diff.children_changed.iter().copied().collect();
            let fate_of: std::collections::HashMap<&str, &Fate> =
                scn.iter().map(|(id, f)| (id.as_str(), f)).collect();
            for (i, s) in new_symbols.iter().enumerate() {
                match fate_of.get(s.id.as_str()) {
                    Some(Fate::Added) => prop_assert!(in_added.contains(&i)),
                    Some(Fate::Modified) => prop_assert!(in_modified.contains(&i)),
                    Some(Fate::ChildrenChanged) => prop_assert!(in_children.contains(&i)),
                    Some(Fate::Unchanged) => prop_assert!(
                        !in_added.contains(&i) && !in_modified.contains(&i) && !in_children.contains(&i)
                    ),
                    Some(Fate::Removed) | None => {
                        prop_assert!(false, "new symbol {} has no non-removed fate", s.id)
                    }
                }
            }
        }
    }

    /// A symbol spanning the whole `source`, named `name`, optionally parented.
    fn span_sym(id: &str, name: &str, parent_id: Option<&str>, src: &str) -> Symbol {
        let mut s = Symbol::new(
            name,
            SymbolKind::Function,
            "f.rs",
            0,
            0,
            0,
            src.len() as u32,
            None,
        );
        s.id = id.to_string();
        s.parent_id = parent_id.map(str::to_string);
        s
    }

    fn hashes(symbols: &[Symbol]) -> Vec<(Option<String>, Option<String>)> {
        symbols
            .iter()
            .map(|s| (s.content_hash.clone(), s.subtree_hash.clone()))
            .collect()
    }

    proptest! {
        /// compute_merkle_hashes is deterministic: same symbols + source twice
        /// yields identical content and subtree hashes.
        #[test]
        fn merkle_hashes_are_deterministic(src in ".{0,80}", names in proptest::collection::vec("[a-z]{1,4}", 1..6)) {
            let mut a: Vec<Symbol> = names.iter().enumerate()
                .map(|(i, n)| span_sym(&format!("s{i}"), n, None, &src)).collect();
            let mut b = a.clone();
            compute_merkle_hashes(&mut a, &src);
            compute_merkle_hashes(&mut b, &src);
            prop_assert_eq!(hashes(&a), hashes(&b));
        }

        /// subtree_hash is independent of sibling order: children are sorted
        /// before hashing, so reversing a variable-size child set must not change
        /// the parent's hash. Varies the child set (the thing the property is
        /// about), not just the source text.
        #[test]
        fn subtree_hash_ignores_sibling_order(
            src in ".{1,40}",
            child_names in proptest::collection::vec("[a-z]{1,4}", 2..6),
        ) {
            let parent = span_sym("p", "parent", None, &src);
            let children: Vec<Symbol> = child_names.iter().enumerate()
                .map(|(i, n)| span_sym(&format!("c{i}"), n, Some("p"), &src))
                .collect();

            let mut forward = vec![parent.clone()];
            forward.extend(children.iter().cloned());
            let mut reversed = vec![parent];
            reversed.extend(children.into_iter().rev());

            compute_merkle_hashes(&mut forward, &src);
            compute_merkle_hashes(&mut reversed, &src);

            prop_assert_eq!(&forward[0].subtree_hash, &reversed[0].subtree_hash);
        }
    }
}
