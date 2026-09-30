// Unit tests for src/lib/links.ts: `node --test tests/links.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isWebUrl } from '../src/lib/links.ts';

test('only web links are followed', () => {
  assert.ok(isWebUrl('https://example.com'));
  assert.ok(isWebUrl('HTTP://example.com'));
  assert.ok(!isWebUrl('javascript:alert(1)'));
  assert.ok(!isWebUrl('file:///etc/passwd'));
  assert.ok(!isWebUrl('/Users/me/.config/tenx'));
  assert.ok(!isWebUrl('x-safari-https://example.com'));
});
