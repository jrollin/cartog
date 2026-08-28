//! Salvage a `.cartog.toml` that `deny_unknown_fields` refused over a typo.
//!
//! One misspelled key should cost the user that key, not the whole file. The
//! keys are dropped section by section against each section's own schema, so a
//! stray name is only ever removed from where it was written.

use crate::config::{
    CartogConfig, DatabaseConfig, EmbeddingConfig, IndexConfig, LocalEmbeddingConfig, OllamaConfig,
    OpenAiConfig, RagConfig, RerankerConfig, SecurityConfig,
};

/// Whether a TOML error is serde's `deny_unknown_fields` rejection (a typo)
/// rather than a syntax or type error.
pub(crate) fn is_unknown_field_error(e: &toml::de::Error) -> bool {
    unknown_field_name(e).is_some()
}

/// The key serde refused, extracted from `unknown field \`name\`, expected ...`.
///
/// Serde already names the offending key; taking it from the error is what lets
/// the salvage converge on any number of typos. An earlier version instead
/// removed each candidate and re-probed, which could only ever resolve a single
/// stray key: with two in one section, removing either left the other
/// complaining, so both were restored and the file was rejected after the user
/// had already been told "the rest of the config still applies".
///
/// The format is pinned by `toml_still_reports_unknown_fields_in_the_format_we_parse`
/// so a dependency bump fails loudly instead of silently reverting to whole-file
/// rejection (which would also revoke index consent).
pub(crate) fn unknown_field_name(e: &toml::de::Error) -> Option<String> {
    let msg = e.to_string();
    let rest = msg.split_once("unknown field `")?.1;
    let (name, _) = rest.split_once('`')?;
    Some(name.to_string())
}

/// Whether `section` is refused by schema `S` specifically for an unknown field.
fn section_unknown_field<S: serde::de::DeserializeOwned>(
    section: &toml::value::Table,
) -> Option<String> {
    let rendered = toml::to_string(&toml::Value::Table(section.clone())).ok()?;
    match toml::from_str::<S>(&rendered) {
        Err(ref e) => unknown_field_name(e),
        Ok(_) => None,
    }
}

/// Whether `section` is refused by schema `S` for an unknown field.
pub(crate) fn section_has_unknown_field<S: serde::de::DeserializeOwned>(
    section: &toml::value::Table,
) -> bool {
    section_unknown_field::<S>(section).is_some()
}

/// Drop the keys that schema `S` refuses from `section`, in place.
///
/// Each pass asks the schema which key it refused and removes exactly that one,
/// so N typos take N passes and every one of them converges. The loop is bounded
/// by the section's key count: a schema that names a key not present (or one
/// already removed) would otherwise spin.
///
/// Sub-tables are never candidates. A stray key inside one
/// (`[embedding.openai] base_urll`) must not be resolved by deleting the
/// sub-table: that silently discarded a whole provider block, moving the
/// endpoint from a self-hosted server to the public API. Sub-tables are cleaned
/// against their own schema by `strip_unknown_in_subtable` before this runs.
fn strip_unknown_in_section<S: serde::de::DeserializeOwned>(section: &mut toml::value::Table) {
    for _ in 0..=section.len() {
        let Some(name) = section_unknown_field::<S>(section) else {
            return;
        };
        // A named key that isn't a removable scalar means the schema is refusing
        // something this pass can't fix; stop rather than loop.
        match section.get(&name) {
            Some(v) if !v.is_table() => {
                section.remove(&name);
            }
            _ => return,
        }
    }
}

/// Clean one named sub-table of `section` against its own schema `S`, leaving the
/// sub-table itself in place.
fn strip_unknown_in_subtable<S: serde::de::DeserializeOwned>(
    section: &mut toml::value::Table,
    name: &str,
) {
    if let Some(toml::Value::Table(sub)) = section.get_mut(name) {
        strip_unknown_in_section::<S>(sub);
    }
}

/// Re-parse `text` with unknown keys removed, so one typo doesn't cost the whole
/// config. Returns `None` when the result still won't parse — a real syntax or
/// type error rather than a stray key.
///
/// Works section by section against that section's own schema, so a stray key is
/// only ever dropped from where it was written.
pub(crate) fn reparse_ignoring_unknown_keys(text: &str) -> Option<CartogConfig> {
    let mut raw: toml::value::Table = toml::from_str(text).ok()?;
    // `[remote]` and `[lsp.<lang>]` are exempt: a mistyped key there could
    // redirect where the index is pushed or which process is spawned as a
    // language server, so both stay hard rejections. See
    // `unknown_remote_key_stays_a_hard_rejection` /
    // `read_config_rejects_unknown_lsp_field`.
    if let Some(toml::Value::Table(remote)) = raw.get("remote") {
        if section_has_unknown_field::<crate::config::RemoteConfig>(remote) {
            return None;
        }
    }
    if let Some(toml::Value::Table(lsp)) = raw.get("lsp") {
        for value in lsp.values() {
            if let toml::Value::Table(lang) = value {
                if section_has_unknown_field::<crate::config::LspLangConfig>(lang) {
                    return None;
                }
            }
        }
    }

    for (name, value) in raw.iter_mut() {
        let toml::Value::Table(section) = value else {
            continue;
        };
        strip_section_by_name(name.as_str(), section);
    }

    let rendered = toml::to_string(&raw).ok()?;
    toml::from_str::<CartogConfig>(&rendered).ok()
}

/// Dispatch one top-level section to its schema.
///
/// Every entry of `KNOWN_CONFIG_SECTIONS` must be handled here or be one of the
/// two deliberate exemptions, or a typo in it silently reverts to whole-file
/// rejection while every sibling section salvages. Pinned by
/// `every_known_section_salvages_a_stray_key`.
fn strip_section_by_name(name: &str, section: &mut toml::value::Table) {
    match name {
        "database" => strip_unknown_in_section::<DatabaseConfig>(section),
        "embedding" => {
            // Clean the provider sub-tables first, each against its own
            // schema, so the parent probe never sees them as the problem.
            strip_unknown_in_subtable::<LocalEmbeddingConfig>(section, "local");
            strip_unknown_in_subtable::<OllamaConfig>(section, "ollama");
            strip_unknown_in_subtable::<OpenAiConfig>(section, "openai");
            strip_unknown_in_section::<EmbeddingConfig>(section);
        }
        "reranker" => strip_unknown_in_section::<RerankerConfig>(section),
        "rag" => strip_unknown_in_section::<RagConfig>(section),
        "security" => strip_unknown_in_section::<SecurityConfig>(section),
        "index" => strip_unknown_in_section::<IndexConfig>(section),
        // `remote` / `lsp` rejected above; unknown sections already warned.
        _ => {}
    }
}
