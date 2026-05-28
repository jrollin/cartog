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

/// Check if a binary is available on PATH.
pub fn is_binary_available(binary: &str) -> bool {
    std::process::Command::new("which")
        .arg(binary)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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
}
