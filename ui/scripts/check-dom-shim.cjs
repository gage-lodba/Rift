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
    'KeyboardEvent',
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

check('events dispatch through a real listener: click, keydown and contextmenu', () => {
  // The three kinds `interact.rs` drives, through the same route `dom.rs` uses:
  // the DOM's own constructors with `bubbles: true`, not a bare `Event` carrying
  // a property. Yew delegates events to the subtree root, so a non-bubbling
  // dispatch would never reach a handler, and a `KeyboardEvent` built as a plain
  // `Event` would fail Yew's cast.
  //
  // `contextmenu` is here because it is the row-level way into a kebab menu: a
  // test that needs a menu open *inside a row* cannot click a button for it.
  const target = document.createElement('button');
  document.body.appendChild(target);
  const seen = [];
  target.addEventListener('click', () => seen.push('click'));
  target.addEventListener('keydown', (e) => seen.push(`keydown:${e.key}`));
  target.addEventListener('contextmenu', () => seen.push('contextmenu'));

  const fire = (constructor, type, key) => {
    const init = { bubbles: true, cancelable: true };
    if (key !== undefined) {
      init.key = key;
    }
    target.dispatchEvent(new globalThis[constructor](type, init));
  };
  fire('MouseEvent', 'click');
  fire('KeyboardEvent', 'keydown', 'Enter');
  fire('MouseEvent', 'contextmenu');

  assert.deepStrictEqual(seen, ['click', 'keydown:Enter', 'contextmenu']);
  target.remove();
});

check('focus() moves document.activeElement', () => {
  // Every keyboard assertion in `interact.rs` reads `document.activeElement`, so
  // the shim's ability to move focus is load-bearing.
  const el = document.createElement('div');
  el.setAttribute('tabindex', '0');
  document.body.appendChild(el);
  el.focus();
  assert.strictEqual(document.activeElement, el, 'focus() did not move activeElement');

  // Removing it moves focus back to the body, which is the spec's fixup and is
  // *not* the reason a focus assertion can see a foreign node: `dom.rs` says
  // "NOT inside this mount" when a test that panicked left its container behind,
  // because a panic on wasm32 never runs `Drop`. Measured here so the two are not
  // confused again.
  el.remove();
  assert.strictEqual(
    document.activeElement,
    document.body,
    'removing the focused element should leave activeElement on the body'
  );
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
