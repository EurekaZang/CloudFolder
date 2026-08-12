const vscode = require('vscode');
const cp = require('child_process');
const net = require('net');
const readline = require('readline');
const { LanguageClient } = require('vscode-languageclient/node');

const clients = new Map();
const testNodeIds = new Map();
let testController;

function workspaceFolder() {
  const editor = vscode.window.activeTextEditor;
  if (editor) {
    const folder = vscode.workspace.getWorkspaceFolder(editor.document.uri);
    if (folder) return folder;
  }
  return vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders[0];
}

function execCf(args, cwd, options = {}) {
  return new Promise((resolve, reject) => {
    cp.execFile('cf', args, { cwd, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024, ...options }, (error, stdout, stderr) => {
      if (error) reject(new Error((stderr || stdout || error.message).trim()));
      else resolve(stdout);
    });
  });
}

function execCfResult(args, cwd) {
  return new Promise((resolve) => {
    cp.execFile('cf', args, { cwd, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 }, (error, stdout, stderr) => {
      resolve({ code: error && typeof error.code === 'number' ? error.code : 0, stdout, stderr });
    });
  });
}

async function startLsp(context, preset, id, name, selector) {
  const folder = workspaceFolder();
  if (!folder) throw new Error('Open a CloudFolder workspace first.');
  const old = clients.get(id);
  if (old) await old.stop();
  const client = new LanguageClient(
    id,
    name,
    { command: 'cf', args: ['lsp', preset], options: { cwd: folder.uri.fsPath } },
    { documentSelector: selector, workspaceFolder: folder }
  );
  clients.set(id, client);
  context.subscriptions.push(client);
  await client.start();
  vscode.window.showInformationMessage(`${name} is running through CloudFolder.`);
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const port = server.address().port;
      server.close(() => resolve(port));
    });
  });
}

async function debugPython() {
  const folder = workspaceFolder();
  const editor = vscode.window.activeTextEditor;
  if (!folder || !editor || editor.document.languageId !== 'python') {
    throw new Error('Open a Python file inside a CloudFolder workspace first.');
  }
  await editor.document.save();
  const port = await freePort();
  const child = cp.spawn(
    'cf',
    ['debug', 'python', '--local-port', String(port), '--', editor.document.uri.fsPath],
    { cwd: folder.uri.fsPath, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] }
  );
  let localRoot = folder.uri.fsPath;
  let remoteRoot = '';
  const stderr = [];
  child.stderr.on('data', chunk => stderr.push(chunk.toString()));
  const ready = new Promise((resolve, reject) => {
    const lines = readline.createInterface({ input: child.stdout });
    const timer = setTimeout(() => reject(new Error('CloudFolder debugger did not become ready.')), 15000);
    lines.on('line', line => {
      if (line.startsWith('Local root:')) localRoot = line.slice('Local root:'.length).trim();
      if (line.startsWith('Remote root:')) remoteRoot = line.slice('Remote root:'.length).trim();
      if (line.includes('Waiting for a DAP client')) {
        clearTimeout(timer);
        resolve();
      }
    });
    child.once('exit', code => {
      if (code && code !== 0) {
        clearTimeout(timer);
        reject(new Error(stderr.join('').trim() || `cf debug exited ${code}`));
      }
    });
  });
  await ready;
  const config = {
    type: 'debugpy',
    name: 'CloudFolder Remote Python',
    request: 'attach',
    connect: { host: '127.0.0.1', port },
    justMyCode: false,
    clientOS: 'windows',
    pathMappings: remoteRoot ? [{ localRoot, remoteRoot }] : undefined
  };
  const started = await vscode.debug.startDebugging(folder, config);
  if (!started) child.kill();
}

class RuntimeSourceProvider {
  async provideTextDocumentContent(uri) {
    const folder = workspaceFolder();
    if (!folder) throw new Error('Open the matching CloudFolder workspace first.');
    const runtimePath = decodeURIComponent(uri.path);
    return execCf(['source', 'read', '--mount', uri.authority, runtimePath], folder.uri.fsPath);
  }
}

function pathBasename(value) {
  return value.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || value;
}

function collectLeafTests(item, output) {
  if (item.children.size === 0) {
    output.push(item);
    return;
  }
  item.children.forEach(child => collectLeafTests(child, output));
}

async function refreshTests() {
  if (!testController) return;
  const folder = workspaceFolder();
  if (!folder) {
    testController.items.replace([]);
    testNodeIds.clear();
    return;
  }
  const raw = await execCf(['test', 'discover', '--framework', 'pytest'], folder.uri.fsPath);
  const discovery = JSON.parse(raw);
  const files = new Map();
  const roots = [];
  testNodeIds.clear();
  for (const test of discovery.tests || []) {
    const uri = vscode.Uri.file(test.path);
    const fileKey = test.path.toLowerCase();
    let fileItem = files.get(fileKey);
    if (!fileItem) {
      fileItem = testController.createTestItem(`file:${test.path}`, pathBasename(test.path), uri);
      files.set(fileKey, fileItem);
      roots.push(fileItem);
    }
    const item = testController.createTestItem(`pytest:${test.id}`, test.name, uri);
    testNodeIds.set(item.id, test.id);
    fileItem.children.add(item);
  }
  testController.items.replace(roots);
}

async function runTests(request, token) {
  const folder = workspaceFolder();
  if (!folder || !testController) return;
  const run = testController.createTestRun(request);
  try {
    const selected = [];
    if (request.include && request.include.length) {
      for (const item of request.include) collectLeafTests(item, selected);
    } else {
      testController.items.forEach(item => collectLeafTests(item, selected));
    }
    const excluded = new Set();
    for (const item of request.exclude || []) {
      const leaves = [];
      collectLeafTests(item, leaves);
      for (const leaf of leaves) excluded.add(leaf.id);
    }
    for (const item of selected) {
      if (token.isCancellationRequested || excluded.has(item.id)) continue;
      const nodeId = testNodeIds.get(item.id);
      if (!nodeId) continue;
      run.started(item);
      const started = Date.now();
      const result = await execCfResult(['test', 'run', nodeId], folder.uri.fsPath);
      const output = `${result.stdout || ''}${result.stderr || ''}`.replace(/\r?\n/g, '\r\n');
      if (output) run.appendOutput(output, undefined, item);
      const duration = Date.now() - started;
      if (result.code === 0) {
        run.passed(item, duration);
      } else {
        run.failed(
          item,
          new vscode.TestMessage((result.stderr || result.stdout || `pytest exited ${result.code}`).trim()),
          duration
        );
      }
    }
  } finally {
    run.end();
  }
}

function setupTests(context) {
  testController = vscode.tests.createTestController('cloudfolder-pytest', 'CloudFolder Pytest');
  testController.refreshHandler = async () => {
    try { await refreshTests(); }
    catch (error) { vscode.window.showErrorMessage(`CloudFolder pytest discovery failed: ${error.message || error}`); }
  };
  context.subscriptions.push(
    testController,
    testController.createRunProfile('Run', vscode.TestRunProfileKind.Run, runTests, true),
    vscode.commands.registerCommand('cloudfolder.refreshTests', async () => {
      try { await refreshTests(); }
      catch (error) { vscode.window.showErrorMessage(`CloudFolder pytest discovery failed: ${error.message || error}`); }
    })
  );
  refreshTests().catch(() => {});
}

function activate(context) {
  setupTests(context);
  context.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider('cloudfolder-runtime', new RuntimeSourceProvider()),
    vscode.commands.registerCommand('cloudfolder.startPythonLsp', () =>
      startLsp(context, 'python', 'cloudfolder-python', 'CloudFolder Remote Python', [{ scheme: 'file', language: 'python' }])
    ),
    vscode.commands.registerCommand('cloudfolder.startClangdLsp', () =>
      startLsp(context, 'clangd', 'cloudfolder-clangd', 'CloudFolder Remote clangd', [
        { scheme: 'file', language: 'c' }, { scheme: 'file', language: 'cpp' }
      ])
    ),
    vscode.commands.registerCommand('cloudfolder.startRustAnalyzer', () =>
      startLsp(context, 'rust', 'cloudfolder-rust', 'CloudFolder Remote rust-analyzer', [{ scheme: 'file', language: 'rust' }])
    ),
    vscode.commands.registerCommand('cloudfolder.debugPython', async () => {
      try { await debugPython(); }
      catch (error) { vscode.window.showErrorMessage(error.message || String(error)); }
    })
  );
}

async function deactivate() {
  await Promise.all([...clients.values()].map(client => client.stop().catch(() => {})));
}

module.exports = { activate, deactivate };
