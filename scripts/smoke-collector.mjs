import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { promises as fs } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const executable = name => `${name}${process.platform === 'win32' ? '.exe' : ''}`;
function target(manifest) {
  const metadata = execFileSync('cargo', ['metadata', '--manifest-path', manifest,
    '--locked', '--no-deps', '--format-version', '1'], { encoding: 'utf8' });
  return JSON.parse(metadata).target_directory;
}
const collector = path.join(target(path.join(repository, 'Cargo.toml')), 'debug', executable('idle-history-collector'));
const engine = path.join(target(path.join(repository, '../editchain/Cargo.toml')), 'debug', executable('editchain'));
const helper = process.argv[2] || path.join(
  target(path.join(repository, '../codex/tools/codex-session-exporter/Cargo.toml')),
  'debug', executable('codex-session-exporter'));
assert.ok(path.isAbsolute(helper), 'the exporter must have an explicit absolute path');
const root = await fs.mkdtemp(path.join(os.tmpdir(), 'idle-headless-'));
const children = new Set();

function start(binding) {
  const child = spawn(collector, ['--watch', JSON.stringify(binding)], {
    cwd: root, stdio: ['ignore', 'pipe', 'pipe'],
  });
  const state = { child, reports: [], stderr: '', exited: false, error: undefined };
  children.add(state);
  let buffered = '';
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', chunk => {
    buffered += chunk;
    for (;;) {
      const end = buffered.indexOf('\n');
      if (end < 0) break;
      const line = buffered.slice(0, end);
      buffered = buffered.slice(end + 1);
      try { state.reports.push(JSON.parse(line)); }
      catch (error) { state.error = error; }
    }
  });
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', chunk => { state.stderr += chunk; });
  child.on('error', error => { state.error = error; });
  state.finished = once(child, 'exit').then(([code, signal]) => {
    state.exited = true;
    return { code, signal };
  });
  return state;
}

async function wait(state, description, accepts) {
  const deadline = Date.now() + 45000;
  while (!accepts(state.reports)) {
    if (state.error) throw state.error;
    assert.equal(state.exited, false, `${description}: process exited: ${state.stderr}`);
    assert.ok(Date.now() < deadline, `${description}: timeout: ${JSON.stringify(state.reports)} ${state.stderr}`);
    await delay(30);
  }
}

async function stop(state) {
  if (state.exited) return;
  assert.equal(state.child.kill('SIGINT'), true, 'signal the standalone process');
  const result = await Promise.race([
    state.finished,
    delay(10000, undefined, { ref: false }).then(() => { throw new Error('collector did not stop'); }),
  ]);
  assert.deepEqual(result, { code: 0, signal: null }, state.stderr);
  children.delete(state);
}

function history(chain) {
  const page = JSON.parse(execFileSync(engine, [
    '--chain', chain, '--output', 'json', 'history', '--limit', '1000',
  ], { encoding: 'utf8' }));
  assert.equal(page.next_after, null, 'the bounded fixture fits in one page');
  return page.items;
}

try {
  const workspace = path.join(root, 'repository');
  const sessions = path.join(root, 'sessions');
  const chain = path.join(root, 'chain');
  await fs.mkdir(workspace);
  await fs.mkdir(sessions);
  execFileSync('git', ['init', '--quiet', workspace]);
  const source = path.join(sessions, 'rollout-2026-09-21T12-00-00-22222222-2222-7222-8222-222222222222.jsonl');
  const fixture = await fs.readFile(path.join(repository,
    'crates/idle-history-import/tests/fixtures/codex/rollout-contract.jsonl'), 'utf8');
  const lines = fixture.trimEnd().split('\n');
  const metadata = JSON.parse(lines[0]);
  metadata.payload.cwd = workspace;
  lines[0] = JSON.stringify(metadata);
  await fs.writeFile(source, lines.join('\n') + '\n');
  const binding = { workspace, chain, sessions, helper };
  let owner = start(binding);
  await wait(owner, 'automatic source discovery', reports =>
    reports.some(report => report.Ok?.written > 0 && !report.Ok.pending));
  const initial = history(chain);
  assert.ok(initial.length > 0, 'headless collection writes engine history');

  const contender = start(binding);
  await wait(contender, 'exclusive collector ownership', reports =>
    reports.some(report => report.Err?.includes('another collector owns this chain')));
  await stop(contender);
  const appended = JSON.stringify({
    timestamp: '2026-09-21T12:00:07.000Z',
    type: 'response_item',
    payload: {
      type: 'message', id: 'msg_headless_append', role: 'assistant',
      content: [{ type: 'output_text', text: 'Captured without an attached client.' }],
      internal_chat_message_metadata_passthrough: { turn_id: 'turn-1' },
    },
  });
  const previous = owner.reports.length;
  await fs.appendFile(source, appended + '\n');
  await wait(owner, 'append while no client is attached', reports =>
    reports.slice(previous).some(report => report.Ok?.written > 0 && !report.Ok.pending));
  await stop(owner);
  const captured = history(chain);
  assert.ok(captured.length > initial.length, 'the watch loop captures appends automatically');

  owner = start(binding);
  await wait(owner, 'restart from durable cursors', reports =>
    reports.some(report => report.Ok?.changed && !report.Ok.pending));
  assert.equal(owner.reports.filter(report => report.Ok).reduce(
    (sum, report) => sum + report.Ok.written, 0), 0, 'restart does not duplicate history');
  assert.deepEqual(history(chain), captured, 'restart retains every stored reference');
  const rewritten = appended.replace('Captured without an attached client.', 'A replacement source generation.');
  const beforeReplacement = owner.reports.length;
  await fs.writeFile(source + '.replacement', [...lines, rewritten].join('\n') + '\n');
  await fs.rename(source + '.replacement', source);
  await wait(owner, 'replacement source discovery', reports =>
    reports.slice(beforeReplacement).some(report => report.Ok?.written > 0 && !report.Ok.pending));
  await stop(owner);
  assert.ok(history(chain).length > captured.length, 'replacement keeps another source generation');
  console.log('PASS: standalone native discovery, append, exclusive ownership, shutdown, restart and replacement with the real exporter.');
} finally {
  for (const state of children) {
    state.child.kill('SIGKILL');
    await state.finished.catch(() => {});
  }
  await fs.rm(root, { recursive: true, force: true });
}
