// Unit tests for src/lib/pages.ts: `node --test tests/pages.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chipLabel, isPage, matchesBookmark, pageKey, plusTarget, sameAddress, typedUrl, workspaceOf } from '../src/lib/pages.ts';

test('names a web view by task and page', () => {
  assert.equal(pageKey('acme/fix-login', 'p2'), 'acme/fix-login#p2');
  assert.equal(workspaceOf('acme/fix-login'), 'acme');
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

test('a bookmark fits every typed word in its name or address', () => {
  const b = { name: 'Checkly staging', url: 'https://app.checklyhq.com/accounts/stg' };
  assert.equal(matchesBookmark(b, ''), true);
  assert.equal(matchesBookmark(b, 'STAG'), true);
  assert.equal(matchesBookmark(b, 'checkly stg'), true);
  assert.equal(matchesBookmark(b, 'prod'), false);
});

test('Enter in the + field opens an address, else the first bookmark it matches', () => {
  const list = [
    { name: 'Checkly staging', url: 'https://app.checklyhq.com/accounts/stg' },
    { name: 'Checkly prod', url: 'https://app.checklyhq.com/accounts/prod' },
  ];
  assert.equal(plusTarget('checkly', list), 'https://app.checklyhq.com/accounts/stg');
  assert.equal(plusTarget('checkly prod', list), 'https://app.checklyhq.com/accounts/prod');
  assert.equal(plusTarget('linear.app/acme', list), 'https://linear.app/acme');
  assert.equal(plusTarget('mac:3000', list), 'https://mac:3000');
  assert.equal(plusTarget('nothing', list), 'https://nothing');
  assert.equal(plusTarget('two words', list), null);
  assert.equal(plusTarget('  ', list), null);
});

test('a trailing slash is the same address', () => {
  assert.equal(sameAddress('https://linear.app', 'https://linear.app/'), true);
  assert.equal(sameAddress('https://linear.app/a', 'https://linear.app/b'), false);
});
