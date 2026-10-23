// Unit tests for src/lib/links.ts: `node --test tests/links.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isWebUrl, linkHref, linkKey } from '../src/lib/links.ts';
import type { ColumnView, LinkView } from '../src/protocol.ts';

test('only web links are followed', () => {
  assert.ok(isWebUrl('https://example.com'));
  assert.ok(isWebUrl('HTTP://example.com'));
  assert.ok(!isWebUrl('javascript:alert(1)'));
  assert.ok(!isWebUrl('file:///etc/passwd'));
  assert.ok(!isWebUrl('/Users/me/.config/tenx'));
  assert.ok(!isWebUrl('x-safari-https://example.com'));
});

const link = (kind: LinkView['kind'], url: string, label = url): LinkView => ({ url, label, kind, icon: '', color: '' });

test('a port is reached through the host the page came from', () => {
  assert.equal(linkHref(link('port', 'http://localhost:7070/x'), 'mac.tail.ts.net'), 'http://mac.tail.ts.net:7070/x');
  assert.equal(linkHref(link('port', 'http://localhost:7070'), '::1'), 'http://[::1]:7070');
  assert.equal(linkHref(link('web', 'http://localhost:9'), 'mac'), 'http://localhost:9');
});

test('keys in L and f mode name links; others go to the server', () => {
  const pr = link('PR', 'https://github.com/o/r/pull/1');
  const art = link('artifact', 'https://claude.ai/artifact/a');
  const base = { items: [], mode: { kind: 'list' } } as unknown as ColumnView;
  const picker = { ...base, mode: { kind: 'links', task: 'w/t', title: 't', links: [pr, art], selected: 1 } } as ColumnView;
  assert.deepEqual(linkKey(picker, 'Enter', false), { do: 'open', link: art });
  assert.deepEqual(linkKey(picker, '1', false), { do: 'open', link: pr });
  assert.deepEqual(linkKey(picker, 'y', false), { do: 'copy', link: art });
  assert.equal(linkKey(picker, '3', false), null);
  assert.equal(linkKey(picker, 'j', false), null);
  assert.equal(linkKey(picker, 'Enter', true), null);

  const task = { kind: 'task', chips: [{ label: '#1', fg: '', link: pr, hint: 'a' }, { label: '#2 👀', fg: '', link: null, hint: null }] };
  const hints = { ...base, items: [task], mode: { kind: 'hints' } } as unknown as ColumnView;
  assert.deepEqual(linkKey(hints, 'a', false), { do: 'open', link: pr });
  assert.deepEqual(linkKey(hints, 'A', false), { do: 'copy', link: pr });
  assert.equal(linkKey(hints, 'b', false), null);
  assert.equal(linkKey(base, 'a', false), null);
});
