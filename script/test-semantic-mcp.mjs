import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as delay } from 'node:timers/promises';

const executable = process.argv[2];
if (!executable) throw new Error('Usage: node script/test-semantic-mcp.mjs /absolute/path/to/zed [rust-analyzer-path]');
const fixture = await mkdtemp(join(tmpdir(), 'zedstorm-semantic-'));
const project = join(fixture, 'project');
const data = join(fixture, 'data');
const source = 'fn alpha() -> i32 { 1 }\nfn beta() -> i32 { alpha() }\nfn main() { let _result = beta(); }\n';
let editor;
let writable;
let readonly;
let editorErrors = '';

function connect(writes) {
  const argumentsList = ['--context-mcp', project, '--user-data-dir', data];
  if (writes) argumentsList.push('--context-mcp-write');
  const child = spawn(resolve(executable), argumentsList, { stdio: ['pipe', 'pipe', 'pipe'] });
  const pending = new Map();
  let identifier = 0;
  let errors = '';
  child.stderr.on('data', chunk => { errors = (errors + chunk).slice(-16384); });
  createInterface({ input: child.stdout }).on('line', line => {
    try {
      const response = JSON.parse(line);
      const request = pending.get(response.id);
      if (!request) return;
      pending.delete(response.id);
      clearTimeout(request.timeout);
      if (response.error) request.reject(new Error(JSON.stringify(response.error)));
      else request.resolve(response.result);
    } catch (error) {
      for (const request of pending.values()) request.reject(error);
      pending.clear();
    }
  });
  child.on('exit', code => {
    for (const request of pending.values()) {
      clearTimeout(request.timeout);
      request.reject(new Error(`MCP exited ${code}: ${errors}`));
    }
    pending.clear();
  });
  child.on('error', error => {
    for (const request of pending.values()) {
      clearTimeout(request.timeout);
      request.reject(error);
    }
    pending.clear();
  });
  function call(method, parameters = {}) {
    const id = ++identifier;
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`MCP timeout: ${method}`));
      }, 65000);
      pending.set(id, { resolve, reject, timeout });
      child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params: parameters }) + '\n');
    });
  }
  async function tool(name, argumentsList = {}) {
    const result = await call('tools/call', { name, arguments: argumentsList });
    if (result.isError) throw new Error(result.content[0].text);
    return result.content[0].text;
  }
  return { child, call, tool };
}

async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGTERM');
  for (let index = 0; index < 50 && child.exitCode === null && child.signalCode === null; index++) await delay(100);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill('SIGKILL');
    await new Promise(resolve => child.once('exit', resolve));
  }
}

async function eventually(query, validate, description) {
  const deadline = Date.now() + 60000;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const result = await query();
      if (validate(result)) return result;
      lastError = new Error(`Result is not ready: ${description}`);
    } catch (error) { lastError = error; }
    await delay(500);
  }
  throw new Error(`${description}: ${lastError?.message}`);
}

try {
  await mkdir(join(project, 'src'), { recursive: true });
  await mkdir(data);
  await mkdir(join(data, 'config'));
  const settings = { session: { trust_all_worktrees: true } };
  if (process.argv[3]) settings.lsp = { 'rust-analyzer': { binary: { path: resolve(process.argv[3]) } } };
  await writeFile(join(data, 'config/settings.json'), JSON.stringify(settings));
  await writeFile(join(project, 'Cargo.toml'), '[package]\nname = "semantic_smoke"\nversion = "0.1.0"\nedition = "2024"\n[workspace]\n');
  await writeFile(join(project, 'src/main.rs'), source);
  const editorArguments = ['--user-data-dir', data, project, join(project, 'src/main.rs')];
  if (process.platform === 'win32') editorArguments.unshift('--foreground');
  editor = spawn(resolve(executable), editorArguments, {
    env: { ...process.env, ZEDSTORM_CONTEXT_MCP_WRITE: '1' }, stdio: ['ignore', 'ignore', 'pipe'],
  });
  editor.stderr.on('data', chunk => { editorErrors = (editorErrors + chunk).slice(-16384); });
  editor.on('error', error => { editorErrors = error.message; });
  let registered = false;
  for (let index = 0; index < 120; index++) {
    if (editor.exitCode !== null) throw new Error(`Editor exited ${editor.exitCode}: ${editorErrors}`);
    try { registered = (await readdir(join(data, 'context-mcp'))).some(name => name.endsWith('.json')); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    if (registered) break;
    await delay(500);
  }
  assert(registered, 'Editor did not publish its semantic bridge');
  writable = connect(true);
  readonly = connect(false);
  for (const client of [writable, readonly]) await client.call('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'semantic-smoke', version: '1' } });
  const tools = (await writable.call('tools/list')).tools.map(tool => tool.name);
  for (const name of ['symbols', 'workspace_symbols', 'definitions', 'references', 'implementations', 'hover', 'incoming_calls', 'outgoing_calls', 'read_buffer', 'rename']) assert(tools.includes(name), name);
  assert(!(await readonly.call('tools/list')).tools.some(tool => tool.name === 'rename'));
  const position = { path: 'src/main.rs', line: 2, column: 19 };
  const blocked = await readonly.call('tools/call', { name: 'rename', arguments: { ...position, revision: 'stale', new_name: 'gamma' } });
  assert.equal(blocked.isError, true);
  let symbols;
  let lastError;
  const languageServerDeadline = Date.now() + 180000;
  while (Date.now() < languageServerDeadline) {
    try {
      symbols = await writable.tool('symbols', { path: 'src/main.rs' });
      if (symbols.includes('alpha')) break;
    } catch (error) { lastError = error; }
    await delay(1000);
  }
  assert(symbols?.includes('alpha'), `Rust language server did not become ready: ${lastError?.message}`);
  await eventually(() => writable.tool('definitions', position), result => result.includes('alpha'), 'definitions');
  await eventually(() => writable.tool('references', { ...position, line: 1, column: 3 }), result => result.includes('src/main.rs'), 'references');
  await eventually(() => writable.tool('hover', position), result => result.includes('alpha'), 'hover');
  await eventually(() => writable.tool('workspace_symbols', { query: 'alpha' }), result => result.includes('alpha'), 'workspace symbols');
  const before = await writable.tool('read_buffer', { path: 'src/main.rs' });
  const revision = /revision=([a-f0-9]{64})/.exec(before)?.[1];
  assert(revision);
  const receipt = await writable.tool('rename', { ...position, line: 1, column: 3, revision, new_name: 'gamma' });
  assert(receipt.includes('renamed buffers='));
  const after = await writable.tool('read_buffer', { path: 'src/main.rs' });
  assert(after.includes('fn gamma'));
  assert(after.includes('gamma() }'));
  assert(after.includes('unsaved=true'));
  assert.equal(await readFile(join(project, 'src/main.rs'), 'utf8'), source);
  await stop(editor);
  const unavailable = await writable.call('tools/call', { name: 'semantic_status', arguments: {} });
  assert.equal(unavailable.isError, true);
  assert((await writable.tool('read', { path: 'src/main.rs' })).includes('fn alpha'));
  console.log('Semantic MCP smoke passed: real editor/LSP, stdio clients, navigation, write gating, unsaved rename, and workspace shutdown');
} catch (error) {
  console.error(error.message);
  if (editorErrors) console.error(editorErrors);
  process.exitCode = 1;
} finally {
  await stop(writable?.child);
  await stop(readonly?.child);
  await stop(editor);
  await rm(fixture, { recursive: true, force: true });
}
