#!/usr/bin/env node
// Fails loudly if the DOM shim cannot do the one thing the renderer tests need.
//
// Worth having as its own check because the failure it guards is *silent in the
// wrong direction*: a shim that loads but leaves `document` undefined makes
// every DOM test fail with a `web-sys` panic that names the missing global and
// not this file, and a jsdom bumped past its engine floor fails inside Node
// before any test runs. This turns both into one legible line.
//
// Run: `node scripts/check-dom-shim.cjs` (from ui/).

'use strict';

const assert = require('node:assert');

require('./../tests/dom-shim.cjs');

const checks = [];
function check(name, fn) {
  try {
    fn();
    checks.push(`  ok   ${name}`);
  } catch (err) {
    checks.push(`  FAIL ${name}: ${err.message}`);
    process.exitCode = 1;
  }
}

check('document exists and is a document', () => {
  assert.ok(globalThis.document, 'globalThis.document is undefined');
  assert.strictEqual(globalThis.document.nodeType, 9);
});

check('window is jsdom\'s and self is set', () => {
  assert.ok(globalThis.window, 'globalThis.window is undefined');
  assert.strictEqual(globalThis.self, globalThis.window);
});

check('the DOM constructors `web-sys` casts through are global', () => {
  for (const name of ['Node', 'Element', 'HTMLElement', 'HTMLInputElement', 'Event', 'EventTarget', 'Document', 'MutationObserver', 'CustomEvent']) {
    assert.strictEqual(typeof globalThis[name], 'function', `${name} is not a constructor`);
  }
});

check('an element can be created, attributed and read back', () => {
  const el = document.createElement('input');
  el.setAttribute('max', '210');
  el.value = '42';
  assert.strictEqual(el.getAttribute('max'), '210');
  assert.strictEqual(el.value, '42');
  assert.ok(el instanceof globalThis.HTMLInputElement, 'instanceof HTMLInputElement');
});

check('a container round-trips innerHTML', () => {
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

console.log(['dom-shim check:', ...checks].join('\n'));
if (process.exitCode) {
  console.error('\nthe DOM shim is not usable; the renderer tests would fail with a panic rather than this message.');
}
