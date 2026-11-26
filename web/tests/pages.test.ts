// Unit tests for src/lib/pages.ts: `node --test tests/pages.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chipLabel, isLocalUrl, isPage, pageKey, typedUrl, viaProxy, workspaceOf } from '../src/lib/pages.ts';

test('names a web view by task and page', () => {
  assert.equal(pageKey('acme/fix-login', 'p2'), 'acme/fix-login#p2');
  assert.equal(workspaceOf('acme/fix-login'), 'acme');
});

test('knows the Mac-only addresses', () => {
  for (const u of ['http://localhost:3000/x', 'http://127.0.0.1:8080', 'http://app.localhost', 'http://[::1]:5173']) {
    assert.equal(isLocalUrl(u), true, u);
  }
  for (const u of ['https://linear.app', 'http://mac.tailnet.ts.net:3000', 'not a url']) {
    assert.equal(isLocalUrl(u), false, u);
  }
});

test('labels a chip by title, else host and path', () => {
  assert.equal(chipLabel({ id: 'p1', url: 'https://linear.app/acme', title: ' Acme – Linear ', opened: '' }), 'Acme – Linear');
  assert.equal(chipLabel({ id: 'p1', url: 'https://github.com/', title: '', opened: '' }), 'github.com');
  assert.equal(chipLabel({ id: 'p1', url: 'https://github.com/acme/shop', title: '', opened: '' }), 'github.com/acme/shop');
});

test('turns what was typed into an address', () => {
  assert.equal(typedUrl('linear.app/acme'), 'https://linear.app/acme');
  assert.equal(typedUrl(' https://x.dev '), 'https://x.dev');
  assert.equal(typedUrl('localhost:3000'), 'http://localhost:3000');
  assert.equal(typedUrl('app.checklyhq.com'), 'https://app.checklyhq.com');
  assert.equal(typedUrl(''), null);
  assert.equal(typedUrl('two words'), null);
  assert.equal(typedUrl('javascript:alert(1)'), null);
  assert.equal(typedUrl('mailto:a@b.c'), null);
});

test('a page is what it was opened with, wherever it went', () => {
  const page = { id: 'p1', url: 'https://github.com/acme/shop/pull/1#issuecomment-9', title: '', opened: 'https://github.com/acme/shop/pull/1' };
  assert.equal(isPage(page, 'https://github.com/acme/shop/pull/1'), true);
  assert.equal(isPage(page, 'https://github.com/acme/shop/pull/1#issuecomment-9'), true);
  assert.equal(isPage(page, 'https://github.com/acme/shop/pull/2'), false);
});

test('only localhost goes through the page proxy', () => {
  assert.equal(viaProxy('http://localhost:3000/x'), true);
  assert.equal(viaProxy('http://127.0.0.1:8080'), false);
  assert.equal(viaProxy('http://[::1]:5173'), false);
  assert.equal(viaProxy('http://127.0.0.2:3000'), false);
  assert.equal(viaProxy('http://app.localhost'), false);
  assert.equal(viaProxy('https://linear.app'), false);
});
