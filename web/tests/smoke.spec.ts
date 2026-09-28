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
  const buttons = page.locator('.form').getByTestId('form-buttons');
  await expect(buttons.getByRole('button', { name: /create/ })).toBeVisible();
  await buttons.getByRole('button', { name: /cancel/ }).click();
  await expect(page.locator('.form')).toHaveCount(0);
  expect(sent().some((m) => m.type === 'key' && m.key === 'Escape')).toBe(true);
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

test('the special keys only show with the on-screen keyboard up', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'touch screens only');
  await page.goto('/');
  const bar = page.getByTestId('keybar');
  // Headless has no on-screen keyboard: the floating esc / 📋 / 📎 only.
  await expect(bar).toHaveClass(/compact/);
  await expect(bar.getByRole('button', { name: 'esc' })).toBeVisible();
  await expect(bar.getByRole('button', { name: 'ctrl' })).toHaveCount(0);
  await expect(page.getByTestId('attach')).toBeVisible();
  // A keyboard taking a third of the screen: the full bar on top of it.
  await page.evaluate(() => {
    const vv = window.visualViewport!;
    Object.defineProperty(vv, 'height', { configurable: true, get: () => 500 });
    vv.dispatchEvent(new Event('resize'));
  });
  await expect(bar).not.toHaveClass(/compact/);
  await expect(bar.getByRole('button', { name: 'ctrl' })).toBeVisible();
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
