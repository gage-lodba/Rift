// Node-side DOM environment for the `rift-ui` wasm tests.
//
// `src/tests/harness.rs` replaces the Tauri bridge so the *logic* above it can
// run without a window. This file is the other half of that seam: it gives the
// real Yew **client** renderer (and `web-sys`) a document to render into, so
// the tests can see a second render, an effect firing, and a callback firing.
// Nothing in the shipped bundle reads it — `trunk build` never loads Node.
//
// Loaded with `NODE_OPTIONS=--require <this file>` so it runs *before* the test
// module, which is what makes it work at all: `wasm-bindgen`'s glue captures
// globals lazily but `web-sys` will panic on a missing `document` the moment a
// component mounts, and the runner's generated script gives no hook of its own.
// `ui/package.json`'s `test` script and `.cargo/config.toml`'s `[env]` table
// both set that up; see README.md.
//
// This is a **devDependency of the test harness only**, which is why it lives
// in `package.json` and not in `ui/Cargo.toml`.

'use strict';

const path = require('node:path');
const { createRequire } = require('node:module');

// Resolve `jsdom` from `ui/`, not from Node's current working directory: the
// runner execs the generated script from `target/wasm32-unknown-unknown/…`,
// so a bare `require('jsdom')` would search from there and find nothing.
const uiRequire = createRequire(path.join(__dirname, '..', 'package.json'));

let JSDOM;
try {
  ({ JSDOM } = uiRequire('jsdom'));
} catch (err) {
  throw new Error(
    'the rift-ui test harness needs jsdom (a devDependency of the Node side of ' +
      'the harness, declared in ui/package.json).\n' +
      'Run `npm install` in ui/ first.\n' +
      `Underlying error: ${err.message}`
  );
}

// The document `wasm-bindgen-test` needs, not just the one `web-sys` needs.
//
// `#output` is not decoration. The test runtime decides between its Node and
// browser formatters by looking for a `self` property on the global object
// (`wasm-bindgen-test-0.3.78/src/rt/detect.rs`), and this shim defines `self`
// because a DOM without it is not a DOM. So the browser formatter is what runs,
// and it writes its results into `document.getElementById("output")` — without
// that element the process dies on `Cannot read properties of null` before a
// single test executes, which names neither the element nor this file.
const dom = new JSDOM(
  '<!doctype html><html><head></head><body><pre id="output"></pre></body></html>',
  {
    url: 'http://localhost/',
    pretendToBeVisual: true,
  }
);

const { window } = dom;

// The names JavaScript already provides in Node. These are the *same* language
// and the same values inside jsdom's realm as outside it, so copying them would
// only risk breaking `instanceof` against Node's own objects for no gain.
const JS_INTRINSICS = new Set([
  'globalThis', 'global', 'process', 'Buffer', 'require', 'module', 'exports',
  '__dirname', '__filename', 'console',
  'Object', 'Function', 'Array', 'Number', 'String', 'Boolean', 'Symbol',
  'Math', 'JSON', 'Date', 'RegExp', 'Error', 'AggregateError', 'EvalError',
  'RangeError', 'ReferenceError', 'SyntaxError', 'TypeError', 'URIError',
  'Promise', 'Proxy', 'Reflect', 'Map', 'Set', 'WeakMap', 'WeakSet',
  'WeakRef', 'FinalizationRegistry', 'ArrayBuffer', 'SharedArrayBuffer',
  'DataView', 'Int8Array', 'Uint8Array', 'Uint8ClampedArray', 'Int16Array',
  'Uint16Array', 'Int32Array', 'Uint32Array', 'Float32Array', 'Float64Array',
  'BigInt', 'BigInt64Array', 'BigUint64Array', 'Atomics', 'Intl',
  'encodeURI', 'encodeURIComponent', 'decodeURI', 'decodeURIComponent',
  'escape', 'unescape', 'eval', 'isFinite', 'isNaN', 'parseFloat', 'parseInt',
  'undefined', 'NaN', 'Infinity', 'WebAssembly', 'TextEncoder', 'TextDecoder',
  // Timers stay Node's: they are what the runner's own machinery uses, and
  // jsdom's are the same spec. Overwriting them is a risk with no upside.
  'setTimeout', 'clearTimeout', 'setInterval', 'clearInterval',
  'setImmediate', 'clearImmediate', 'queueMicrotask',
  // Globals that Node provides and that jsdom's own implementations *delegate
  // to* — `Performance-impl.js` is literally `performance.now() -
  // this._nowAtTimeOrigin`. Mirroring jsdom's wrapper over Node's built-in makes
  // the delegation call itself, and the run dies with `Maximum call stack size
  // exceeded` before a single test executes. Node's versions are the correct
  // ones for the host anyway.
  'performance', 'crypto', 'fetch', 'structuredClone',
  'AbortController', 'AbortSignal', 'Blob', 'File', 'FormData',
  'Headers', 'Request', 'Response', 'MessageChannel', 'MessagePort',
  'BroadcastChannel', 'CompressionStream', 'DecompressionStream',
  'TextEncoderStream', 'TextDecoderStream', 'WebSocket', 'EventSource',
]);

const mirrored = [];
const skipped = [];

for (const name of Object.getOwnPropertyNames(window)) {
  if (JS_INTRINSICS.has(name)) {
    continue;
  }
  // `window`, `self`, `top`, `parent`, `frames` point at the window itself;
  // define them explicitly below rather than copying a self-reference.
  if (name === 'window' || name === 'self' || name === 'top' || name === 'parent' || name === 'frames') {
    continue;
  }
  const descriptor = Object.getOwnPropertyDescriptor(window, name);
  if (!descriptor) {
    continue;
  }
  try {
    Object.defineProperty(globalThis, name, descriptor);
    mirrored.push(name);
  } catch {
    // A handful of names (`location`, `navigator`, …) are non-configurable on
    // some Node versions. `web-sys` reaches most of them through `window`,
    // which is defined below, so a skip here is not a gap.
    skipped.push(name);
  }
}

for (const name of ['window', 'top', 'parent']) {
  try {
    Object.defineProperty(globalThis, name, {
      value: window,
      configurable: true,
      writable: true,
    });
  } catch {
    // `window` and `document` arrive with jsdom's own (non-configurable)
    // descriptors from the loop above, which is what we want anyway — they are
    // jsdom's objects.
    globalThis[name] = window;
  }
}

// `document` needs no explicit definition: jsdom's `document` accessor is
// already mirrored above, and its `this` is `globalThis`. Do *not* try to
// redefine it — the mirrored descriptor is non-configurable, and
// `Object.defineProperty` on it throws (`Cannot redefine property: document`),
// which is how this file failed the first time it ran.
if (!globalThis.document) {
  globalThis.document = window.document;
}

// `self` must NOT resolve. This is the least obvious line in the file.
//
// `wasm-bindgen-test` picks its output formatter by asking whether the global
// object has a `self` property (`wasm-bindgen-test-0.3.78/src/rt/detect.rs`):
// `Some` means browser (or worker), `None` means Node. A DOM shim naturally sets
// `self` — it is what a browser window is — and if it does, the *browser*
// formatter runs and writes every result line into
// `document.getElementById("output")` instead of to stdout.
//
// The Node formatter would be the obvious choice for a Node run, so it is worth
// recording why this does not simply delete `self` and take it: jsdom's own
// `window` still answers a structural `self` lookup, so the detection reports
// Browser anyway, and the run then produces *no output at all* — no results, no
// counts, and a failure that reports nothing but an exit code. Keeping the
// browser formatter and mirroring its output to stdout (below) is what makes a
// CI run legible.
globalThis.self = undefined;

// Give a CI log the result lines the browser formatter would otherwise leave in
// the DOM. Without this the suite can pass, or fail, with nothing in the log to
// say which tests ran.
//
// Only the *new* text goes to stdout, not the whole element each time. The
// formatter appends a line at a time and the observer fires per append, so
// re-printing `textContent` grows the log quadratically: measured at 369 lines
// for a 53-test run, every line a longer prefix of the same report, with the one
// line that matters buried in it. The common-prefix diff below also survives the
// formatter replacing the element's contents instead of appending.
const output = window.document.getElementById('output');
if (output) {
  let written = '';
  const stream = () => {
    const text = output.textContent;
    if (text === written) {
      return;
    }
    let common = 0;
    const shared = Math.min(text.length, written.length);
    while (common < shared && text[common] === written[common]) {
      common += 1;
    }
    process.stdout.write(text.slice(common));
    written = text;
  };
  new window.MutationObserver(stream).observe(output, {
    childList: true,
    characterData: true,
    subtree: true,
  });
  process.on('exit', () => {
    stream();
    if (written && !written.endsWith('\n')) {
      process.stdout.write('\n');
    }
  });
}

if (process.env.RIFT_TEST_SHIM_VERBOSE === '1') {
  console.error(
    `[dom-shim] jsdom ${uiRequire('jsdom/package.json').version}: ` +
      `${mirrored.length} globals mirrored` +
      (skipped.length ? `, ${skipped.length} skipped (${skipped.join(', ')})` : '')
  );
}

module.exports = { window, document: window.document, dom };
