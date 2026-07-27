import * as path from "path";
import { execFile } from "child_process";
import { promisify } from "util";

import {
  ExtensionContext,
  OutputChannel,
  Uri,
  commands,
  env,
  window,
  workspace,
} from "vscode";

import {
  Executable,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

const execFileAsync = promisify(execFile);

let client: LanguageClient | undefined;
let output: OutputChannel | undefined;

export async function activate(context: ExtensionContext): Promise<void> {
  output = window.createOutputChannel("JWC Language Server");
  context.subscriptions.push(output);

  context.subscriptions.push(
    commands.registerCommand("jwc.restartServer", async () => {
      output?.appendLine("Restarting jwc-lsp...");
      await stopClient();
      await startClient(context);
    }),
    commands.registerCommand("jwc.showOutput", () => {
      output?.show(true);
    })
  );

  await startClient(context);
}

export async function deactivate(): Promise<void> {
  await stopClient();
}

async function startClient(context: ExtensionContext): Promise<void> {
  const serverPath = await resolveServerPath();
  if (!serverPath) {
    output?.appendLine(
      "jwc-lsp not found. Set `jwc.lspPath` in settings or install jwc-lsp on PATH."
    );
    window.showWarningMessage(
      "JWC: jwc-lsp not found. Diagnostics disabled. Set `jwc.lspPath` or install jwc-lsp."
    );
    return;
  }

  output?.appendLine(`Starting jwc-lsp: ${serverPath}`);

  const run: Executable = {
    command: serverPath,
    transport: TransportKind.stdio,
    options: { env: process.env },
  };

  const serverOptions: ServerOptions = { run, debug: run };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: "file", language: "jwc" },
      { scheme: "untitled", language: "jwc" },
    ],
    synchronize: {
      fileEvents: workspace.createFileSystemWatcher("**/*.jwc"),
    },
    outputChannel: output,
    traceOutputChannel: output,
  };

  client = new LanguageClient(
    "jwcLanguageServer",
    "JWC Language Server",
    serverOptions,
    clientOptions
  );

  try {
    await client.start();
    context.subscriptions.push({
      dispose: () => {
        void client?.stop();
      },
    });
    reportVersionSkew(context, client);
  } catch (err) {
    output?.appendLine(`Failed to start jwc-lsp: ${err}`);
    window.showErrorMessage(`JWC: failed to start jwc-lsp — ${err}`);
  }
}

/**
 * The extension and `jwc-lsp` ship on the same version line, but they are
 * installed through different channels — the extension updates itself from
 * the marketplace, the binary only moves when someone reruns the installer.
 * A stale binary is the single most confusing failure mode we have: the
 * editor highlights syntax the server then flags as an error, and the
 * Problems panel fills with red on a file that `jwc check` accepts.
 *
 * So say it out loud instead of letting the user debug their own source.
 */
function reportVersionSkew(
  context: ExtensionContext,
  started: LanguageClient
): void {
  const extensionVersion = String(
    context.extension?.packageJSON?.version ?? ""
  );
  const serverVersion = started.initializeResult?.serverInfo?.version;

  if (!serverVersion) {
    output?.appendLine(
      "jwc-lsp did not report a version (pre-0.5 build?). Skipping the version check."
    );
    return;
  }

  output?.appendLine(
    `jwc-lsp ${serverVersion} / extension ${extensionVersion || "unknown"}`
  );

  if (!extensionVersion) return;
  if (compareVersions(serverVersion, extensionVersion) >= 0) return;

  const message =
    `JWC: the installed jwc-lsp is ${serverVersion} but this extension ships ` +
    `against ${extensionVersion}. Diagnostics may flag valid code. Update ` +
    `the toolchain to match.`;
  output?.appendLine(message);
  void window
    .showWarningMessage(message, "How to update", "Dismiss")
    .then((choice) => {
      if (choice === "How to update") {
        void env.openExternal(Uri.parse("https://jwc.1kb.uz/docs/install"));
      }
    });
}

/**
 * Compares `major.minor` only. Patch releases are compiler fixes that don't
 * change the protocol surface, and warning about every one of them would
 * train the user to dismiss the popup that actually matters.
 *
 * Returns <0 if `a` is older than `b`, 0 if the same line, >0 if newer.
 * Anything unparseable (a `-dev` suffix, a git build) compares as equal, so
 * a local build of the server never nags.
 */
function compareVersions(a: string, b: string): number {
  const parse = (v: string): [number, number] | undefined => {
    const m = /^(\d+)\.(\d+)/.exec(v.trim());
    return m ? [Number(m[1]), Number(m[2])] : undefined;
  };
  const left = parse(a);
  const right = parse(b);
  if (!left || !right) return 0;
  if (left[0] !== right[0]) return left[0] - right[0];
  return left[1] - right[1];
}

async function stopClient(): Promise<void> {
  if (!client) return;
  try {
    await client.stop();
  } catch {
    // ignore
  }
  client = undefined;
}

async function resolveServerPath(): Promise<string | undefined> {
  const configured = workspace
    .getConfiguration("jwc")
    .get<string>("lspPath", "")
    .trim();
  if (configured) {
    return configured;
  }

  const exe = process.platform === "win32" ? "jwc-lsp.exe" : "jwc-lsp";
  if (await isOnPath(exe)) {
    return exe;
  }

  const home = process.env.HOME || process.env.USERPROFILE;
  if (home) {
    const candidate = path.join(home, ".jwc", "bin", exe);
    if (await isOnPath(candidate)) {
      return candidate;
    }
  }

  return undefined;
}

async function isOnPath(cmd: string): Promise<boolean> {
  try {
    await execFileAsync(cmd, ["--help"], { timeout: 3000 });
    return true;
  } catch (err: unknown) {
    const code = (err as NodeJS.ErrnoException)?.code;
    return code !== "ENOENT";
  }
}
