import * as path from "path";
import { execFile } from "child_process";
import { promisify } from "util";

import {
  ExtensionContext,
  OutputChannel,
  commands,
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
  } catch (err) {
    output?.appendLine(`Failed to start jwc-lsp: ${err}`);
    window.showErrorMessage(`JWC: failed to start jwc-lsp — ${err}`);
  }
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
