//! Tests for the `cartog projects` rendering and JSON shape.
//!
//! The registry's own behaviour is tested in `cartog-registry`; these cover the
//! adapter layer — what a person and an agent actually see.

use cartog_registry::{Description, DescriptionSource, Listing, Markers, ProjectRow};

use super::list::{render, resolution_rate, to_json};

fn row(name: &str) -> ProjectRow {
    ProjectRow {
        id: format!("serve-{name}"),
        db_path: format!("/w/{name}/.cartog/db.sqlite").into(),
        root: format!("/w/{name}").into(),
        name: name.to_string(),
        languages: vec![("rust".to_string(), 412), ("markdown".to_string(), 30)],
        schema_version: Some(8),
        file_count: Some(412),
        symbol_count: Some(8134),
        edge_count: Some(19_022),
        resolved_count: Some(13_505),
        embedding_count: Some(8134),
        embed_provider: Some("local".to_string()),
        embed_model: Some("bge-small".to_string()),
        embed_dim: Some(384),
        last_indexed: Some(1_700_000_000),
        last_seen: 1_700_000_100,
        declared_name: None,
        description: None,
        markers: Markers::default(),
    }
}

#[test]
fn an_unavailable_registry_renders_an_explanation_not_an_empty_list() {
    // "No projects" and "no registry" are different facts; a person reading
    // the former would think their indexes vanished.
    let out = render(&Listing::unavailable());
    assert!(out.contains("No project registry found"));
    assert!(out.contains("CARTOG_REGISTRY"));
}

#[test]
fn an_available_but_empty_registry_says_how_to_add_a_project() {
    let out = render(&Listing {
        projects: vec![],
        available: true,
    });
    assert!(out.contains("No projects registered"));
    assert!(out.contains("cartog index"));
}

#[test]
fn a_project_row_renders_its_name_counts_languages_and_db_path() {
    let out = render(&Listing {
        projects: vec![row("svc-billing")],
        available: true,
    });
    assert!(out.contains("svc-billing"));
    assert!(out.contains("8134"));
    assert!(out.contains("rust"));
    assert!(
        out.contains("/w/svc-billing/.cartog/db.sqlite"),
        "the db_path is the actionable payload and must always be shown"
    );
}

#[test]
fn an_unknown_count_renders_as_a_question_mark_not_as_zero() {
    let mut r = row("bare");
    r.symbol_count = None;
    r.file_count = None;
    let out = render(&Listing {
        projects: vec![r],
        available: true,
    });
    assert!(
        out.contains('?'),
        "an unknown count must not be indistinguishable from an empty project"
    );
    assert!(!out.contains("0 symbols"));
}

#[test]
fn a_never_indexed_project_renders_never_rather_than_a_1970_date() {
    let mut r = row("fresh");
    r.last_indexed = None;
    let out = render(&Listing {
        projects: vec![r],
        available: true,
    });
    assert!(out.contains("never"));
    assert!(!out.contains("1970"));
}

#[test]
fn a_stale_schema_marker_names_the_version_so_it_is_actionable() {
    let mut r = row("old");
    r.schema_version = Some(6);
    r.markers.stale_schema = true;
    let out = render(&Listing {
        projects: vec![r],
        available: true,
    });
    assert!(out.contains("stale-schema v6"));
}

#[test]
fn every_marker_renders_when_set() {
    let mut r = row("all");
    r.markers = Markers {
        live: true,
        stale_schema: true,
        missing: true,
        embed_mismatch: true,
    };
    let out = render(&Listing {
        projects: vec![r],
        available: true,
    });
    for marker in ["live", "stale-schema", "missing", "embed-mismatch"] {
        assert!(out.contains(marker), "missing marker: {marker}");
    }
}

#[test]
fn no_markers_render_when_a_project_is_healthy_and_idle() {
    let out = render(&Listing {
        projects: vec![row("clean")],
        available: true,
    });
    assert!(
        !out.contains('['),
        "a healthy project needs no marker noise"
    );
}

#[test]
fn the_json_envelope_distinguishes_an_absent_registry_from_an_empty_one() {
    let absent = to_json(&Listing::unavailable());
    assert!(!absent.registry_available);
    assert!(absent.projects.is_empty());

    let empty = to_json(&Listing {
        projects: vec![],
        available: true,
    });
    assert!(empty.registry_available);
    assert!(empty.projects.is_empty());
}

#[test]
fn the_json_row_carries_the_db_path_and_a_derived_resolution_rate() {
    let payload = to_json(&Listing {
        projects: vec![row("svc")],
        available: true,
    });
    let p = &payload.projects[0];
    assert_eq!(p.db_path, "/w/svc/.cartog/db.sqlite");
    let rate = p
        .resolution_rate
        .expect("a rate is derivable from the counts");
    assert!((rate - 13_505.0 / 19_022.0).abs() < 1e-9);
}

#[test]
fn the_json_row_omits_unknown_fields_rather_than_reporting_zero() {
    let mut r = row("bare");
    r.symbol_count = None;
    r.embed_provider = None;
    let payload = to_json(&Listing {
        projects: vec![r],
        available: true,
    });
    let text = serde_json::to_string(&payload).unwrap();
    assert!(
        !text.contains("symbol_count"),
        "an unknown count must be absent, not 0 — an agent would trust a 0"
    );
    assert!(!text.contains("embed_provider"));
}

#[test]
fn a_timestamp_serializes_as_rfc3339_not_as_a_unix_integer() {
    let payload = to_json(&Listing {
        projects: vec![row("svc")],
        available: true,
    });
    assert_eq!(
        payload.projects[0].last_indexed.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
}

#[test]
fn zero_edges_yields_no_resolution_rate_rather_than_zero_percent() {
    // "0% resolved" and "nothing to resolve" are different claims.
    assert_eq!(resolution_rate(Some(0), Some(0)), None);
    assert_eq!(resolution_rate(None, Some(10)), None);
    assert_eq!(resolution_rate(Some(5), None), None);
    assert_eq!(resolution_rate(Some(5), Some(10)), Some(0.5));
}

#[test]
fn a_long_name_is_truncated_with_an_ellipsis_so_it_is_not_mistaken_for_real() {
    let mut r = row("x");
    r.name = "a-really-very-extremely-long-project-name".to_string();
    let out = render(&Listing {
        projects: vec![r],
        available: true,
    });
    assert!(out.contains('…'));
}

/// A description with the given source, for the rows that carry one.
fn described(text: &str, source: DescriptionSource) -> Description {
    Description {
        text: text.to_string(),
        source,
    }
}

#[test]
fn a_declared_name_is_shown_instead_of_the_root_basename() {
    // The user named the project; showing the directory name would ignore that.
    let mut r = row("api");
    r.declared_name = Some("svc-billing".to_string());
    let out = render(&Listing {
        projects: vec![r],
        available: true,
    });
    assert!(out.contains("svc-billing"), "{out}");
    assert!(
        !out.lines().next().unwrap().contains("api"),
        "the basename must not shadow the declared name: {out}"
    );
}

#[test]
fn a_declared_description_renders_without_a_source_suffix() {
    let mut r = row("svc");
    r.description = Some(described(
        "Invoice generation and payment reconciliation.",
        DescriptionSource::Config,
    ));
    let out = render(&Listing {
        projects: vec![r],
        available: true,
    });
    assert!(out.contains("Invoice generation and payment reconciliation."));
    assert!(
        !out.contains("(readme)"),
        "a declared description is not inferred: {out}"
    );
}

#[test]
fn an_inferred_description_is_marked_readme_so_it_is_not_mistaken_for_declared() {
    let mut r = row("svc");
    r.description = Some(described(
        "Guessed from the readme.",
        DescriptionSource::Readme,
    ));
    let out = render(&Listing {
        projects: vec![r],
        available: true,
    });
    assert!(out.contains("Guessed from the readme. (readme)"), "{out}");
}

#[test]
fn a_row_without_a_description_renders_no_description_line() {
    let out = render(&Listing {
        projects: vec![row("svc")],
        available: true,
    });
    // Three lines: the counts row, the db_path row, and the trailing summary.
    let db_line = out
        .lines()
        .position(|l| l.trim_start().starts_with("/w/svc"))
        .expect("the db_path line");
    assert_eq!(db_line, 1, "the db_path must follow the counts row: {out}");
}

#[test]
fn a_long_description_is_truncated_so_the_row_stays_single_line() {
    let mut r = row("svc");
    r.description = Some(described(&"word ".repeat(80), DescriptionSource::Config));
    let out = render(&Listing {
        projects: vec![r],
        available: true,
    });
    let line = out
        .lines()
        .find(|l| l.contains("word"))
        .expect("the description line");
    assert!(line.contains('…'), "{line}");
    assert!(
        line.chars().count() <= 100,
        "{} chars",
        line.chars().count()
    );
}

#[test]
fn the_json_name_is_the_display_name_and_carries_the_description_and_its_source() {
    let mut r = row("api");
    r.declared_name = Some("svc-billing".to_string());
    r.description = Some(described("Invoices.", DescriptionSource::Readme));
    let payload = to_json(&Listing {
        projects: vec![r],
        available: true,
    });
    let p = &payload.projects[0];
    assert_eq!(p.name, "svc-billing");
    assert_eq!(p.description.as_deref(), Some("Invoices."));
    assert_eq!(p.description_source, Some("readme"));
}

#[test]
fn the_json_row_omits_the_description_fields_when_the_project_has_none() {
    // Absent, not null: `description_source` present with no `description`
    // would read as a source for text that is not there.
    let payload = to_json(&Listing {
        projects: vec![row("svc")],
        available: true,
    });
    let text = serde_json::to_string(&payload).unwrap();
    assert!(!text.contains("description"), "{text}");
}
