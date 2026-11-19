// Node.js worker for the FerroJS Test262 runner.
//
// Reads one JSON job per line on stdin and writes one JSON result per line on
// stdout. Every job runs in a fresh realm (vm context), as Test262 requires.
// Errors are reported with the phase they happened in: "parse" (compile or
// early error), "resolution" (module linking) or "runtime" (evaluation).
//
// Node.js is a reference engine here: it checks that the runner itself is
// correct, and later it is one of the engines FerroJS is compared against.
'use strict';

const vm = require('node:vm');
const fs = require('node:fs');
const path = require('node:path');
const readline = require('node:readline');

let current = null; // the job in progress, so stray errors land on it

function errorName(e) {
  try {
    if ((typeof e === 'object' && e !== null) || typeof e === 'function') {
      const c = e.constructor;
      if (c && typeof c.name === 'string' && c.name) return c.name;
    }
  } catch (_) {}
  return typeof e;
}

function errorMessage(e) {
  try {
    if (e && typeof e === 'object' && 'message' in e) return String(e.message);
    return String(e);
  } catch (_) {
    return '<unprintable>';
  }
}

function thrown(phase, e) {
  return { phase, name: errorName(e), message: errorMessage(e) };
}

// Builds a realm with Test262's host-defined `print` and `$262`.
function makeRealm(printed) {
  const ctx = vm.createContext({});
  const global = vm.runInContext('this', ctx);
  const define = (obj, name, value) =>
    Object.defineProperty(obj, name, { value, writable: true, configurable: true, enumerable: false });

  define(global, 'print', (msg) => printed.push(String(msg)));

  const $262 = vm.runInContext('({})', ctx);
  define($262, 'global', global);
  define($262, 'evalScript', (src) => new vm.Script(String(src)).runInContext(ctx));
  define($262, 'createRealm', () => makeRealm(printed).$262);
  define($262, 'detachArrayBuffer', (buf) => {
    structuredClone(buf, { transfer: [buf] });
    return null;
  });
  if (typeof globalThis.gc === 'function') define($262, 'gc', () => globalThis.gc());
  define($262, 'agent', vm.runInContext('({})', ctx));
  define(global, '$262', $262);
  return { ctx, global, $262 };
}

async function settle() {
  // Let promise jobs and immediate callbacks from the test run.
  for (let i = 0; i < 3; i++) await new Promise((r) => setImmediate(r));
}

async function waitForPrint(printed, timeoutMs) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (printed.some((p) => p.startsWith('Test262:Async'))) return;
    await new Promise((r) => setTimeout(r, 2));
  }
}

async function whileBusy(m) {
  while (m.status === 'linking' || m.status === 'evaluating') {
    await new Promise((r) => setImmediate(r));
  }
}

// One module loader per realm, shared by module tests and by import() calls
// in scripts, so every module a test loads lives in the test's own realm.
function makeLoader(realm) {
  const cache = new Map();
  const load = (file, source) => {
    if (cache.has(file)) return cache.get(file);
    const m = new vm.SourceTextModule(source ?? fs.readFileSync(file, 'utf8'), {
      identifier: file,
      context: realm.ctx,
      importModuleDynamically: (spec, referrer) => importDynamic(spec, referrer.identifier),
    });
    cache.set(file, m);
    return m;
  };
  const resolve = (spec, fromFile) => path.resolve(path.dirname(fromFile), spec);
  const linker = (spec, referrer) => load(resolve(spec, referrer.identifier));
  // Concurrent import() calls of the same module share one link-and-evaluate.
  const inFlight = new Map();
  const importDynamic = (spec, fromFile) => {
    let m;
    try {
      m = load(resolve(spec, fromFile));
    } catch (e) {
      return Promise.reject(e);
    }
    if (!inFlight.has(m)) {
      inFlight.set(
        m,
        (async () => {
          // A module mid-link or mid-evaluation (e.g. part of a graph that is
          // still loading) must finish that step before it can be evaluated.
          await whileBusy(m);
          if (m.status === 'unlinked') await m.link(linker);
          await whileBusy(m);
          await m.evaluate();
          return m;
        })(),
      );
    }
    return inFlight.get(m);
  };
  return { load, linker, importDynamic };
}

async function runScript(job, realm) {
  const loader = makeLoader(realm);
  let script;
  try {
    script = new vm.Script(job.prelude, {
      filename: job.path,
      importModuleDynamically: (spec) => loader.importDynamic(spec, job.path),
    });
  } catch (e) {
    return thrown('parse', e);
  }
  try {
    script.runInContext(realm.ctx, { timeout: job.timeout_ms });
  } catch (e) {
    return thrown('runtime', e);
  }
  return null;
}

async function runModule(job, realm) {
  // Harness first, as a script in the same realm (raw module tests have none).
  if (job.prelude) {
    try {
      new vm.Script(job.prelude, { filename: 'harness.js' }).runInContext(realm.ctx, { timeout: job.timeout_ms });
    } catch (e) {
      return thrown('runtime', e);
    }
  }

  const { load, linker } = makeLoader(realm);
  let mod;
  try {
    mod = load(job.path, job.source);
  } catch (e) {
    return thrown('parse', e);
  }
  try {
    await mod.link(linker);
  } catch (e) {
    // A dependency that fails to parse is still a resolution-phase failure.
    return thrown('resolution', e);
  }
  try {
    await mod.evaluate({ timeout: job.timeout_ms });
  } catch (e) {
    return thrown('runtime', e);
  }
  return null;
}

async function runJob(job) {
  const printed = [];
  const realm = makeRealm(printed);
  current = { error: null };
  let error = job.kind === 'module' ? await runModule(job, realm) : await runScript(job, realm);
  if (!error) {
    if (job.is_async) await waitForPrint(printed, job.timeout_ms);
    else await settle();
    if (current.error) error = current.error;
  }
  current = null;
  return { error, printed };
}

// Errors thrown from promise jobs or timers belong to the job that caused them.
process.on('uncaughtException', (e) => {
  if (current && !current.error) current.error = thrown('runtime', e);
});
process.on('unhandledRejection', () => {
  // Test262 reports async failures through print(); ignore stray rejections.
});

const queue = [];
let busy = false;
async function pump() {
  if (busy) return;
  busy = true;
  while (queue.length) {
    const job = queue.shift();
    let result;
    try {
      result = await runJob(job);
    } catch (e) {
      result = { error: thrown('runtime', e), printed: [] };
    }
    process.stdout.write(JSON.stringify(result) + '\n');
  }
  busy = false;
}

readline.createInterface({ input: process.stdin }).on('line', (line) => {
  if (!line.trim()) return;
  queue.push(JSON.parse(line));
  pump();
});
