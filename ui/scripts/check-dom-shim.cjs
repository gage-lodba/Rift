#!/usr/bin/env node
// Fails loudly if the DOM shim cannot do the one thing the renderer tests need.
//
// Worth having as its own check because the failures it guards are *silent in
// the wrong direction*: a shim that loads but installs nothing makes every
// renderer test fail with a `web-sys` panic naming a global rather than this
// file, and a jsdom bumped past its engine floor fails inside Node before any
// test runs at all. This turns both into one legible line, in about a second,
// without compiling anything.
//
// Run: `node scripts/check-dom-shim.cjs` (from ui/). Exits non-zero if any check
// fails.

'use strict';

const assert = require('node:assert');

require('./../tests/dom-shim.cjs');

let failed = false;
const lines = [];

function check(name, fn) {
  try {
    fn();
    lines.push(`  ok   ${name}`);
  } catch (err) {
    lines.push(`  FAIL ${name}: ${err.message.split('\n')[0]}`);
    failed = true;
  }
}

check('document exists and is a document', () => {
  assert.ok(globalThis.document, 'globalThis.document is undefined');
  assert.strictEqual(globalThis.document.nodeType, 9);
});

check('window is jsdom\'s', () => {
  assert.ok(globalThis.window, 'globalThis.window is undefined');
  assert.strictEqual(globalThis.window.document, globalThis.document);
});

// The one assertion here that is about *not* providing something, and it is the
// least obvious requirement in the file. `wasm-bindgen-test` picks its output
// formatter by looking for a `self` property on the global object, writing every
// result line into `document#output` if it finds one. The shim defines `self` as
// `undefined` so the browser formatter is what runs — deliberately, see
// `tests/dom-shim.cjs` — and this pins that it stays that way, because the
// failure mode is a suite that passes with no output.
check('self is intentionally undefined so the browser formatter finds no scope', () => {
  assert.strictEqual(
    globalThis.self,
    undefined,
    'self resolved to a value, so wasm-bindgen-test will run its browser formatter'
  );
});

check('the DOM constructors `web-sys` casts through are global', () => {
  for (const name of [
    'Node',
    'Element',
    'HTMLElement',
    'HTMLInputElement',
    'Event',
    'EventTarget',
    'MouseEvent',
    'Document',
    'MutationObserver',
    'CustomEvent',
  ]) {
    assert.strictEqual(typeof globalThis[name], 'function', `${name} is not a constructor`);
  }
});

check('the test runner\'s output element exists', () => {
  // `wasm-bindgen-test`'s browser formatter writes here; without it the run dies
  // on `Cannot read properties of null` before a single test executes.
  assert.ok(globalThis.document.getElementById('output'), 'no #output element');
});

check('an element can be created, attributed and read back', () => {
  const el = document.createElement('input');
  el.setAttribute('max', '210');
  el.value = '42';
  assert.strictEqual(el.getAttribute('max'), '210');
  assert.strictEqual(el.value, '42');
  assert.ok(el instanceof globalThis.HTMLInputElement, 'instanceof HTMLInputElement');
});

check('a container round-trips innerHTML and querySelector', () => {
  const container = document.createElement('div');
  document.body.appendChild(container);
  container.innerHTML = '<span class="probe">hello</span>';
  assert.ok(container.innerHTML.includes('probe'));
  assert.strictEqual(container.querySelector('.probe').textContent, 'hello');
  container.remove();
});

check('events dispatch through a real listener', () => {
  const target = document.createElement('button');
  document.body.appendChild(target);
  let seen = 0;
  target.addEventListener('click', () => {
    seen += 1;
  });
  target.dispatchEvent(new globalThis.MouseEvent('click', { bubbles: true }));
  assert.strictEqual(seen, 1, 'the listener did not fire');
  target.remove();
});

check('CSSStyleDeclaration accepts a property assignment', () => {
  const el = document.createElement('div');
  el.style.transform = 'translateY(20px)';
  assert.strictEqual(el.style.transform, 'translateY(20px)');
});

console.log(['dom-shim check:', ...lines].join('\n'));

if (failed) {
  console.error(
    '\nthe DOM shim is not usable, so the renderer tests would fail with a panic that names a missing global rather than this message.'
  );
  process.exit(1);
}
