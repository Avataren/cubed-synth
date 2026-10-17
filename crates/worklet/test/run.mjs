// Runs test/index.html in a headless Chromium-family browser and prints its
// result: the real AudioWorkletProcessor driving the release wasm module.
//
//   cargo build -p cubed-worklet --target wasm32-unknown-unknown --release
//   node crates/worklet/test/run.mjs        (from the repository root)
//
// BROWSER picks the browser (default: brave); PORT the HTTP port (default
// 47930). The browser gets a throwaway profile. Exits 1 if the test fails.
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const browser = process.env.BROWSER || 'brave';
const port = Number(process.env.PORT || 47930);
const debugPort = port + 1;
const root = process.cwd();
const profile = mkdtempSync(join(tmpdir(), 'cubed-worklet-'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const server = spawn('python3', ['-m', 'http.server', String(port), '--bind', '127.0.0.1', '--directory', root], { stdio: 'ignore' });
const chrome = spawn(browser, [
  '--headless=new', `--user-data-dir=${profile}`, `--remote-debugging-port=${debugPort}`,
  '--no-first-run', '--autoplay-policy=no-user-gesture-required', 'about:blank',
], { stdio: 'ignore' });

let exitCode = 1;
try {
  let target;
  for (let i = 0; i < 100 && !target; i++) {
    await sleep(100);
    try {
      const list = await (await fetch(`http://127.0.0.1:${debugPort}/json`)).json();
      target = list.find((t) => t.type === 'page');
    } catch {}
  }
  if (!target) throw new Error(`${browser} did not start`);
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((r) => ws.addEventListener('open', r));
  let id = 0;
  const calls = new Map();
  ws.addEventListener('message', (e) => {
    const m = JSON.parse(e.data);
    if (m.id && calls.has(m.id)) { calls.get(m.id)(m); calls.delete(m.id); }
    if (m.method === 'Runtime.consoleAPICalled') console.log('[page]', ...m.params.args.map((a) => a.value ?? a.description));
    if (m.method === 'Runtime.exceptionThrown') console.log('[page exception]', m.params.exceptionDetails.text, m.params.exceptionDetails.exception?.description);
  });
  const call = (method, params = {}) => new Promise((resolve) => {
    calls.set(++id, resolve);
    ws.send(JSON.stringify({ id, method, params }));
  });
  await call('Runtime.enable');
  await call('Page.navigate', { url: `http://127.0.0.1:${port}/crates/worklet/test/index.html` });
  let result;
  for (let i = 0; i < 150 && !result; i++) {
    await sleep(100);
    const r = await call('Runtime.evaluate', { expression: 'JSON.stringify(window.result ?? null)', returnByValue: true });
    const value = r.result?.result?.value;
    if (value && value !== 'null') result = JSON.parse(value);
  }
  console.log(JSON.stringify(result ?? { ok: false, error: 'no result within 15 s' }, null, 2));
  exitCode = result?.ok ? 0 : 1;
  ws.close();
} catch (error) {
  console.error(String(error));
} finally {
  chrome.kill();
  server.kill();
  await sleep(300);
  rmSync(profile, { recursive: true, force: true });
  process.exit(exitCode);
}
