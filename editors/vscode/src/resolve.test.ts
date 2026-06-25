// Resolver regression tests: bad-config fall-through, execute-bit check,
// CARGO_HOME="", tilde expansion. Pure — run via node:test on compiled JS.

import { test } from "node:test";
import * as assert from "node:assert/strict";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { resolveBinary, isExecutable, expandTilde } from "./resolve";

// Execute-bit and PATH-separator behaviour is Unix-specific; skip on win32.
const unix = process.platform !== "win32";

function tmpDir(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "cartog-resolve-"));
}

function writeExe(dir: string, name: string): string {
  fs.mkdirSync(dir, { recursive: true });
  const p = path.join(dir, name);
  fs.writeFileSync(p, "#!/bin/sh\n");
  fs.chmodSync(p, 0o755);
  return p;
}

function writeNonExe(dir: string, name: string): string {
  fs.mkdirSync(dir, { recursive: true });
  const p = path.join(dir, name);
  fs.writeFileSync(p, "not a binary");
  fs.chmodSync(p, 0o644);
  return p;
}

// Clear the env around `body` so a host cartog can't leak into tier-3/4 cases.
function withCleanEnv(body: () => void): void {
  const saved = { ...process.env };
  delete process.env.CARTOG_INSTALL_DIR;
  delete process.env.CARGO_HOME;
  process.env.PATH = "";
  try {
    body();
  } finally {
    for (const k of Object.keys(process.env)) {
      delete process.env[k];
    }
    Object.assign(process.env, saved);
  }
}

test("isExecutable rejects a present but non-executable file", { skip: !unix }, () => {
  const dir = tmpDir();
  const p = writeNonExe(dir, "cartog");
  assert.equal(fs.existsSync(p), true);
  assert.equal(isExecutable(p), false);
});

test("isExecutable accepts an executable regular file", { skip: !unix }, () => {
  const dir = tmpDir();
  const p = writeExe(dir, "cartog");
  assert.equal(isExecutable(p), true);
});

test("isExecutable rejects a directory", () => {
  const dir = tmpDir();
  assert.equal(isExecutable(dir), false);
});

test("expandTilde rewrites a leading ~/ to the home dir", () => {
  assert.equal(expandTilde("~/.local/bin/cartog"), path.join(os.homedir(), ".local/bin/cartog"));
  assert.equal(expandTilde("~"), os.homedir());
});

test("expandTilde leaves absolute and undefined inputs unchanged", () => {
  assert.equal(expandTilde("/usr/local/bin/cartog"), "/usr/local/bin/cartog");
  assert.equal(expandTilde(undefined), undefined);
});

test("a non-executable configured path falls through to a later tier", { skip: !unix }, () => {
  const cfgDir = tmpDir();
  const installDir = tmpDir();
  const badConfigured = writeNonExe(cfgDir, "cartog"); // present but not +x
  const real = writeExe(installDir, "cartog"); // tier 2

  withCleanEnv(() => {
    process.env.CARTOG_INSTALL_DIR = installDir;
    const resolved = resolveBinary(badConfigured);
    assert.equal(resolved, real);
    assert.notEqual(resolved, badConfigured);
  });
});

test("an executable configured path wins over every other tier", { skip: !unix }, () => {
  const cfgDir = tmpDir();
  const installDir = tmpDir();
  const configured = writeExe(cfgDir, "cartog");
  writeExe(installDir, "cartog");

  withCleanEnv(() => {
    process.env.CARTOG_INSTALL_DIR = installDir;
    assert.equal(resolveBinary(configured), configured);
  });
});

test('CARGO_HOME="" never yields the relative "bin/cartog"', { skip: !unix }, () => {
  withCleanEnv(() => {
    process.env.CARGO_HOME = ""; // exported but empty
    const resolved = resolveBinary(undefined);
    // No binary here → undefined; the invariant is it's never a relative path.
    if (resolved !== undefined) {
      assert.equal(path.isAbsolute(resolved), true);
    }
  });
});

test("PATH resolution wins over the home fallback dirs", { skip: !unix }, () => {
  const pathDir = tmpDir();
  const real = writeExe(pathDir, "cartog");

  withCleanEnv(() => {
    process.env.PATH = pathDir;
    assert.equal(resolveBinary(undefined), real);
  });
});
