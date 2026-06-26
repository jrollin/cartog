// Pure install-command logic, free of the `vscode` module so it unit-tests in
// plain Node (mirrors resolve.ts). The web-UI gate lives in extension.ts where
// the vscode `UIKind` enum is available.

// install.sh hard-dies on Windows (download the .zip from Releases instead), so
// the in-editor installer is offered only where the script can run.
export function isInstallSupported(platform: NodeJS.Platform): boolean {
  return platform !== "win32";
}

// The command typed into the terminal. `env VAR=val` is shell-agnostic (bash,
// zsh, sh, fish, nu), and sets CARTOG_VERSION on exactly the `sh` that runs the
// piped script. An empty version is harmless: install.sh coerces it to latest.
export function buildInstallCommand(version: string): string {
  return `curl -fsSL https://www.cartog.dev/install.sh | env CARTOG_VERSION=${version} sh`;
}

// Single-quote a path for a POSIX shell so a space or metacharacter in the
// resolved binary path can't break the command line. Embedded single quotes
// become '\'' (close-quote, escaped quote, reopen).
export function shellQuote(value: string): string {
  return `'${value.replace(/'/g, "'\\''")}'`;
}

// Index the current directory using the already-resolved absolute binary path,
// not a bare `cartog` — a GUI-launched terminal inherits the same PATH-less env
// the resolver works around, so a bare name would print "command not found".
export function buildIndexCommand(binaryPath: string): string {
  return `${shellQuote(binaryPath)} index .`;
}
