//! Secret detection + redaction applied to extracted symbol text at index time.
//!
//! Redaction runs on the *extracted value strings* (symbol content, signature,
//! docstring), never on the source buffer, whose byte offsets back symbol
//! slicing and Merkle hashing. The placeholder is not length-preserving, so
//! applying it to `source` would corrupt those offsets.
//!
//! Detection is best-effort: anchored, length-bounded patterns for common
//! vendor token shapes plus a quoted `key = value` assignment scan. It favours
//! precision (avoid mangling real code) over recall, so some secrets slip
//! through. It is mitigation, not a guarantee.

use std::borrow::Cow;
use std::path::Path;
use std::sync::OnceLock;

use regex::{Regex, RegexSet};

/// Stable replacement substituted for any detected secret.
pub(crate) const PLACEHOLDER: &str = "[REDACTED_SECRET]";

/// Per-invocation redaction policy.
///
/// `Copy` so it threads through the parallel parse closure and every
/// `index_directory` call site without lifetime plumbing.
#[derive(Debug, Clone, Copy)]
pub struct RedactionConfig {
    /// When true, secret patterns in stored symbol text are replaced with
    /// a redaction placeholder. The sensitive-*file* deny-list is always enforced
    /// regardless of this flag.
    pub enabled: bool,
}

impl Default for RedactionConfig {
    /// Redaction is on by default.
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl RedactionConfig {
    /// Redaction enabled (the default).
    #[must_use]
    pub fn enabled() -> Self {
        Self { enabled: true }
    }

    /// Redaction disabled: `redact` is a verbatim no-op.
    #[must_use]
    pub fn disabled() -> Self {
        Self { enabled: false }
    }

    /// Redact secrets in `input` when enabled; otherwise return it unchanged.
    ///
    /// Returns [`Cow::Borrowed`] when nothing was redacted (the common case),
    /// allocating only when a secret is actually replaced.
    pub(crate) fn redact<'a>(&self, input: &'a str) -> Cow<'a, str> {
        if self.enabled {
            redact_str(input)
        } else {
            Cow::Borrowed(input)
        }
    }
}

/// One detectable secret shape. Variants map 1:1 to compiled patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatternId {
    AwsAccessKeyId,
    AwsSecretKey,
    GithubToken,
    SlackToken,
    StripeKey,
    Jwt,
    GenericAssignment,
}

/// Regex source for each pattern, in [`RegexSet`] order.
///
/// Patterns whose whole match is the secret use no capture group. The two
/// contextual patterns (`AwsSecretKey`, `GenericAssignment`) capture the value
/// so only it is replaced, preserving the surrounding key name as a search
/// anchor. `GenericAssignment` matches `(key)(op+quote)(value)(quote)` and is
/// rebuilt as `$1$2[REDACTED_SECRET]$4` to avoid manual span arithmetic.
const PATTERNS: &[(PatternId, &str)] = &[
    (
        PatternId::AwsAccessKeyId,
        r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA|ANPA|ANVA)[0-9A-Z]{16}\b",
    ),
    (
        PatternId::AwsSecretKey,
        r#"(?i)aws_secret_access_key["'\s:=]+([A-Za-z0-9/+=]{40})"#,
    ),
    (
        PatternId::GithubToken,
        r"\b(?:ghp|gho|ghu|ghs|ghr|github_pat)_[A-Za-z0-9_]{20,255}\b",
    ),
    (PatternId::SlackToken, r"\bxox[baprs]-[A-Za-z0-9-]{10,72}\b"),
    (
        PatternId::StripeKey,
        r"\b(?:sk|rk|pk)_(?:live|test)_[A-Za-z0-9]{16,99}\b",
    ),
    (
        PatternId::Jwt,
        r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
    ),
    (
        PatternId::GenericAssignment,
        r#"(?i)\b(api[_-]?key|secret|token|password|passwd|access[_-]?key)(\s*[:=]\s*["'])([^"'\n]{8,})(["'])"#,
    ),
];

/// Compiled patterns: a `RegexSet` for the cheap "does anything match" probe,
/// and individual `Regex` for replacement. Built once per process.
struct Compiled {
    set: RegexSet,
    regexes: Vec<(PatternId, Regex)>,
}

fn compiled() -> &'static Compiled {
    static COMPILED: OnceLock<Compiled> = OnceLock::new();
    COMPILED.get_or_init(|| {
        let set = RegexSet::new(PATTERNS.iter().map(|(_, p)| *p))
            .expect("redaction patterns are static and valid");
        let regexes = PATTERNS
            .iter()
            .map(|(id, p)| {
                (
                    *id,
                    Regex::new(p).expect("redaction patterns are static and valid"),
                )
            })
            .collect();
        Compiled { set, regexes }
    })
}

/// Replace secret-shaped tokens in `input` with [`PLACEHOLDER`].
///
/// Borrows when no pattern matches (zero allocation). Values equal to the
/// placeholder, env-var references, and short/literal non-secrets are left
/// intact by the assignment matcher's guards.
fn redact_str(input: &str) -> Cow<'_, str> {
    let compiled = compiled();
    let matched = compiled.set.matches(input);
    if !matched.matched_any() {
        return Cow::Borrowed(input);
    }

    let mut out = Cow::Borrowed(input);
    for (idx, (id, re)) in compiled.regexes.iter().enumerate() {
        if !matched.matched(idx) {
            continue;
        }
        let current = out.as_ref();
        let replaced: Cow<'_, str> = match id {
            PatternId::GenericAssignment => re.replace_all(current, |caps: &regex::Captures| {
                let value = &caps[3];
                if is_non_secret_value(value) {
                    caps[0].to_string()
                } else {
                    format!("{}{}{}{}", &caps[1], &caps[2], PLACEHOLDER, &caps[4])
                }
            }),
            PatternId::AwsSecretKey => re.replace_all(current, |caps: &regex::Captures| {
                let whole = &caps[0];
                let value = &caps[1];
                // Replace only the captured 40-char value within the match.
                let prefix_len = whole.len() - value.len();
                format!("{}{}", &whole[..prefix_len], PLACEHOLDER)
            }),
            _ => re.replace_all(current, PLACEHOLDER),
        };
        out = Cow::Owned(replaced.into_owned());
    }
    out
}

/// Values the generic-assignment matcher must not treat as secrets: the
/// placeholder itself (idempotence), env-var references, and obvious literals.
fn is_non_secret_value(value: &str) -> bool {
    if value == PLACEHOLDER {
        return true;
    }
    let v = value.trim();
    matches!(v, "None" | "null" | "nil" | "true" | "false")
        || v.bytes().all(|b| b.is_ascii_digit())
        || v.contains("${")
        || v.contains("{{")
        || v.contains("process.env")
        || v.contains("os.environ")
        || v.contains("getenv")
}

/// Sensitive file extensions (matched case-insensitively, without the dot).
const SENSITIVE_EXTENSIONS: &[&str] = &["pem", "key", "p12", "pfx", "keystore"];

/// Exact sensitive basenames (matched case-insensitively).
const SENSITIVE_BASENAMES: &[&str] = &[
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "credentials.json",
    "secrets.yml",
    "secrets.yaml",
];

/// Whether `rel_path` names a file whose contents must never be indexed.
///
/// Matches `.env` and `.env.*`, common key/cert extensions, and well-known
/// credential filenames. Most such files already lack a recognised code
/// extension and are skipped by `detect_language`; this is the explicit,
/// documented guarantee and catches sensitive names that do carry a code
/// extension. Always enforced, independent of [`RedactionConfig::enabled`].
pub(crate) fn is_sensitive_file(rel_path: &str) -> bool {
    let Some(name) = Path::new(rel_path).file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();

    if name == ".env" || name.starts_with(".env.") {
        return true;
    }
    if SENSITIVE_BASENAMES.contains(&name.as_str()) {
        return true;
    }
    Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SENSITIVE_EXTENSIONS.contains(&ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(s: &str) -> String {
        redact_str(s).into_owned()
    }

    #[test]
    fn redacts_aws_access_key_id() {
        let out = redact("let k = \"AKIAIOSFODNN7EXAMPLE\";");
        assert!(out.contains(PLACEHOLDER));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn redacts_aws_access_key_id_asia_variant() {
        let out = redact("ASIAEXAMPLEFAKEKEY00");
        assert_eq!(out, PLACEHOLDER);
    }

    #[test]
    fn redacts_aws_secret_in_assignment_keeps_key_name() {
        let out = redact("aws_secret_access_key = \"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\"");
        assert!(out.contains("aws_secret_access_key"));
        assert!(out.contains(PLACEHOLDER));
        assert!(!out.contains("wJalrXUtnFEMI"));
    }

    #[test]
    fn redacts_github_pat() {
        let out = redact("token: ghp_1234567890abcdefghijklmnopqrstuvwxyz");
        assert!(out.contains(PLACEHOLDER));
        assert!(!out.contains("ghp_1234567890"));
    }

    #[test]
    fn redacts_github_fine_grained_pat() {
        let out = redact("github_pat_11ABCDEFG0aBcDeFgHiJkLmNoPqRsTuVwXyZ");
        assert!(out.contains(PLACEHOLDER));
    }

    #[test]
    fn redacts_slack_token() {
        // Build the token at runtime so no contiguous xox*-prefixed literal sits
        // in source for secret scanners to flag (the value is still shape-valid).
        let prefix = format!("xo{}b", 'x');
        let token = format!("{prefix}-000000000000-EXAMPLEFAKESLACK");
        let out = redact(&format!("const t = \"{token}\";"));
        assert!(out.contains(PLACEHOLDER));
        assert!(!out.contains(&token));
    }

    #[test]
    fn redacts_stripe_live_key() {
        let out = redact("sk_live_EXAMPLEFAKEKEY00000000");
        assert_eq!(out, PLACEHOLDER);
    }

    #[test]
    fn redacts_stripe_test_key() {
        let out = redact("pk_test_EXAMPLEFAKEKEY00000000");
        assert_eq!(out, PLACEHOLDER);
    }

    #[test]
    fn redacts_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N";
        let out = redact(&format!("auth = \"{jwt}\""));
        assert!(out.contains(PLACEHOLDER));
        assert!(!out.contains("eyJhbGci"));
    }

    #[test]
    fn redacts_generic_password_assignment_keeps_key() {
        let out = redact("password = \"hunter2longvalue\"");
        assert_eq!(out, "password = \"[REDACTED_SECRET]\"");
    }

    #[test]
    fn redacts_generic_api_key_with_colon() {
        let out = redact("api_key: 'sk-abcdefghijklmnop'");
        assert!(out.contains("api_key"));
        assert!(out.contains(PLACEHOLDER));
        assert!(!out.contains("sk-abcdefghijklmnop"));
    }

    #[test]
    fn leaves_clean_code_untouched_returns_borrowed() {
        let src = "fn validate(token: &str) -> bool { token.len() > 0 }";
        assert!(matches!(redact_str(src), Cow::Borrowed(_)));
    }

    #[test]
    fn does_not_redact_env_var_reference() {
        let out = redact("token = \"${GITHUB_TOKEN}\"");
        assert!(out.contains("${GITHUB_TOKEN}"));
        assert!(!out.contains(PLACEHOLDER));
    }

    #[test]
    fn does_not_redact_process_env_reference() {
        let out = redact("api_key = \"process.env.API_KEY\"");
        assert!(!out.contains(PLACEHOLDER));
    }

    #[test]
    fn does_not_redact_short_value() {
        let out = redact("password = \"x\"");
        assert!(!out.contains(PLACEHOLDER));
    }

    #[test]
    fn does_not_redact_literal_none() {
        let out = redact("secret = \"None\"");
        // "None" is len 4 < 8 so it never enters the matcher; assert no change.
        assert!(!out.contains(PLACEHOLDER));
    }

    #[test]
    fn placeholder_is_idempotent_for_assignment() {
        let out = redact("password = \"[REDACTED_SECRET]\"");
        assert_eq!(out, "password = \"[REDACTED_SECRET]\"");
    }

    #[test]
    fn does_not_redact_non_secret_function_call() {
        let src = "let token = self.next_token();";
        assert!(matches!(redact_str(src), Cow::Borrowed(_)));
    }

    #[test]
    fn does_not_redact_uuid_or_hash() {
        let src = "id = \"550e8400-e29b-41d4-a716-446655440000\"";
        // Not behind a secret keyword; left intact.
        assert!(matches!(redact_str(src), Cow::Borrowed(_)));
    }

    #[test]
    fn redacts_multiple_secrets_in_one_string() {
        let out = redact("a = AKIAIOSFODNN7EXAMPLE; b = ghp_1234567890abcdefghijklmnopqrstuvwxyz");
        assert_eq!(out.matches(PLACEHOLDER).count(), 2);
    }

    #[test]
    fn is_sensitive_file_matches_env() {
        assert!(is_sensitive_file(".env"));
        assert!(is_sensitive_file("app/.env.local"));
        assert!(is_sensitive_file("config/.env.production"));
    }

    #[test]
    fn is_sensitive_file_matches_key_and_cert_extensions() {
        assert!(is_sensitive_file("certs/server.pem"));
        assert!(is_sensitive_file("private.key"));
        assert!(is_sensitive_file("bundle.p12"));
        assert!(is_sensitive_file("cert.PFX"));
        assert!(is_sensitive_file("store.keystore"));
    }

    #[test]
    fn is_sensitive_file_matches_known_credential_names() {
        assert!(is_sensitive_file(".ssh/id_rsa"));
        assert!(is_sensitive_file("home/id_ed25519"));
        assert!(is_sensitive_file("gcp/credentials.json"));
        assert!(is_sensitive_file("config/secrets.yml"));
    }

    #[test]
    fn is_sensitive_file_rejects_normal_code() {
        assert!(!is_sensitive_file("src/main.rs"));
        assert!(!is_sensitive_file("app/env.ts"));
        assert!(!is_sensitive_file("lib/keychain.py"));
        assert!(!is_sensitive_file("environment.go"));
    }

    #[test]
    fn disabled_config_is_verbatim_noop() {
        let cfg = RedactionConfig::disabled();
        let out = cfg.redact("AKIAIOSFODNN7EXAMPLE");
        assert_eq!(out, "AKIAIOSFODNN7EXAMPLE");
    }

    #[test]
    fn enabled_config_redacts() {
        let cfg = RedactionConfig::enabled();
        assert!(cfg.redact("AKIAIOSFODNN7EXAMPLE").contains(PLACEHOLDER));
    }

    use proptest::prelude::*;

    /// A shape-valid secret of a known vendor pattern (never a real credential).
    fn known_secret() -> impl Strategy<Value = String> {
        prop_oneof![
            // AWS access key id: AKIA + 16 upper-alnum.
            "[A-Z0-9]{16}".prop_map(|s| format!("AKIA{s}")),
            // GitHub PAT: ghp_ + 36 alnum (>= the 20-char minimum).
            "[A-Za-z0-9]{36}".prop_map(|s| format!("ghp_{s}")),
            // Stripe live secret key: sk_live_ + 24 alnum.
            "[A-Za-z0-9]{24}".prop_map(|s| format!("sk_live_{s}")),
            // JWT: three base64url segments.
            (
                "[A-Za-z0-9_-]{12}",
                "[A-Za-z0-9_-]{12}",
                "[A-Za-z0-9_-]{12}"
            )
                .prop_map(|(a, b, c)| format!("eyJ{a}.{b}.{c}")),
        ]
    }

    /// Filler with no secret shape and no secret keyword: must pass through
    /// unchanged. Alphabet excludes the digits/quotes a vendor shape or
    /// `keyword="value"` assignment would need.
    fn benign_filler() -> impl Strategy<Value = String> {
        "[a-z ()\\-+]{0,40}"
    }

    proptest! {
        /// A known-shape secret spliced into arbitrary surrounding text never
        /// survives verbatim in the output, and the placeholder appears.
        #[test]
        fn known_secret_never_survives(
            secret in known_secret(),
            before in any::<String>(),
            after in any::<String>(),
        ) {
            // Spaces around the secret so `\b` anchors fire (adjacent word chars
            // would extend the token past the shape).
            let input = format!("{before} {secret} {after}");
            let out = redact_str(&input);
            prop_assert!(!out.contains(secret.as_str()), "secret survived: {out}");
            prop_assert!(out.contains(PLACEHOLDER));
        }

        /// Redaction is idempotent: re-redacting already-redacted text is a no-op.
        #[test]
        fn redaction_is_idempotent(input in any::<String>()) {
            let once = redact_str(&input).into_owned();
            let twice = redact_str(&once).into_owned();
            prop_assert_eq!(once, twice);
        }

        /// Redaction never panics on any UTF-8 input (unicode, newlines, control).
        #[test]
        fn redaction_never_panics(input in any::<String>()) {
            let _ = redact_str(&input);
        }

        /// Text with no secret shape and no secret keyword is returned unchanged
        /// (borrowed): over-redaction would silently wreck search recall.
        #[test]
        fn benign_text_is_not_redacted(input in benign_filler()) {
            let out = redact_str(&input);
            prop_assert!(matches!(out, Cow::Borrowed(_)), "over-redacted: {out}");
        }
    }
}
