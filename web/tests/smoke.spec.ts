import { expect, test } from '@playwright/test';

test('the column renders the view and the terminal mounts', async ({ page, isMobile }) => {
  await page.goto('/');
  await expect(page.locator('.xterm')).toBeVisible();
  // A phone starts on the terminal; the header's toggle opens the column.
  if (isMobile) await page.getByTestId('toggle').tap();
  await expect(page.getByTestId('conn')).toContainText('mock');
  await expect(page.getByTestId('task')).toHaveCount(8);
  await expect(page.getByTestId('task').first()).toContainText('Cloud tasks');
  await expect(page.getByText('WAITING FOR INPUT')).toBeVisible();
});

test('keys drive the column and ^w cycles the focus', async ({ page, isMobile }) => {
  test.skip(isMobile, 'keyboard');
  await page.goto('/');
  await expect(page.getByTestId('task')).toHaveCount(8);
  const selected = page.locator('.row.sel');
  await expect(selected).toContainText('Fix login timeout');
  await page.keyboard.press('j');
  await expect(selected).toContainText('Better loading indicators');

  // column focused → hidden (terminal has the keyboard) → shown and focused.
  await page.keyboard.press('Control+w');
  await expect(page.getByTestId('column')).toHaveCount(0);
  await expect(page.locator('.xterm-helper-textarea')).toBeFocused();
  await page.keyboard.press('Control+w');
  await expect(page.getByTestId('column')).toBeVisible();
  await expect(page.getByTestId('footer')).toContainText('NORMAL');

  await page.keyboard.press('?');
  await expect(page.getByTestId('help')).toBeVisible();
  await page.keyboard.press('x');
  await expect(page.getByTestId('help')).toHaveCount(0);
});

test('a click selects a row without opening it', async ({ page, isMobile }) => {
  test.skip(isMobile, 'covered by the tap in the first test');
  await page.goto('/');
  await page.getByText('Mobile fixes').click();
  await expect(page.locator('.row.sel')).toContainText('Mobile fixes');
  await expect(page.getByTestId('column')).toBeVisible();
});

/** Every text frame the page sends, for asserting on the messages a control
 * produced. Call before `goto`. */
function sentMessages(page: import('@playwright/test').Page): () => Array<Record<string, unknown>> {
  const sent: string[] = [];
  page.on('websocket', (ws) => ws.on('framesent', (f) => typeof f.payload === 'string' && sent.push(f.payload)));
  return () => sent.map((t) => JSON.parse(t));
}

test('the mouse alone shows and hides the column and drives it', async ({ page, isMobile }) => {
  const sent = sentMessages(page);
  await page.goto('/');
  const tap = async (loc: import('@playwright/test').Locator) => (isMobile ? loc.tap() : loc.click());
  if (isMobile) await tap(page.getByTestId('toggle'));
  await expect(page.getByTestId('column')).toBeVisible();

  // The header's toggle hides it and brings it back.
  await tap(page.getByTestId('toggle'));
  await expect(page.getByTestId('column')).toHaveCount(0);
  await tap(page.getByTestId('toggle'));
  await expect(page.getByTestId('column')).toBeVisible();

  // The blocked row answers on its chip — selected or not.
  const blocked = page.locator('[data-id="acme/fix-login-timeout"]');
  await expect(blocked.getByTestId('answer')).toContainText('permission: Bash');
  await tap(blocked.getByRole('button', { name: '✓ allow' }));
  await expect.poll(() => sent().some((m) => m.type === 'action' && m.name === 'approve')).toBe(true);
  const i = sent().findIndex((m) => m.type === 'action' && m.name === 'approve');
  expect(sent()[i - 1]).toMatchObject({ type: 'click', kind: 'task', id: 'acme/fix-login-timeout' });

  // ? in the header opens the keys; a tap closes them.
  await tap(page.getByTestId('keys'));
  await expect(page.getByTestId('help')).toBeVisible();
  await tap(page.getByTestId('help'));
  await expect(page.getByTestId('help')).toHaveCount(0);
});

test('the selected row has a menu, and a delete asks on the row', async ({ page, isMobile }) => {
  test.skip(isMobile, 'touch gets the sheet: next test');
  const sent = sentMessages(page);
  await page.goto('/');
  await page.getByText('Better loading indicators').click();
  const row = page.locator('.row.sel');
  await row.getByTestId('more').click();
  const menu = page.getByTestId('menu');
  for (const item of ['go to task', 'rename', 'edit repos', 'close window', 'delete…']) {
    await expect(menu.getByRole('menuitem', { name: new RegExp(item) })).toBeVisible();
  }
  // Escape closes it without reaching the column.
  await page.keyboard.press('Escape');
  await expect(menu).toHaveCount(0);
  await expect(page.getByTestId('column')).toBeVisible();

  // A right-click on another row selects it and opens its menu; a closed
  // task has no "close window".
  await page.getByText('Zero permission').click({ button: 'right' });
  await expect(menu).toBeVisible();
  await expect(menu.getByRole('menuitem', { name: /close window/ })).toHaveCount(0);
  await menu.getByRole('menuitem', { name: /delete/ }).click();
  await expect.poll(() => sent().some((m) => m.type === 'action' && m.name === 'delete')).toBe(true);

  // The question sits on the row; the footer only echoes the keys.
  const confirm = page.getByTestId('confirm');
  await expect(confirm).toBeVisible();
  await expect(page.locator('.row.conf')).toContainText('Zero permission');
  await expect(page.getByTestId('footer')).toHaveText('y delete · n keep');
  await confirm.getByRole('button', { name: /keep/ }).click();
  await expect(confirm).toHaveCount(0);
});

test('forms carry their own buttons; + opens the new-task form', async ({ page, isMobile }) => {
  const sent = sentMessages(page);
  await page.goto('/');
  if (isMobile) await page.getByTestId('toggle').tap();
  await (isMobile ? page.getByTestId('add').tap() : page.getByTestId('add').click());
  const buttons = page.locator('.wf').getByTestId('form-buttons');
  await expect(buttons.getByRole('button', { name: /create task/i })).toBeVisible();
  // The form owns the column: no search box, no terminal hint line.
  await expect(page.locator('.search')).toHaveCount(0);
  await expect(page.getByTestId('footer')).toHaveCount(0);
  await buttons.getByRole('button', { name: /cancel/i }).click();
  await expect(page.locator('.wf')).toHaveCount(0);
  expect(sent().some((m) => m.type === 'form' && m.op === 'cancel')).toBe(true);
});

test('on touch, a swipe uncovers allow / deny and a long-press opens the sheet', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'touch');
  await page.goto('/');
  await page.getByTestId('toggle').tap();
  const row = page.locator('[data-id="acme/fix-login-timeout"]');
  const box = (await row.boundingBox())!;
  const at = (dx: number) => ({ pointerType: 'touch', pointerId: 7, isPrimary: true, bubbles: true, clientX: box.x + box.width / 2 + dx, clientY: box.y + box.height / 2 });

  // Swipe left: allow / deny slide out from under the row.
  await row.dispatchEvent('pointerdown', at(0));
  for (const dx of [-20, -60, -110, -150]) await row.dispatchEvent('pointermove', at(dx));
  await row.dispatchEvent('pointerup', at(-150));
  await expect(page.locator('.ua.ok')).toBeVisible();
  await expect(page.locator('.ua.no')).toBeVisible();
  await page.locator('.ua.no').tap();
  await expect(page.locator('.ua.ok')).toHaveCount(0);

  // Hold: the sheet, with the answers first.
  const other = page.locator('[data-id="web/better-loading"]');
  const ob = (await other.boundingBox())!;
  await other.dispatchEvent('pointerdown', { pointerType: 'touch', pointerId: 8, isPrimary: true, bubbles: true, clientX: ob.x + 40, clientY: ob.y + 10 });
  const sheet = page.getByTestId('sheet');
  await expect(sheet).toBeVisible({ timeout: 2000 });
  await other.dispatchEvent('pointerup', { pointerType: 'touch', pointerId: 8, isPrimary: true, bubbles: true, clientX: ob.x + 40, clientY: ob.y + 10 });
  await expect(sheet.getByRole('menuitem', { name: 'go to task' })).toBeVisible();
  await expect(sheet.getByRole('menuitem', { name: 'delete…' })).toBeVisible();
});

test('the bell turns notifications on; an iPhone outside the Home Screen is told to install first', async ({ page, browser, isMobile }) => {
  await page.goto('/');
  if (isMobile) await page.getByTestId('toggle').tap();
  const bell = page.getByTestId('push');
  await expect(bell).toBeVisible();
  // Chromium on http://127.0.0.1 (a secure context) has Web Push: off until
  // asked — or denied, which headless Chromium answers for every page.
  await expect(bell).toHaveAttribute('data-state', /^(off|denied)$/);

  // Safari on an iPhone, as a tab: no Push API until it's a Home Screen app.
  const ios = await browser.newContext({
    userAgent: 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1',
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
  });
  await ios.addInitScript(() => {
    // @ts-expect-error — Safari outside a Home Screen app has none.
    delete window.PushManager;
  });
  const phone = await ios.newPage();
  await phone.goto('/');
  await phone.getByTestId('toggle').tap();
  await expect(phone.getByTestId('push-hint')).toContainText('Add to Home Screen');
  await expect(phone.getByTestId('push')).toHaveAttribute('data-state', 'needs-install');
  await ios.close();
});

test('the manifest and service worker make it installable', async ({ request }) => {
  const manifest = await (await request.get('/manifest.webmanifest')).json();
  expect(manifest.display).toBe('standalone');
  expect(manifest.id).toBe('/');
  const sizes = manifest.icons.map((i: { sizes: string; purpose?: string }) => `${i.sizes}:${i.purpose ?? 'any'}`);
  expect(sizes).toEqual(expect.arrayContaining(['192x192:any', '512x512:any', '512x512:maskable']));
  for (const icon of manifest.icons) expect((await request.get(icon.src)).ok()).toBeTruthy();
  const sw = await request.get('/sw.js');
  expect(await sw.text()).toContain("addEventListener('push'");
});

test('the key bar only shows with the on-screen keyboard up', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'touch screens only');
  await page.goto('/');
  // Headless has no on-screen keyboard: no bar at all.
  await expect(page.getByTestId('keybar')).toHaveCount(0);
  // A keyboard taking a third of the screen, for the focused terminal: the
  // bar on top of it.
  await page.locator('.xterm-helper-textarea').focus();
  await page.evaluate(() => {
    const vv = window.visualViewport!;
    Object.defineProperty(vv, 'height', { configurable: true, get: () => 500 });
    vv.dispatchEvent(new Event('resize'));
  });
  const bar = page.getByTestId('keybar');
  await expect(bar.getByRole('button', { name: 'esc' })).toBeVisible();
  await expect(bar.getByTestId('attach')).toBeVisible();
});

test('a sliver of a viewport while switching apps keeps the whole page', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'touch screens only');
  await page.goto('/');
  await page.locator('.xterm-helper-textarea').focus();
  // iPadOS reports ~10% of the screen for the app switcher's snapshot.
  await page.evaluate(() => {
    const vv = window.visualViewport!;
    Object.defineProperty(vv, 'height', { configurable: true, get: () => 80 });
    vv.dispatchEvent(new Event('resize'));
  });
  const app = await page.locator('.app').boundingBox();
  const inner = await page.evaluate(() => window.innerHeight);
  expect(app!.height).toBeGreaterThan(inner * 0.9);
  await expect(page.getByTestId('keybar')).toHaveCount(0);
});

test('the header names the task you are in and drives the column', async ({ page, isMobile }) => {
  const sent = sentMessages(page);
  await page.goto('/');
  const tap = async (loc: import('@playwright/test').Locator) => (isMobile ? loc.tap() : loc.click());
  const header = page.getByTestId('header');
  const current = header.getByTestId('current');
  await expect(current).toContainText('Fix login timeout');
  await expect(current).toContainText('permission: Bash');
  // Only the task you're in needs you: nothing to jump to.
  await expect(header.getByTestId('next')).toHaveCount(0);

  // The toggle: the column appears (on a phone, over the terminal, under the
  // header) and goes again.
  if (isMobile) {
    await tap(header.getByTestId('toggle'));
    await expect(page.getByTestId('column')).toBeVisible();
    const h = (await header.boundingBox())!;
    const c = (await page.getByTestId('column').boundingBox())!;
    expect(c.y).toBeGreaterThanOrEqual(h.y + h.height - 1);
    await tap(header.getByTestId('toggle'));
    await expect(page.getByTestId('column')).toHaveCount(0);
  } else {
    await tap(header.getByTestId('toggle'));
    await expect(page.getByTestId('column')).toHaveCount(0);
  }

  // The current task: back in the column, selected.
  await tap(current);
  await expect(page.getByTestId('column')).toBeVisible();
  await expect.poll(() => sent().some((m) => m.type === 'click' && m.kind === 'task' && m.id === 'acme/fix-login-timeout')).toBe(true);

  // + and ? are the new-task and keys actions.
  await tap(header.getByTestId('keys'));
  await expect.poll(() => sent().some((m) => m.type === 'action' && m.name === 'help')).toBe(true);
  await tap(page.getByTestId('help'));
  await tap(header.getByTestId('add'));
  await expect.poll(() => sent().some((m) => m.type === 'action' && m.name === 'new')).toBe(true);
});

test('a fresh start goes back to the last task, if its window is open', async ({ page }) => {
  // The mock's current task is acme/fix-login-timeout; this device last had
  // web/better-loading (open) in front of it.
  await page.addInitScript(() => {
    localStorage.setItem('tenx-last-task', 'web/better-loading');
    localStorage.removeItem('tenx-web-session');
  });
  const sent = sentMessages(page);
  await page.goto('/');
  await expect.poll(() => sent().some((m) => m.type === 'action' && m.name === 'open')).toBe(true);
  expect(sent().some((m) => m.type === 'click' && m.kind === 'task' && m.id === 'web/better-loading')).toBe(true);
});

test('a closed last task is not reopened', async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem('tenx-last-task', 'notes/competitor-analysis');
    localStorage.removeItem('tenx-web-session');
  });
  const sent = sentMessages(page);
  await page.goto('/');
  await expect(page.getByTestId('header')).toBeVisible();
  await page.waitForTimeout(800);
  expect(sent().some((m) => m.type === 'action' && m.name === 'open')).toBe(false);
});

test('the new-task form is a real web form', async ({ page, isMobile }) => {
  const sent = sentMessages(page);
  const forms = () => sent().filter((m) => m.type === 'form');
  const tap = (loc: import('@playwright/test').Locator) => (isMobile ? loc.tap() : loc.click());
  const chip = (label: string) => page.locator('.wf-chip', { hasText: new RegExp(`^\\W*${label}`) });
  await page.goto('/');
  if (isMobile) await page.getByTestId('toggle').tap();
  await tap(page.getByTestId('add'));
  const name = page.locator('input[data-field=name]');
  await expect(name).toBeVisible();
  if (!isMobile) await expect(name).toBeFocused();
  await name.fill('Rate limit login');
  await expect.poll(() => forms().some((m) => m.op === 'set' && m.field === 'name' && m.value === 'Rate limit login')).toBe(true);
  // The server's slug for it, under the name.
  await expect(page.getByTestId('slug')).toHaveText('→ rate-limit-login');

  // Workspace chips: another workspace brings its repos.
  await tap(chip('notes'));
  await expect.poll(() => forms().some((m) => m.op === 'pick' && m.field === 'workspace' && m.index === 1)).toBe(true);
  await expect(page.getByRole('checkbox', { name: 'notes' })).toBeChecked();
  await expect(page.getByRole('checkbox', { name: /api/ })).toHaveCount(0);
  await tap(chip('acme'));
  await expect(page.getByRole('radio', { name: /acme/ })).toBeChecked();
  // Repo chips toggle; "none" / "all" check each one.
  await tap(chip('web'));
  await expect.poll(() => forms().some((m) => m.op === 'check' && m.field === 'repo' && m.index === 1 && m.on === false)).toBe(true);
  await expect(page.getByRole('checkbox', { name: /web/ })).not.toBeChecked();
  await tap(page.getByRole('button', { name: 'none' }));
  await expect(page.getByRole('checkbox', { name: /api/ })).not.toBeChecked();
  await tap(page.getByRole('button', { name: 'all' }));
  await expect(page.getByRole('checkbox', { name: /infra/ })).toBeChecked();
  // Agent chips; "default" names what it resolves to.
  await expect(chip('default')).toContainText('claude');
  await tap(chip('codex'));
  await expect.poll(() => forms().some((m) => m.op === 'pick' && m.field === 'agent' && m.index === 2)).toBe(true);

  // Tab walks the fields natively; Enter submits.
  if (!isMobile) {
    await name.focus();
    await page.keyboard.press('Tab');
    await expect(page.getByRole('radio', { name: /acme/ })).toBeFocused();
  }
  await name.press('Enter');
  await expect.poll(() => forms().some((m) => m.op === 'submit')).toBe(true);
  // The job it started: the form stays, frozen, with the progress (3g)…
  await expect(page.getByTestId('form-progress')).toContainText("creating 'Rate limit login'");
  await expect(page.getByTestId('form-progress')).toContainText('Work tab');
  // …until the job lands, and the list is back.
  await expect(page.locator('.webform')).toHaveCount(0, { timeout: 10_000 });
  await expect(page.getByTestId('list')).toBeVisible();
  // Typing in the form never went to the column as keys.
  expect(sent().some((m) => m.type === 'key' && m.key === 'R')).toBe(false);
});

test('esc leaves a running job to the Work tab', async ({ page, isMobile }) => {
  test.skip(isMobile, 'keyboard');
  await page.goto('/');
  await page.getByTestId('add').click();
  await page.locator('input[data-field=name]').fill('Leave it');
  await page.locator('input[data-field=name]').press('Enter');
  await expect(page.getByTestId('form-progress')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.locator('.webform')).toHaveCount(0);
  await expect(page.getByTestId('list')).toBeVisible();
});

test('a refused submit shows why inside the form; Escape cancels', async ({ page, isMobile }) => {
  test.skip(isMobile, 'keyboard');
  const sent = sentMessages(page);
  await page.goto('/');
  await page.getByTestId('add').click();
  await page.locator('input[data-field=name]').press('Enter');
  // Under the field it is about, which turns red (3a).
  await expect(page.getByTestId('form-error')).toHaveText('name the task first');
  await expect(page.locator('input[data-field=name]')).toHaveClass(/err/);
  await expect(page.getByTestId('form-progress')).toHaveCount(0);
  await page.locator('input[data-field=name]').press('Escape');
  await expect(page.locator('.webform')).toHaveCount(0);
  expect(sent().some((m) => m.type === 'form' && m.op === 'cancel')).toBe(true);
});

test('rename and edit repos are web forms too', async ({ page, isMobile }) => {
  test.skip(isMobile, 'the row menu by right-click');
  const sent = sentMessages(page);
  await page.goto('/');
  const row = page.locator('[data-id="web/better-loading"]');
  await row.click({ button: 'right' });
  await page.getByText('rename').click();
  const title = page.locator('input[data-field=title]');
  await expect(title).toBeFocused();
  await title.fill('Faster loading indicators');
  await title.press('Enter');
  await expect.poll(() => sent().some((m) => m.type === 'form' && m.op === 'set' && m.field === 'title' && m.value === 'Faster loading indicators')).toBe(true);
  await expect(title).toHaveCount(0);

  await row.click({ button: 'right' });
  await page.getByText('edit repos').click();
  const repoRow = (name: string) => page.locator('.wf-row', { hasText: name });
  await expect(repoRow('api')).toContainText('in task');
  await repoRow('web').click();
  await expect.poll(() => sent().some((m) => m.type === 'form' && m.op === 'check' && m.field === 'repo' && m.index === 1 && m.on === true)).toBe(true);
  await expect(repoRow('web')).toContainText('+ add');
  // Unchecking a repo the task has: detach, which asks in place.
  await repoRow('api').click();
  await expect(repoRow('api')).toContainText('− detach');
  await page.getByRole('button', { name: /apply/i }).click();
  const confirm = page.getByTestId('detach-confirm');
  await expect(confirm).toContainText('Detach api?');
  await confirm.getByRole('button', { name: /detach/i }).click();
  await expect.poll(() => sent().some((m) => m.type === 'key' && m.key === 'y')).toBe(true);
  await expect(page.locator('.webform')).toHaveCount(0);
});

test('a closed task under the cursor shows its empty screen and takes the marker', async ({ page, isMobile }) => {
  test.skip(isMobile, 'the column overlays the terminal on a phone');
  const sent = sentMessages(page);
  await page.goto('/');
  // Starts on the current window's task.
  await expect(page.locator('.row.task.cur')).toContainText('Fix login timeout');
  await expect(page.getByTestId('closed-screen')).toHaveCount(0);
  // Onto a closed task: its screen replaces the terminal, the marker moves.
  await page.getByText('Competitor analysis').click();
  await expect(page.getByTestId('closed-screen')).toContainText('Competitor analysis');
  await expect(page.getByTestId('closed-screen')).toContainText('open it here');
  await expect(page.locator('.row.task.cur')).toHaveCount(1);
  await expect(page.locator('.row.task.cur')).toContainText('Competitor analysis');
  // Back onto an open task: the terminal again, the marker on the window.
  await page.getByText('Better loading indicators').click();
  await expect(page.getByTestId('closed-screen')).toHaveCount(0);
  await expect(page.locator('.row.task.cur')).toContainText('Fix login timeout');
  // The empty screen's button opens the task.
  await page.getByText('Competitor analysis').click();
  await page.getByTestId('closed-screen').getByRole('button').click();
  await expect.poll(() => sent().some((m) => m.type === 'action' && m.name === 'open')).toBe(true);
});

test('the list scrolls to keep the selection in view', async ({ page, isMobile }) => {
  test.skip(isMobile, 'keyboard');
  await page.setViewportSize({ width: 1200, height: 560 });
  await page.goto('/');
  await expect(page.getByTestId('task')).toHaveCount(8);
  await page.locator('.xterm-helper-textarea').focus();
  await page.keyboard.press('Control+w');
  // Down to the last row, below the fold at this height.
  for (let i = 0; i < 8; i++) await page.keyboard.press('j');
  await expect(page.locator('.row.sel')).toContainText('Zero permission');
  const gap = () =>
    page.evaluate(() => {
      const list = document.querySelector('[data-testid=list]')!.getBoundingClientRect();
      const sel = document.querySelector('[data-testid=list] .sel')!.getBoundingClientRect();
      return Math.min(sel.top - list.top, list.bottom - sel.bottom);
    });
  await expect.poll(gap).toBeGreaterThanOrEqual(-1);
});

test('a pending credential request can be rejected, with a note for the agent', async ({ page, isMobile }) => {
  const sent = sentMessages(page);
  await page.goto('/');
  if (isMobile) await page.getByTestId('toggle').tap();
  const row = page.locator('[data-id="tenx/cloud-tasks"]');
  let presses = 20;
  // The row's menu (right-click), or on touch its sheet (long-press), then
  // "reject request…".
  const openReject = async () => {
    if (isMobile) {
      const b = (await row.boundingBox())!;
      const at = { pointerType: 'touch', pointerId: presses++, isPrimary: true, bubbles: true, clientX: b.x + 40, clientY: b.y + 10 };
      await row.dispatchEvent('pointerdown', at);
      await expect(page.getByTestId('sheet')).toBeVisible({ timeout: 2000 });
      await row.dispatchEvent('pointerup', at);
      await page.getByTestId('sheet').getByRole('menuitem', { name: /reject request/ }).tap();
    } else {
      await row.click({ button: 'right' });
      await page.getByTestId('menu').getByRole('menuitem', { name: /reject request/ }).click();
    }
  };
  await openReject();
  await expect.poll(() => sent().some((m) => m.type === 'action' && m.name === 'reject')).toBe(true);

  // The form lists every pending name with the agent's reason.
  const form = page.getByTestId('webform');
  await expect(form).toContainText('Reject credential request');
  const names = page.getByTestId('reject-names');
  await expect(names).toContainText('OPENAI_API_KEY');
  await expect(names).toContainText('run the embedding eval against the real API');
  await expect(names).toContainText('SENTRY_DSN');
  await expect(names).toContainText('no reason given');
  await page.screenshot({ path: `../../.playwright-mcp/reject-${isMobile ? 'phone' : 'desk'}.png` });

  // Cancel leaves the request alone.
  await form.getByTestId('form-buttons').getByRole('button', { name: /cancel/i }).click();
  await expect(form).toHaveCount(0);
  await expect(row).toBeVisible();

  // Again, with a note, and reject: the note goes over, then the submit.
  await openReject();
  const note = form.locator('input[data-field="note"]');
  await note.fill('use the test key in .env.example');
  await expect
    .poll(() => sent().some((m) => m.type === 'form' && m.op === 'set' && m.field === 'note' && m.value === 'use the test key in .env.example'))
    .toBe(true);
  await page.getByTestId('reject-submit').click();
  await expect.poll(() => sent().some((m) => m.type === 'form' && m.op === 'submit')).toBe(true);
  await expect(form).toHaveCount(0);
  await expect(page.getByTestId('footer')).toContainText('rejected OPENAI_API_KEY, SENTRY_DSN');
});

test('esc cancels the reject form', async ({ page, isMobile }) => {
  test.skip(isMobile, 'keyboard');
  const sent = sentMessages(page);
  await page.goto('/');
  await page.locator('[data-id="tenx/cloud-tasks"]').click({ button: 'right' });
  await page.getByTestId('menu').getByRole('menuitem', { name: /reject request/ }).click();
  await expect(page.getByTestId('webform')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('webform')).toHaveCount(0);
  expect(sent().some((m) => m.type === 'form' && m.op === 'cancel')).toBe(true);
});
