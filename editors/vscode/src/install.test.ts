// Install-command regression tests: platform gate + exact pinned command
// string. Pure — run via node:test on compiled JS.

import { test } from "node:test";
import * as assert from "node:assert/strict";
import {
  isInstallSupported,
  buildInstallCommand,
  buildIndexCommand,
  shellQuote,
} from "./install";

test("isInstallSupported is false on Windows", () => {
  assert.equal(isInstallSupported("win32"), false);
});

test("isInstallSupported is true on macOS and Linux", () => {
  assert.equal(isInstallSupported("darwin"), true);
  assert.equal(isInstallSupported("linux"), true);
});

test("buildInstallCommand pins the version with a shell-agnostic env prefix", () => {
  assert.equal(
    buildInstallCommand("0.30.0"),
    "curl -fsSL https://www.cartog.dev/install.sh | env CARTOG_VERSION=0.30.0 sh",
  );
});

test("buildInstallCommand with an empty version emits a bare env var (installer coerces to latest)", () => {
  assert.equal(
    buildInstallCommand(""),
    "curl -fsSL https://www.cartog.dev/install.sh | env CARTOG_VERSION= sh",
  );
});

test("buildIndexCommand uses the resolved absolute path, not a bare cartog", () => {
  assert.equal(
    buildIndexCommand("/usr/local/bin/cartog"),
    "'/usr/local/bin/cartog' init && '/usr/local/bin/cartog' index .",
  );
});

test("buildIndexCommand quotes a path containing spaces", () => {
  assert.equal(
    buildIndexCommand("/Users/a b/.local/bin/cartog"),
    "'/Users/a b/.local/bin/cartog' init && '/Users/a b/.local/bin/cartog' index .",
  );
});

// The consent gate refuses a bare `index` when the project has no .cartog.toml
// and no existing index, so `init` must run first and must gate `index`.
test("buildIndexCommand runs init before index, chained so a failed init stops it", () => {
  const command = buildIndexCommand("/usr/local/bin/cartog");
  assert.ok(command.indexOf(" init") < command.indexOf(" index ."));
  assert.ok(command.includes("&&"));
});

test("shellQuote escapes an embedded single quote", () => {
  assert.equal(shellQuote("/o'brien/cartog"), "'/o'\\''brien/cartog'");
});
