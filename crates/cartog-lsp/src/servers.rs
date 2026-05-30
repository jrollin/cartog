use std::path::Path;

/// Specification for a language server binary.
pub struct ServerSpec {
    /// cartog language name (matches `detect_language()` output)
    pub language: &'static str,
    /// Executable name to look up on PATH
    pub binary: &'static str,
    /// Command-line arguments to start in stdio mode
    pub args: &'static [&'static str],
    /// LSP `languageId` for `textDocument/didOpen`
    pub language_id: &'static str,
    /// Install hint shown when binary is not found
    pub install_hint: &'static str,
}

pub const SERVERS: &[ServerSpec] = &[
    ServerSpec {
        language: "rust",
        binary: "rust-analyzer",
        args: &[],
        language_id: "rust",
        install_hint: "rustup component add rust-analyzer",
    },
    ServerSpec {
        language: "python",
        binary: "pyright-langserver",
        args: &["--stdio"],
        language_id: "python",
        install_hint: "npm i -g pyright",
    },
    ServerSpec {
        language: "typescript",
        binary: "typescript-language-server",
        args: &["--stdio"],
        language_id: "typescript",
        install_hint: "npm i -g typescript-language-server typescript",
    },
    ServerSpec {
        language: "tsx",
        binary: "typescript-language-server",
        args: &["--stdio"],
        language_id: "typescriptreact",
        install_hint: "npm i -g typescript-language-server typescript",
    },
    ServerSpec {
        language: "javascript",
        binary: "typescript-language-server",
        args: &["--stdio"],
        language_id: "javascript",
        install_hint: "npm i -g typescript-language-server typescript",
    },
    ServerSpec {
        language: "go",
        binary: "gopls",
        args: &["serve"],
        language_id: "go",
        install_hint: "go install golang.org/x/tools/gopls@latest",
    },
    ServerSpec {
        language: "ruby",
        binary: "ruby-lsp",
        args: &[],
        language_id: "ruby",
        install_hint: "gem install ruby-lsp (requires Ruby >= 3.2)",
    },
    ServerSpec {
        language: "ruby",
        binary: "solargraph",
        args: &["stdio"],
        language_id: "ruby",
        install_hint: "gem install solargraph (requires Ruby >= 3.1)",
    },
    ServerSpec {
        language: "java",
        binary: "jdtls",
        args: &[],
        language_id: "java",
        install_hint: "https://github.com/eclipse-jdtls/eclipse.jdt.ls#installation",
    },
    ServerSpec {
        language: "php",
        binary: "intelephense",
        args: &["--stdio"],
        language_id: "php",
        install_hint: "npm i -g intelephense",
    },
    ServerSpec {
        language: "php",
        binary: "phpactor",
        args: &["language-server"],
        language_id: "php",
        install_hint: "composer global require phpactor/phpactor",
    },
    ServerSpec {
        language: "dart",
        binary: "dart",
        args: &["language-server", "--protocol=lsp"],
        language_id: "dart",
        install_hint: "install Dart SDK from https://dart.dev/get-dart",
    },
];

/// Find all server specs for a cartog language name, in priority order.
pub fn find_servers(language: &str) -> Vec<&'static ServerSpec> {
    SERVERS.iter().filter(|s| s.language == language).collect()
}

/// Check if a binary is available on PATH (resolved directly, not via the
/// Unix-only `which` — that silently disabled LSP on Windows).
pub fn is_binary_available(binary: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let pathext = std::env::var_os("PATHEXT");
    found_on_path(binary, &path, pathext.as_deref())
}

/// PATH-resolution core, separated from env lookup so it's testable without
/// depending on host binaries. On Windows also tries each `PATHEXT` suffix.
fn found_on_path(binary: &str, path: &std::ffi::OsStr, pathext: Option<&std::ffi::OsStr>) -> bool {
    let exts = executable_extensions(pathext);
    std::env::split_paths(path).any(|dir| {
        if dir.as_os_str().is_empty() {
            return false;
        }
        exts.iter().any(|ext| {
            let mut name = std::ffi::OsString::from(binary);
            name.push(ext);
            is_executable_file(&dir.join(&name))
        })
    })
}

/// Suffixes to try for a bare name: empty (as-given), plus `%PATHEXT%` on Windows.
fn executable_extensions(pathext: Option<&std::ffi::OsStr>) -> Vec<std::ffi::OsString> {
    let mut exts = vec![std::ffi::OsString::new()];
    if cfg!(windows) {
        let raw = pathext
            .map(std::ffi::OsStr::to_owned)
            .unwrap_or_else(|| std::ffi::OsString::from(".COM;.EXE;.BAT;.CMD"));
        if let Some(s) = raw.to_str() {
            for e in s.split(';').filter(|e| !e.is_empty()) {
                exts.push(std::ffi::OsString::from(e));
            }
        }
    }
    exts
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_servers_rust() {
        let specs = find_servers("rust");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].binary, "rust-analyzer");
        assert_eq!(specs[0].language_id, "rust");
    }

    #[test]
    fn test_find_servers_dart() {
        let specs = find_servers("dart");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].binary, "dart");
        assert_eq!(specs[0].language_id, "dart");
    }

    #[test]
    fn test_find_servers_unknown_language() {
        assert!(find_servers("cobol").is_empty());
    }

    #[test]
    fn test_find_servers_tsx_uses_typescript_server() {
        let specs = find_servers("tsx");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].binary, "typescript-language-server");
    }

    #[test]
    fn test_find_servers_ruby_has_two_candidates() {
        let specs = find_servers("ruby");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].binary, "ruby-lsp");
        assert_eq!(specs[1].binary, "solargraph");
    }

    #[test]
    fn test_find_servers_php_has_two_candidates() {
        let specs = find_servers("php");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].binary, "intelephense");
        assert_eq!(specs[1].binary, "phpactor");
    }

    #[cfg(unix)]
    fn touch_executable(dir: &Path, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn finds_executable_present_in_a_path_entry() {
        let dir = tempfile::tempdir().unwrap();
        touch_executable(dir.path(), "rust-analyzer");
        let path = dir.path().as_os_str();

        assert!(found_on_path("rust-analyzer", path, None));
    }

    #[cfg(unix)]
    #[test]
    fn reports_missing_when_binary_not_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().as_os_str();

        assert!(!found_on_path("rust-analyzer", path, None));
    }

    #[cfg(unix)]
    #[test]
    fn ignores_non_executable_file_of_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rust-analyzer"), b"not executable").unwrap();
        let path = dir.path().as_os_str();

        assert!(!found_on_path("rust-analyzer", path, None));
    }

    #[test]
    fn pathext_expansion_is_windows_only() {
        // The empty (as-given) suffix is always present; PATHEXT entries are
        // appended only on Windows, so a non-Windows host sees exactly one.
        let exts = executable_extensions(Some(std::ffi::OsStr::new(".EXE;.CMD")));
        if cfg!(windows) {
            assert!(exts.iter().any(|e| e == std::ffi::OsStr::new(".EXE")));
        } else {
            assert_eq!(exts.len(), 1);
            assert_eq!(exts[0], std::ffi::OsString::new());
        }
    }
}
