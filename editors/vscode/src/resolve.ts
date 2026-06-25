// Pure binary-resolution logic, free of the `vscode` module so it unit-tests in
// plain Node. extension.ts supplies the configured-path setting.

import * as fs from "fs";
import * as os from "os";
import * as path from "path";

// A regular file we can execute. The X_OK check rejects a present-but-non-+x
// `cartog` (partial download, copied data file); X_OK is moot on Windows.
export function isExecutable(p: string): boolean {
  try {
    if (!fs.statSync(p).isFile()) {
      return false;
    }
    if (process.platform === "win32") {
      return true;
    }
    fs.accessSync(p, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

// Expand a leading `~`/`~/` to the home dir — binaryPath is hand-typed and
// ~/.local/bin is one of our own fallback dirs.
export function expandTilde(p: string | undefined): string | undefined {
  if (!p) {
    return p;
  }
  if (p === "~") {
    return os.homedir();
  }
  if (p.startsWith("~/") || p.startsWith("~\\")) {
    return path.join(os.homedir(), p.slice(2));
  }
  return p;
}

// PATH lookup without spawning a shell (the shell is what's missing on a GUI
// launch). Probes each $PATH entry directly.
export function whichOnPath(exe: string): string | undefined {
  const pathVar = process.env.PATH;
  if (!pathVar) {
    return undefined;
  }
  const sep = process.platform === "win32" ? ";" : ":";
  for (const dir of pathVar.split(sep)) {
    if (!dir) {
      continue;
    }
    const candidate = path.join(dir, exe);
    if (isExecutable(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

// Resolve the cartog binary in the same order as skills/cartog/scripts/install.sh.
// A set-but-bad `configured` path falls through (not short-circuit), so a typo
// can't silently disable an otherwise-working cartog.
export function resolveBinary(configured: string | undefined): string | undefined {
  const exe = process.platform === "win32" ? "cartog.exe" : "cartog";

  // 1. binaryPath setting.
  const override = expandTilde(configured?.trim());
  if (override && isExecutable(override)) {
    return override;
  }

  // 2. $CARTOG_INSTALL_DIR.
  const installDir = process.env.CARTOG_INSTALL_DIR;
  if (installDir) {
    const p = path.join(installDir, exe);
    if (isExecutable(p)) {
      return p;
    }
  }

  // 3. PATH (cargo bin, /usr/local/bin, Homebrew).
  const onPath = whichOnPath(exe);
  if (onPath) {
    return onPath;
  }

  // 4. ~/.local/bin then cargo bin — the dirs a GUI-launched VS Code often
  // lacks on PATH. `||` not `??`: empty CARGO_HOME must fall back to ~/.cargo,
  // else join("", "bin", exe) gives the relative "bin/cartog".
  const home = os.homedir();
  const cargoHome = process.env.CARGO_HOME || path.join(home, ".cargo");
  const fallbacks = [path.join(home, ".local", "bin", exe), path.join(cargoHome, "bin", exe)];
  for (const p of fallbacks) {
    if (isExecutable(p)) {
      return p;
    }
  }

  return undefined;
}
