//! Drift surfaces driven by the plugin pin the binary hands in.
//!
//! The pin is a `ServerOptions` parameter, never an env read, so these tests
//! need no serialization and mutate no process state. Two rules under test:
//! the drift sentence and the `cartog_stats` fields appear only when the
//! binary is BEHIND the pin, and never on a degraded server (an unconfigured
//! project gets no cartog output of any kind).

use super::test_provider;
use crate::*;

fn pin(behind: bool) -> Option<PluginPin> {
    Some(PluginPin {
        version: "9.9.9".to_string(),
        behind,
        update_command: "/cartog-install".to_string(),
    })
}

fn primary(pin: Option<PluginPin>) -> (tempfile::TempDir, CartogServer) {
    let dir = tempfile::TempDir::new().unwrap();
    let server = CartogServer::new_with_provider(
        &dir.path().join("test.db"),
        test_provider(),
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
        Role::Primary,
    )
    .expect("primary constructs")
    .with_plugin_pin(pin);
    (dir, server)
}

fn degraded(pin: Option<PluginPin>) -> CartogServer {
    CartogServer::new_degraded_for_tests(
        test_provider(),
        indexer::RedactionConfig::disabled(),
        indexer::WalkFilter::unrestricted(),
    )
    .expect("degraded constructs")
    .with_plugin_pin(pin)
}

#[test]
fn instructions_name_the_pin_and_command_when_behind() {
    let (_dir, server) = primary(pin(true));
    let text = server.instructions();
    assert!(text.contains("9.9.9"), "names the pin: {text}");
    assert!(
        text.contains("/cartog-install"),
        "names the command: {text}"
    );
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "names the running version: {text}"
    );
}

#[test]
fn instructions_stay_silent_when_current_or_unpinned() {
    let (_d1, at_pin) = primary(pin(false));
    let (_d2, unpinned) = primary(None);
    for server in [at_pin, unpinned] {
        let text = server.instructions();
        assert!(!text.contains("NOTE:"), "no drift sentence: {text}");
        assert!(!text.contains("9.9.9"), "no pin leaks: {text}");
    }
}

#[test]
fn instructions_stay_silent_when_degraded_even_if_behind() {
    let text = degraded(pin(true)).instructions();
    assert!(
        !text.contains("9.9.9") && !text.contains("NOTE:"),
        "a degraded (unconfigured) server must not surface drift: {text}"
    );
}

#[tokio::test]
async fn stats_carry_pin_and_command_only_when_behind() {
    let (_dir, server) = primary(pin(true));
    let structured = server
        .cartog_stats()
        .await
        .expect("stats succeeds")
        .structured_content
        .expect("stats has structured content");
    assert_eq!(
        structured.get("plugin_pin").and_then(|v| v.as_str()),
        Some("9.9.9")
    );
    assert_eq!(
        structured.get("update_command").and_then(|v| v.as_str()),
        Some("/cartog-install")
    );
}

#[tokio::test]
async fn stats_omit_pin_fields_when_current() {
    let (_dir, server) = primary(pin(false));
    let structured = server
        .cartog_stats()
        .await
        .expect("stats succeeds")
        .structured_content
        .expect("stats has structured content");
    assert!(
        structured.get("plugin_pin").is_none() && structured.get("update_command").is_none(),
        "no drift fields when at the pin: {structured}"
    );
}

#[tokio::test]
async fn stats_omit_pin_fields_when_degraded() {
    let structured = degraded(pin(true))
        .cartog_stats()
        .await
        .expect("stats succeeds")
        .structured_content
        .expect("stats has structured content");
    assert!(
        structured.get("plugin_pin").is_none(),
        "degraded stats must not surface drift: {structured}"
    );
}
