// cartog VS Code extension — registers the cartog MCP server with Copilot.
// Ships no binary and no new tools: it locates the installed `cartog` and hands
// VS Code a stdio server def for `cartog serve`, so no `.vscode/mcp.json` edit
// and no GUI-launch PATH-spawn failure. When no binary resolves, it offers to
// run the installer in a terminal (local desktop only).

import * as vscode from "vscode";
import { resolveBinary } from "./resolve";
import { isInstallSupported, buildInstallCommand, buildIndexCommand } from "./install";

const PROVIDER_ID = "cartog.servers";
const DOCS_URL = "https://www.cartog.dev/usage.html";

function configuredBinaryPath(): string | undefined {
  return vscode.workspace.getConfiguration("cartog").get<string>("binaryPath");
}

function resolveCartogPath(): string | undefined {
  return resolveBinary(configuredBinaryPath());
}

function serveArgs(): string[] {
  const watch = vscode.workspace.getConfiguration("cartog").get<boolean>("watch", true);
  return watch ? ["serve", "--watch"] : ["serve"];
}

function firstFolder(): vscode.Uri | undefined {
  return vscode.workspace.workspaceFolders?.[0]?.uri;
}

function buildDefinitions(): vscode.McpServerDefinition[] {
  const command = resolveCartogPath();
  if (!command) {
    return [];
  }
  // One server at the first folder, by design: a cartog DB is per-project and
  // single-writer assumes one serve per repo (matches `cartog ide`).
  const folder = firstFolder();
  const def = new vscode.McpStdioServerDefinition("cartog", command, serveArgs());
  if (folder) {
    def.cwd = folder;
  }
  return [def];
}

// In-editor install is offered only on local desktop where install.sh can run:
// not Windows (the script hard-dies), not the web UI (no usable terminal).
function canOfferInstall(): boolean {
  return isInstallSupported(process.platform) && vscode.env.uiKind !== vscode.UIKind.Web;
}

// Run `cartog install.sh`, pinned to this extension's version, in a terminal.
// sendText with `false` types the command WITHOUT running it: the user reads
// the `curl … | sh` line and presses Enter to consent before anything runs.
// Gated on canOfferInstall so the Command Palette entry can't stage a curl|sh
// where it can never run (Windows has no sh; the web UI has no real terminal).
function runInstall(context: vscode.ExtensionContext): void {
  if (!canOfferInstall()) {
    void vscode.window
      .showWarningMessage(
        "In-editor install isn't available on this platform. Follow the install instructions instead.",
        "Install instructions",
      )
      .then((choice) => {
        if (choice === "Install instructions") {
          void vscode.env.openExternal(vscode.Uri.parse(DOCS_URL));
        }
      });
    return;
  }
  const version = String(context.extension.packageJSON.version ?? "");
  const terminal = vscode.window.createTerminal({ name: "Install cartog", cwd: firstFolder() });
  terminal.show();
  terminal.sendText(buildInstallCommand(version), false);
  void vscode.window.showInformationMessage(
    "Review the install command in the terminal and press Enter to run it. " +
      "When it finishes, run cartog: Recheck from the Command Palette.",
  );
}

// Index the workspace folder using the already-resolved absolute binary path
// (a bare `cartog` would hit the same GUI-launch PATH gap the resolver works
// around). Runs immediately — a trusted local binary, unlike the staged
// installer. Caller guarantees a folder; the cwd anchors `index .`.
function runIndex(binaryPath: string, folder: vscode.Uri): void {
  const terminal = vscode.window.createTerminal({ name: "Index cartog", cwd: folder });
  terminal.show();
  terminal.sendText(buildIndexCommand(binaryPath), true);
}

// Re-resolve after an install (or a manual one). Registers the server if found,
// otherwise nudges a retry — "not found" right after launching usually means
// the install hasn't finished, not that it failed, so we don't re-offer Install.
async function recheck(didChange: vscode.EventEmitter<void>): Promise<void> {
  // Resolve once: only fire when found, since that's the only case where the
  // server def flips from [] to a real def (firing on a miss is a no-op churn).
  const binaryPath = resolveCartogPath();
  if (binaryPath) {
    didChange.fire();
    // Offer indexing only with a folder open — `index .` against the home dir
    // (the cwd fallback) would index the whole tree.
    const folder = firstFolder();
    // Name the .cartog.toml write up front: the action runs `init` before
    // `index` (the consent gate refuses a bare `index`), so clicking it adds a
    // file to the user's repo. The terminal is visible, but that's after the
    // fact — say it before the click, not only in the scrollback.
    const choice = await vscode.window.showInformationMessage(
      "cartog found — code-graph tools are now available." +
        (folder ? " Indexing writes a .cartog.toml config, then builds the graph." : ""),
      ...(folder ? ["Set up and index"] : []),
    );
    if (choice === "Set up and index" && folder) {
      runIndex(binaryPath, folder);
    }
    return;
  }
  const choice = await vscode.window.showWarningMessage(
    "cartog still not found — the install may still be running, or it failed. " +
      "Check the terminal, then run cartog: Recheck again.",
    "Recheck",
  );
  if (choice === "Recheck") {
    await recheck(didChange);
  }
}

// Warn (with actions) when no binary resolves, so Copilot never gets a dead
// server. Distinguishes a set-but-bad binaryPath from cartog not installed, and
// offers an in-editor install where the platform supports it.
async function warnNoBinary(context: vscode.ExtensionContext): Promise<void> {
  const configured = vscode.workspace
    .getConfiguration("cartog")
    .get<string>("binaryPath")
    ?.trim();

  const message = configured
    ? `cartog.binaryPath ("${configured}") is not an executable file, and no cartog binary was found elsewhere. Code-graph tools are unavailable in Copilot.`
    : "cartog binary not found on this machine. Install it to enable code-graph tools in Copilot.";

  const actions = canOfferInstall()
    ? ["Install cartog", "Install instructions", "Set path…"]
    : ["Install instructions", "Set path…"];

  const choice = await vscode.window.showWarningMessage(message, ...actions);
  if (choice === "Install cartog") {
    runInstall(context);
  } else if (choice === "Install instructions") {
    void vscode.env.openExternal(vscode.Uri.parse(DOCS_URL));
  } else if (choice === "Set path…") {
    void vscode.commands.executeCommand("workbench.action.openSettings", "cartog.binaryPath");
  }
}

export function activate(context: vscode.ExtensionContext): void {
  const didChange = new vscode.EventEmitter<void>();

  // Re-query the server def on config change, and warn if the new path is bad
  // (else a typo'd binaryPath fails silently).
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("cartog.binaryPath") || e.affectsConfiguration("cartog.watch")) {
        didChange.fire();
        if (e.affectsConfiguration("cartog.binaryPath") && !resolveCartogPath()) {
          void warnNoBinary(context);
        }
      }
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cartog.install", () => runInstall(context)),
    vscode.commands.registerCommand("cartog.recheck", () => recheck(didChange)),
  );

  context.subscriptions.push(
    vscode.lm.registerMcpServerDefinitionProvider(PROVIDER_ID, {
      onDidChangeMcpServerDefinitions: didChange.event,
      provideMcpServerDefinitions: () => buildDefinitions(),
    }),
  );

  if (!resolveCartogPath()) {
    void warnNoBinary(context);
  }

  context.subscriptions.push(didChange);
}

export function deactivate(): void {
  // Nothing to tear down — VS Code owns the spawned server's lifecycle.
}
