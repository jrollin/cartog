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
