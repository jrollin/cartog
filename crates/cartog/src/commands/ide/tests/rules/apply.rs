use super::{write, Sandbox};
use crate::cli::ClientKind;
use crate::commands::ide::rules::{
    install_rule, named_rule_targets, remove_rule, render, unsupported_rule_step, RuleTarget,
};
use crate::commands::ide::{FileKind, IdeStatus};
use std::fs;
use std::time::{Duration, SystemTime};

const MARKER: &str = "<!-- Managed by cartog: `cartog ide --rules` updates this file and `cartog uninstall` deletes it. Delete this line to keep your own edits. -->";

fn target(sb: &Sandbox, kind: ClientKind) -> RuleTarget {
    let (mut targets, _) = named_rule_targets(&[kind], &sb.cwd);
    targets.remove(0)
}

fn cursor(sb: &Sandbox) -> RuleTarget {
    target(sb, ClientKind::Cursor)
}

fn stale_rule() -> String {
    format!("---\nalwaysApply: true\n---\n{MARKER}\n\nOld cartog text.\n")
}

#[test]
fn an_absent_rule_file_is_created_with_its_directories() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    let step = install_rule(&t, &sb.cwd, false);
    assert_eq!(
        (step.status, fs::read_to_string(&t.path).unwrap()),
        (IdeStatus::Created, render(&t.format))
    );
}

#[test]
fn a_rule_step_is_tagged_as_a_rule_file() {
    let sb = Sandbox::new();
    let step = install_rule(&cursor(&sb), &sb.cwd, false);
    assert_eq!(
        (step.file_kind, step.client.as_str(), step.scope.as_str()),
        (FileKind::Rule, "cursor", "project")
    );
}

#[test]
fn an_identical_rule_file_is_not_rewritten() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    write(&t.path, &render(&t.format));
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&t.path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let step = install_rule(&t, &sb.cwd, false);
    assert_eq!(
        (
            step.status,
            fs::metadata(&t.path).unwrap().modified().unwrap()
        ),
        (IdeStatus::Unchanged, old)
    );
}

#[test]
fn a_rule_file_differing_only_by_crlf_is_unchanged() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    write(&t.path, &render(&t.format).replace('\n', "\r\n"));
    assert_eq!(
        install_rule(&t, &sb.cwd, false).status,
        IdeStatus::Unchanged
    );
}

#[test]
fn a_marked_rule_file_with_other_text_is_updated() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    write(&t.path, &stale_rule());
    let step = install_rule(&t, &sb.cwd, false);
    assert_eq!(
        (step.status, fs::read_to_string(&t.path).unwrap()),
        (IdeStatus::Updated, render(&t.format))
    );
}

#[test]
fn install_leaves_a_file_without_the_marker_byte_identical() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    write(&t.path, "my own rules\n");
    let step = install_rule(&t, &sb.cwd, false);
    assert_eq!(
        (step.status, fs::read_to_string(&t.path).unwrap()),
        (IdeStatus::Skipped, "my own rules\n".to_string())
    );
}

#[test]
fn removal_leaves_a_file_without_the_marker_in_place() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    write(&t.path, "my own rules\n");
    let step = remove_rule(&t, false);
    assert_eq!((step.status, t.path.exists()), (IdeStatus::Skipped, true));
}

#[test]
fn a_blocked_agent_is_skipped_without_creating_anything() {
    let sb = Sandbox::new();
    sb.file(".roorules", "team rules");
    let step = install_rule(&target(&sb, ClientKind::Roo), &sb.cwd, false);
    assert_eq!(
        (
            step.status,
            step.message.contains(".roorules"),
            sb.cwd.join(".roo").exists()
        ),
        (IdeStatus::Skipped, true, false)
    );
}

#[test]
fn a_rules_dir_that_is_a_file_is_named_in_the_skip() {
    let sb = Sandbox::new();
    sb.file(".clinerules", "team rules");
    let step = install_rule(&target(&sb, ClientKind::Cline), &sb.cwd, false);
    assert_eq!(
        (step.status, step.message.contains(".clinerules is a file")),
        (IdeStatus::Skipped, true)
    );
}

#[test]
fn removal_deletes_a_marked_rule_file_and_keeps_its_directory() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    write(&t.path, &stale_rule());
    let step = remove_rule(&t, false);
    assert_eq!(
        (
            step.status,
            t.path.exists(),
            sb.cwd.join(".cursor/rules").is_dir()
        ),
        (IdeStatus::Removed, false, true)
    );
}

#[test]
fn removal_of_an_absent_rule_file_reports_not_wired() {
    let sb = Sandbox::new();
    let step = remove_rule(&cursor(&sb), false);
    assert_eq!(
        (step.status, step.message.as_str()),
        (IdeStatus::Unchanged, "not wired (no rule file)")
    );
}

#[cfg(unix)]
#[test]
fn install_writes_through_a_symlinked_rule_file() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    let real = write(&sb.home.join("dotfiles/cartog.mdc"), &stale_rule());
    fs::create_dir_all(t.path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &t.path).unwrap();
    install_rule(&t, &sb.cwd, false);
    assert_eq!(
        (
            fs::symlink_metadata(&t.path)
                .unwrap()
                .file_type()
                .is_symlink(),
            fs::read_to_string(&real).unwrap()
        ),
        (true, render(&t.format))
    );
}

#[cfg(unix)]
#[test]
fn removal_deletes_the_link_and_keeps_its_target() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    let real = write(&sb.home.join("dotfiles/cartog.mdc"), &stale_rule());
    fs::create_dir_all(t.path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &t.path).unwrap();
    remove_rule(&t, false);
    assert_eq!(
        (fs::symlink_metadata(&t.path).is_ok(), real.exists()),
        (false, true)
    );
}

#[test]
fn a_dry_run_install_creates_nothing_and_shows_the_file() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    let step = install_rule(&t, &sb.cwd, true);
    let diff = step.diff.expect("dry run carries a diff");
    assert_eq!(
        (
            step.status,
            step.message.as_str(),
            diff.before,
            diff.after,
            sb.cwd.join(".cursor").exists()
        ),
        (
            IdeStatus::Created,
            "would create",
            None,
            render(&t.format),
            false
        )
    );
}

#[test]
fn a_dry_run_update_leaves_the_old_text() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    write(&t.path, &stale_rule());
    let step = install_rule(&t, &sb.cwd, true);
    assert_eq!(
        (step.message.as_str(), fs::read_to_string(&t.path).unwrap()),
        ("would update", stale_rule())
    );
}

#[test]
fn a_dry_run_removal_keeps_the_file_and_shows_it() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    write(&t.path, &stale_rule());
    let step = remove_rule(&t, true);
    let diff = step.diff.expect("dry run carries a diff");
    assert_eq!(
        (
            step.status,
            step.message.as_str(),
            diff.before,
            diff.after,
            t.path.exists()
        ),
        (
            IdeStatus::Removed,
            "would remove",
            Some(stale_rule()),
            String::new(),
            true
        )
    );
}

#[test]
fn an_unreadable_rule_path_is_an_error_naming_it() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    fs::create_dir_all(&t.path).unwrap();
    let step = install_rule(&t, &sb.cwd, false);
    assert_eq!(
        (
            step.status,
            step.message.contains(&t.path.display().to_string())
        ),
        (IdeStatus::Error, true)
    );
}

#[test]
fn a_client_without_a_rules_dir_gets_a_pathless_skip() {
    let step = unsupported_rule_step(ClientKind::Zed);
    assert_eq!(
        (
            step.status,
            step.path.as_str(),
            step.file_kind,
            step.message.contains("docs/agent-snippet.md")
        ),
        (IdeStatus::Skipped, "", FileKind::Rule, true)
    );
}

#[test]
fn removal_through_a_rules_dir_that_is_a_file_reports_not_wired() {
    let sb = Sandbox::new();
    sb.file(".clinerules", "legacy cline rules");
    let step = remove_rule(&target(&sb, ClientKind::Cline), false);
    assert_eq!(
        (step.status, step.message.as_str()),
        (IdeStatus::Unchanged, "not wired (no rule file)")
    );
}

#[test]
fn install_skips_a_non_utf8_file_cartog_did_not_write() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    fs::create_dir_all(t.path.parent().unwrap()).unwrap();
    fs::write(&t.path, [0xff, 0xfe, b'x']).unwrap();
    let step = install_rule(&t, &sb.cwd, false);
    assert_eq!(
        (step.status, fs::read(&t.path).unwrap()),
        (IdeStatus::Skipped, vec![0xff, 0xfe, b'x'])
    );
}

#[test]
fn removal_skips_a_non_utf8_file_cartog_did_not_write() {
    let sb = Sandbox::new();
    let t = cursor(&sb);
    fs::create_dir_all(t.path.parent().unwrap()).unwrap();
    fs::write(&t.path, [0xff, 0xfe, b'x']).unwrap();
    let step = remove_rule(&t, false);
    assert_eq!((step.status, t.path.exists()), (IdeStatus::Skipped, true));
}
