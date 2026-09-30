// Unit tests for src/lib/links.ts: `node --test tests/links.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { browserUrl, isWebUrl } from '../src/lib/links.ts';

test('an installed iOS app hands the link to Safari', () => {
  assert.equal(browserUrl('https://github.com/aluedeke/tenx/pull/1', true), 'x-safari-https://github.com/aluedeke/tenx/pull/1');
  assert.equal(browserUrl('http://localhost:3000/', true), 'x-safari-http://localhost:3000/');
});

test('everywhere else the link is opened as it is', () => {
  assert.equal(browserUrl('https://example.com/a?b=1', false), 'https://example.com/a?b=1');
});

test('only web links are followed', () => {
  assert.ok(isWebUrl('https://example.com'));
  assert.ok(isWebUrl('HTTP://example.com'));
  assert.ok(!isWebUrl('javascript:alert(1)'));
  assert.ok(!isWebUrl('file:///etc/passwd'));
  assert.ok(!isWebUrl('/Users/me/.config/tenx'));
});
