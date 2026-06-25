// cartog VS Code extension — registers the cartog MCP server with Copilot.
// Ships no binary and no new tools: it locates the installed `cartog` and hands
// VS Code a stdio server def for `cartog serve`, so no `.vscode/mcp.json` edit
// and no GUI-launch PATH-spawn failure.

import * as vscode from "vscode";
import { resolveBinary } from "./resolve";

const PROVIDER_ID = "cartog.servers";

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

function buildDefinitions(): vscode.McpServerDefinition[] {
  const command = resolveCartogPath();
  if (!command) {
    return [];
  }
  // One server at the first folder, by design: a cartog DB is per-project and
  // single-writer assumes one serve per repo (matches `cartog ide`).
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri;
  const def = new vscode.McpStdioServerDefinition("cartog", command, serveArgs());
  if (folder) {
    def.cwd = folder;
  }
  return [def];
}

// Warn (with an action) when no binary resolves, so Copilot never gets a dead
// server. Distinguishes a set-but-bad binaryPath from cartog not installed.
async function warnNoBinary(): Promise<void> {
  const configured = vscode.workspace
    .getConfiguration("cartog")
    .get<string>("binaryPath")
    ?.trim();

  const message = configured
    ? `cartog.binaryPath ("${configured}") is not an executable file, and no cartog binary was found elsewhere. Code-graph tools are unavailable in Copilot.`
    : "cartog binary not found on this machine. Install it to enable code-graph tools in Copilot.";

  const choice = await vscode.window.showWarningMessage(
    message,
    "Install instructions",
    "Set path…",
  );
  if (choice === "Install instructions") {
    void vscode.env.openExternal(
      vscode.Uri.parse("https://jrollin.github.io/cartog/usage/"),
    );
  } else if (choice === "Set path…") {
    void vscode.commands.executeCommand(
      "workbench.action.openSettings",
      "cartog.binaryPath",
    );
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
          void warnNoBinary();
        }
      }
    }),
  );

  context.subscriptions.push(
    vscode.lm.registerMcpServerDefinitionProvider(PROVIDER_ID, {
      onDidChangeMcpServerDefinitions: didChange.event,
      provideMcpServerDefinitions: () => buildDefinitions(),
    }),
  );

  if (!resolveCartogPath()) {
    void warnNoBinary();
  }

  context.subscriptions.push(didChange);
}

export function deactivate(): void {
  // Nothing to tear down — VS Code owns the spawned server's lifecycle.
}
