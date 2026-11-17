//! Tests for grouping and describing running serve/watch processes.

use std::collections::HashMap;

use crate::commands::self_cmd::*;

fn locks(entries: &[(&str, u32)]) -> Vec<(String, u32)> {
    entries.iter().map(|(s, p)| (s.to_string(), *p)).collect()
}

fn roots(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(h, r)| (h.to_string(), r.to_string()))
        .collect()
}

fn peer(pid: u32, project: Option<&str>, slots: &[&str]) -> RunningPeer {
    RunningPeer {
        pid,
        project: project.map(str::to_string),
        slots: slots.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn a_serve_watch_pair_held_by_one_process_counts_once() {
    let peers = group_peers(&locks(&[("watch-aa", 7), ("serve-aa", 7)]), &roots(&[]));
    assert_eq!(peers, vec![peer(7, None, &["serve-aa", "watch-aa"])]);
}

#[test]
fn each_process_is_named_by_the_project_its_slot_hash_maps_to() {
    let peers = group_peers(
        &locks(&[("serve-aa", 9), ("watch-bb", 3)]),
        &roots(&[("aa", "/w/app"), ("bb", "/w/lib")]),
    );
    assert_eq!(
        peers,
        vec![
            peer(3, Some("/w/lib"), &["watch-bb"]),
            peer(9, Some("/w/app"), &["serve-aa"]),
        ]
    );
}

#[test]
fn an_unregistered_process_is_labelled_by_its_lock_slot() {
    let peers = group_peers(&locks(&[("watch-cc", 4)]), &roots(&[("aa", "/w/app")]));
    assert_eq!(peers[0].label(), "watch-cc");
}

#[test]
fn the_peer_list_names_every_process_up_to_the_cap() {
    let peers = vec![
        peer(1, Some("/w/a"), &["serve-a"]),
        peer(2, None, &["watch-b"]),
    ];
    assert_eq!(format_peer_list(&peers), "PID 1 (/w/a), PID 2 (watch-b)");
}

#[test]
fn the_peer_list_collapses_processes_beyond_the_cap_into_a_count() {
    let peers: Vec<RunningPeer> = (1..=PEER_LIST_CAP as u32 + 2)
        .map(|pid| peer(pid, None, &["serve-x"]))
        .collect();
    assert!(
        format_peer_list(&peers).ends_with(", and 2 more"),
        "{}",
        format_peer_list(&peers)
    );
}

#[test]
fn the_refusal_counts_processes_and_offers_force() {
    let msg = peer_refusal_message(
        &[peer(1, None, &["serve-a"]), peer(2, None, &["serve-b"])],
        false,
    );
    assert!(
        msg.starts_with("2 cartog processes are running: PID 1"),
        "{msg}"
    );
    assert!(
        msg.contains("Read-only sessions on the same projects are not listed"),
        "{msg}"
    );
    assert!(msg.contains("cartog self update --force"), "{msg}");
}

#[test]
fn the_windows_refusal_does_not_offer_force() {
    let msg = peer_refusal_message(&[peer(1, None, &["serve-a"])], true);
    assert!(msg.starts_with("1 cartog process is running"), "{msg}");
    assert!(msg.contains("Close them and retry"), "{msg}");
    assert!(!msg.contains("run `cartog self update --force`"), "{msg}");
}

#[test]
fn the_stale_notice_names_the_target_and_every_old_process() {
    let msg = stale_peers_message(&[peer(1, Some("/w/a"), &["serve-a"])], "0.36.0");
    assert!(
        msg.contains("1 process is still running the previous version: PID 1 (/w/a)"),
        "{msg}"
    );
    assert!(msg.contains("Restart them to use 0.36.0"), "{msg}");
    assert!(
        msg.contains("Read-only sessions on the same projects are not listed"),
        "{msg}"
    );
}
