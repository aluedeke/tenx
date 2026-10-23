import { devices, expect, test } from '@playwright/test';

test('the column renders the view and the terminal mounts', async ({ page, isMobile }) => {
  await page.goto('/');
  await expect(page.locator('.xterm')).toBeVisible();
  // A phone starts on the terminal; the header's toggle opens the column.
  if (isMobile) await page.getByTestId('toggle').tap();
  if (!isMobile) await expect(page.getByTestId('conn-dot')).toBeVisible();
  await (isMobile ? page.getByTestId('hmore').tap() : page.getByTestId('hmore').click());
  await expect(page.getByTestId('conn')).toContainText('connected · mock');
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('hmenu')).toHaveCount(0);
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

  // keys, in the header's ⋯ menu, opens them; a tap closes them.
  await tap(page.getByTestId('hmore'));
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

/** The `+` menu's "new task" entry (its other one is the quick task). */
const newTaskItem = (page: import('@playwright/test').Page) => page.getByRole('menuitem', { name: /^new task/ });

test('+ offers the quick task: only a prompt, ⏎ starts it', async ({ page, isMobile }) => {
  const sent = sentMessages(page);
  const tap = (loc: import('@playwright/test').Locator) => (isMobile ? loc.tap() : loc.click());
  await page.goto('/');
  if (isMobile) await page.getByTestId('toggle').tap();
  await tap(page.getByTestId('add'));
  await tap(page.getByRole('menuitem', { name: /^quick task/ }));
  await expect.poll(() => sent().some((m) => m.type === 'action' && m.name === 'quick')).toBe(true);
  const prompt = page.locator('textarea[data-field=prompt]');
  await expect(prompt).toBeVisible();
  await expect(page.locator('input[data-field=name]')).toHaveCount(0);
  if (isMobile) return;
  await expect(prompt).toBeFocused();
  await prompt.pressSequentially('fix the flaky test');
  await prompt.press('Shift+Enter');
  await prompt.pressSequentially('then push');
  await expect
    .poll(() => sent().some((m) => m.type === 'form' && m.op === 'set' && m.field === 'prompt' && m.value === 'fix the flaky test\nthen push'))
    .toBe(true);
  await prompt.press('Enter');
  await expect.poll(() => sent().some((m) => m.type === 'form' && m.op === 'submit')).toBe(true);
});

test('forms carry their own buttons; + opens the new-task form', async ({ page, isMobile }) => {
  const sent = sentMessages(page);
  await page.goto('/');
  if (isMobile) await page.getByTestId('toggle').tap();
  await (isMobile ? page.getByTestId('add').tap() : page.getByTestId('add').click());
  await (isMobile ? newTaskItem(page).tap() : newTaskItem(page).click());
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
  await (isMobile ? page.getByTestId('hmore').tap() : page.getByTestId('hmore').click());
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
  await phone.getByTestId('hmore').tap();
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

test('on an iPad the key bar shows while the terminal has the focus', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'touch screens only');
  // iPadOS Safari: a "Mac" with a touch screen.
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'platform', { get: () => 'MacIntel' });
    Object.defineProperty(navigator, 'maxTouchPoints', { get: () => 5 });
  });
  await page.goto('/');
  const bar = page.getByTestId('keybar');
  const textarea = page.locator('.xterm-helper-textarea');
  await textarea.blur();
  await expect(bar).toHaveCount(0);
  // A hardware keyboard: the terminal has the focus, no on-screen keyboard.
  await textarea.focus();
  await expect(bar.getByTestId('attach')).toBeVisible();
  await expect(bar.getByTestId('paste')).toBeVisible();
  await expect(bar.getByRole('button', { name: 'esc' })).toBeVisible();
  await textarea.blur();
  await expect(bar).toHaveCount(0);
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

  // + and ⋯ → keys are the new-task and keys actions.
  await tap(header.getByTestId('hmore'));
  await tap(page.getByTestId('keys'));
  await expect.poll(() => sent().some((m) => m.type === 'action' && m.name === 'help')).toBe(true);
  await tap(page.getByTestId('help'));
  await tap(header.getByTestId('add'));
  await tap(newTaskItem(page));
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
  await tap(newTaskItem(page));
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
    await expect(page.locator('textarea[data-field=prompt]')).toBeFocused();
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
  await newTaskItem(page).click();
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
  await newTaskItem(page).click();
  await page.locator('input[data-field=name]').press('Enter');
  // Under the field it is about, which turns red (3a).
  await expect(page.getByTestId('form-error')).toHaveText('type a name or a prompt');
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

test('a secrets notification opens the unlock prompt', async ({ page, isMobile }) => {
  const sent = sentMessages(page);
  await page.goto('/?task=tenx/cloud-tasks&unlock=1');
  await expect.poll(() => sent().some((m) => m.type === 'action' && m.name === 'unlock')).toBe(true);
  expect(sent().some((m) => m.type === 'click' && m.kind === 'task' && m.id === 'tenx/cloud-tasks')).toBe(true);
  expect(sent().some((m) => m.type === 'action' && m.name === 'open')).toBe(false);
  expect(new URL(page.url()).search).toBe('');
  // A phone can only raise its keyboard from a tap: it's offered one.
  if (isMobile) {
    const prompt = page.getByText('tap here to answer');
    await expect(prompt).toBeVisible();
    await prompt.tap();
    await expect(page.locator('.xterm-helper-textarea')).toBeFocused();
  }
});

test('unlock from the row menu hands the keyboard to the terminal in the same tap', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'the tap is what matters on a phone');
  await page.goto('/');
  await page.getByTestId('toggle').tap();
  const row = page.locator('[data-id="tenx/cloud-tasks"]');
  const b = (await row.boundingBox())!;
  const at = { pointerType: 'touch', pointerId: 91, isPrimary: true, bubbles: true, clientX: b.x + 40, clientY: b.y + 10 };
  await row.dispatchEvent('pointerdown', at);
  await expect(page.getByTestId('sheet')).toBeVisible({ timeout: 2000 });
  await row.dispatchEvent('pointerup', at);
  await page.getByTestId('sheet').getByRole('menuitem', { name: /unlock/ }).tap();
  await expect(page.locator('.xterm-helper-textarea')).toBeFocused();
});

test('a link in the terminal opens outside the app', async ({ page, isMobile }) => {
  test.skip(isMobile, 'hover + click');
  await page.addInitScript(() => {
    (window as unknown as { opened: unknown[] }).opened = [];
    window.open = ((url: string, target: string, features: string) => {
      (window as unknown as { opened: unknown[] }).opened.push([url, target, features]);
      return null;
    }) as typeof window.open;
  });
  await page.goto('/?renderer=dom');
  const line = page.locator('.xterm-rows > div', { hasText: 'https://github.com/aluedeke/tenx/pull/42' });
  await expect(line).toBeVisible();
  const box = (await line.boundingBox())!;
  await page.mouse.move(box.x + 30, box.y + box.height / 2);
  await page.waitForTimeout(300);
  await page.mouse.click(box.x + 30, box.y + box.height / 2);
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { opened: unknown[] }).opened))
    .toEqual([['https://github.com/aluedeke/tenx/pull/42', '_blank', 'noopener,noreferrer']]);
});

test('a paste while the column has the keyboard goes to the terminal, not to the column as keys', async ({ page, isMobile }) => {
  test.skip(isMobile, 'same code path');
  const sent = sentMessages(page);
  const term = terminalInput(page);
  await page.goto('/?renderer=dom');
  await expect(page.locator('.xterm-rows')).toBeVisible();
  // The column's hidden input has the keyboard (as on a touch screen).
  await page.locator('.kbd-sink').focus();
  await page.evaluate(() => {
    const sink = document.querySelector('.kbd-sink') as HTMLInputElement;
    const dt = new DataTransfer();
    dt.setData('text/plain', 'Deny xdd:q');
    sink.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
  });
  await expect.poll(term.raw).toContain('Deny xdd:q');
  // Announced first, so the server takes the pane out of copy-mode.
  expect(sent().some((m) => m.type === 'paste')).toBe(true);
  // Not one column key: `D` would reject secrets, `x` close a window.
  expect(sent().filter((m) => m.type === 'key')).toEqual([]);
  await expect(page.locator('.xterm-helper-textarea')).toBeFocused();
});

test('a paste into the terminal is announced before it is sent', async ({ page, isMobile }) => {
  test.skip(isMobile, 'same code path');
  const order: string[] = [];
  page.on('websocket', (ws) => {
    if (!ws.url().includes('/speech'))
      ws.on('framesent', (f) => order.push(typeof f.payload === 'string' ? JSON.parse(f.payload).type : `bytes:${f.payload.toString()}`));
  });
  await page.goto('/?renderer=dom');
  await expect(page.locator('.xterm-rows')).toBeVisible();
  await page.locator('.xterm-helper-textarea').focus();
  await page.evaluate(() => {
    const ta = document.querySelector('.xterm-helper-textarea') as HTMLTextAreaElement;
    const dt = new DataTransfer();
    dt.setData('text/plain', 'from-the-menu');
    ta.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
  });
  await expect.poll(() => order.findIndex((o) => o.includes('from-the-menu'))).toBeGreaterThan(-1);
  const bytes = order.findIndex((o) => o.includes('from-the-menu'));
  expect(order.slice(0, bytes)).toContain('paste');
});

test('a paste into the terminal is typed once', async ({ page, isMobile }) => {
  test.skip(isMobile, 'same code path; the DOM renderer check is desktop');
  const term = terminalInput(page);
  await page.goto('/?renderer=dom');
  await expect(page.locator('.xterm-rows')).toBeVisible();
  await page.locator('.xterm-helper-textarea').focus();
  // What a browser does on a paste with no key press (the iPad's Paste
  // menu): a paste event, then the text lands in the field as an input.
  await page.evaluate(() => {
    const ta = document.querySelector('.xterm-helper-textarea') as HTMLTextAreaElement;
    const dt = new DataTransfer();
    dt.setData('text/plain', 'pasted-once');
    ta.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
    ta.dispatchEvent(new InputEvent('beforeinput', { inputType: 'insertFromPaste', data: 'pasted-once', bubbles: true, cancelable: true }));
    ta.value += 'pasted-once';
    ta.dispatchEvent(new InputEvent('input', { inputType: 'insertFromPaste', data: 'pasted-once', bubbles: true }));
  });
  await page.waitForTimeout(500);
  const sent = term.raw();
  expect(sent.split('pasted-once').length - 1).toBe(1);
});

test('dictation sends each revision as an edit, not a repeat', async ({ page, isMobile }) => {
  test.skip(isMobile, 'same code path');
  const term = terminalInput(page);
  await page.goto('/?renderer=dom');
  await expect(page.locator('.xterm-rows')).toBeVisible();
  await page.locator('.xterm-helper-textarea').focus();
  await page.evaluate(async () => {
    const ta = document.querySelector('.xterm-helper-textarea') as HTMLTextAreaElement;
    const say = (text: string) => {
      ta.dispatchEvent(new InputEvent('beforeinput', { inputType: 'insertText', data: text, bubbles: true, cancelable: true }));
      ta.value = text;
      ta.dispatchEvent(new InputEvent('input', { inputType: 'insertText', data: text, bubbles: true }));
    };
    say('Fix');
    await new Promise((r) => setTimeout(r, 150));
    say('Fix the log in');
    await new Promise((r) => setTimeout(r, 150));
    say('Fix the login timeout');
  });
  await page.waitForTimeout(500);
  expect(term.raw()).toBe('Fix' + ' the log in' + '\x7f\x7f\x7f' + 'in timeout');
});

// A slow connection: every view the mock sends arrives this late.
async function slowLink(page: import('@playwright/test').Page, ms: number, extra: Record<string, string> = {}) {
  const cookies = Object.entries({ mocklag: String(ms), ...extra }).map(([name, value]) => ({ name, value, url: 'http://127.0.0.1:7071' }));
  await page.context().addCookies(cookies);
}

test('on a slow link the cursor moves at once, and the server agrees later', async ({ page, isMobile }) => {
  test.skip(isMobile, 'keyboard');
  await slowLink(page, 600);
  await page.goto('/');
  const selected = page.locator('.row.sel');
  await expect(selected).toContainText('Fix login timeout', { timeout: 5000 });
  const t0 = Date.now();
  await page.keyboard.press('j');
  await expect(selected).toContainText('Better loading indicators', { timeout: 150 });
  expect(Date.now() - t0).toBeLessThan(400);
  // The server's own answer (600 ms later) is the same row: no flicker back.
  await page.waitForTimeout(900);
  await expect(selected).toContainText('Better loading indicators');
});

test('fast presses on a slow link end on the right row without jumping back', async ({ page, isMobile }) => {
  test.skip(isMobile, 'keyboard');
  await slowLink(page, 500);
  await page.goto('/');
  const selected = page.locator('.row.sel');
  await expect(selected).toContainText('Fix login timeout', { timeout: 5000 });
  for (let i = 0; i < 3; i++) await page.keyboard.press('j');
  // Fix login → Better loading → web version → its first subagent.
  await expect(selected).toContainText('Map column keymap', { timeout: 150 });
  // Sample while the delayed views come in: it never shows an earlier row.
  const seen = new Set<string>();
  for (let i = 0; i < 16; i++) {
    seen.add(((await selected.first().textContent()) ?? '').slice(0, 24));
    await page.waitForTimeout(75);
  }
  expect([...seen].every((s) => s.includes('Map column keymap'))).toBe(true);
});

test('a wrong prediction is corrected by the server’s view', async ({ page, isMobile }) => {
  test.skip(isMobile, 'keyboard');
  // This mock moves two rows on j; the page predicts one.
  await slowLink(page, 300, { mockskip: '1' });
  await page.goto('/');
  const selected = page.locator('.row.sel');
  await expect(selected).toContainText('Fix login timeout', { timeout: 5000 });
  await page.keyboard.press('j');
  await expect(selected).toContainText('Better loading indicators', { timeout: 150 });
  await expect(selected).toContainText('web version', { timeout: 2000 });
});

test('a workspace without repos creates a task the agent runs on its own', async ({ page, isMobile }) => {
  const sent = sentMessages(page);
  const tap = (loc: import('@playwright/test').Locator) => (isMobile ? loc.tap() : loc.click());
  await page.goto('/');
  if (isMobile) await page.getByTestId('toggle').tap();
  await tap(page.getByTestId('add'));
  await tap(newTaskItem(page));
  await tap(page.locator('.wf-chip', { hasText: /^\W*adhoc/ }));
  await expect(page.getByText('none — the agent runs on its own and can read the workspace')).toBeVisible();
  await page.locator('input[data-field=name]').fill('how does sweep work');
  await tap(page.getByRole('button', { name: /Create task/ }));
  await expect.poll(() => sent().some((m) => m.type === 'form' && m.op === 'submit')).toBe(true);
});

for (const [name, parts, expected] of [
  ['plain text', { 'text/plain': 'npm run build' }, 'npm run build'],
  ['HTML only (a page selection)', { 'text/html': '<p>Fix <b>login</b> timeout</p>' }, 'Fix login timeout'],
] as const) {
  test(`the paste key pastes ${name}`, async ({ page, context, isMobile }) => {
    test.skip(!isMobile, 'the key bar is for touch');
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    const binary: string[] = [];
    page.on('websocket', (ws) => ws.on('framesent', (f) => typeof f.payload !== 'string' && binary.push(f.payload.toString())));
    await page.goto('/');
    await page.locator('.xterm-helper-textarea').focus();
    await page.evaluate(async (parts) => {
      const blobs = Object.fromEntries(Object.entries(parts).map(([t, v]) => [t, new Blob([v], { type: t })]));
      await navigator.clipboard.write([new ClipboardItem(blobs)]);
      const vv = window.visualViewport!;
      Object.defineProperty(vv, 'height', { configurable: true, get: () => 500 });
      vv.dispatchEvent(new Event('resize'));
    }, parts as Record<string, string>);
    await page.getByTestId('paste').tap();
    await expect.poll(() => binary.join('')).toContain(expected);
    // Pasted on the finger lifting, and not again by the click after it.
    await page.waitForTimeout(400);
    expect(binary.join('').split(expected).length - 1).toBe(1);
  });
}

test('the clipboard keys copy text to the Mac and back', async ({ page, context, isMobile }) => {
  test.skip(!isMobile, 'the key bar is for touch');
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto('/');
  await page.locator('.xterm-helper-textarea').focus();
  await page.evaluate(() => {
    const vv = window.visualViewport!;
    Object.defineProperty(vv, 'height', { configurable: true, get: () => 500 });
    vv.dispatchEvent(new Event('resize'));
  });
  const bar = page.getByTestId('keybar');
  // The approve/deny keys are gone; the column still has its own.
  await expect(bar.getByRole('button', { name: 'A', exact: true })).toHaveCount(0);
  await expect(bar.getByRole('button', { name: 'D', exact: true })).toHaveCount(0);
  // The Mac's clipboard onto the phone's.
  await bar.getByTestId('from-mac').tap();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('from the mac ✓');
  // The phone's onto the Mac's, and back again.
  await page.evaluate(() => navigator.clipboard.writeText('from the phone'));
  await bar.getByTestId('to-mac').tap();
  await expect(page.getByText('copied to the Mac')).toBeVisible();
  await page.evaluate(() => navigator.clipboard.writeText(''));
  await bar.getByTestId('from-mac').tap();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('from the phone');
});

test('a dead key (US International) types its composed character once', async ({ page, isMobile }) => {
  test.skip(isMobile, 'a hardware keyboard layout');
  const term = terminalInput(page);
  await page.goto('/?renderer=dom');
  await expect(page.locator('.xterm-rows')).toBeVisible();
  await page.locator('.xterm-helper-textarea').focus();
  // Chrome on macOS: the dead key starts a composition holding the accent;
  // the next key arrives as a keydown with its real keyCode (not 229) while
  // still composing, and the composition then ends with the result.
  const compose = async (accent: string, dead: number, next: string, nextCode: number, result: string) =>
    page.evaluate(
      async ([accent, dead, next, nextCode, result]) => {
        const ta = document.querySelector('.xterm-helper-textarea') as HTMLTextAreaElement;
        const tick = () => new Promise((r) => setTimeout(r, 20));
        const key = (key: string, keyCode: number, isComposing: boolean) =>
          ta.dispatchEvent(new KeyboardEvent('keydown', { key, keyCode, isComposing, bubbles: true, cancelable: true } as KeyboardEventInit));
        const update = (data: string) => {
          ta.dispatchEvent(new CompositionEvent('compositionupdate', { data, bubbles: true }));
          ta.dispatchEvent(new InputEvent('beforeinput', { inputType: 'insertCompositionText', data, isComposing: true, bubbles: true }));
          ta.value = data;
          ta.dispatchEvent(new InputEvent('input', { inputType: 'insertCompositionText', data, isComposing: true, bubbles: true }));
        };
        ta.value = '';
        key('Dead', dead as number, false);
        ta.dispatchEvent(new CompositionEvent('compositionstart', { data: '', bubbles: true }));
        update(accent as string);
        await tick();
        key(next as string, nextCode as number, true);
        update(result as string);
        ta.dispatchEvent(new CompositionEvent('compositionend', { data: result as string, bubbles: true }));
        await tick();
      },
      [accent, dead, next, nextCode, result] as const,
    );
  await compose('"', 222, 'ö', 79, 'ö');
  await compose("'", 222, ' ', 32, "'");
  await page.waitForTimeout(300);
  expect(term.raw()).toBe("ö'");
});

test('a tmux copy (OSC 52 with no target) reaches the browser clipboard', async ({ page, context, isMobile }) => {
  test.skip(isMobile, 'same code path');
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto('/');
  await expect(page.locator('.xterm')).toBeVisible();
  await page.locator('.xterm-helper-textarea').focus();
  // The mock echoes what is typed, so the terminal receives what tmux sends
  // when a selection is copied: ESC ]52;;<base64> BEL, the target left empty.
  await page.keyboard.press('Escape');
  await page.keyboard.insertText(`]52;;${Buffer.from('from nvim').toString('base64')}`);
  await page.keyboard.press('Control+g');
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('from nvim');
});

test('the speaker reads the current task’s last reply aloud, piece by piece; a tap pauses, the next goes on, ■ stops', async ({ page }) => {
  const asked: string[] = [];
  const spoken: string[] = [];
  page.on('request', (r) => {
    const url = new URL(r.url());
    if (url.pathname === '/speak') asked.push(url.searchParams.get('task') ?? '');
    if (url.pathname === '/speak/audio') spoken.push(r.postData() ?? '');
  });
  await page.goto('/');
  const speaker = page.getByTestId('speaker');
  await expect(speaker).toHaveAttribute('data-state', 'idle');
  await speaker.click();
  // The mock's reply is two pieces of half a second each: they play one
  // after the other, then the button is off again.
  await expect(speaker).toHaveAttribute('data-state', 'playing');
  await expect(speaker).toHaveAttribute('data-state', 'idle');
  expect(asked).toEqual(['acme/fix-login-timeout']);
  expect(spoken).toEqual(['First piece.', 'Second piece.']);

  // A long piece: stopped by the next tap. The second piece was already
  // asked for while the first played; nothing after it is.
  spoken.length = 0;
  await page.route('**/speak/audio', async (route) => {
    const res = await route.fetch();
    const wav = await res.body();
    // The same silence, with a header claiming far more of it.
    const long = Buffer.concat([wav, Buffer.alloc(8000 * 2 * 30)]);
    long.writeUInt32LE(long.length - 8, 4);
    long.writeUInt32LE(long.length - 44, 40);
    await route.fulfill({ status: 200, contentType: 'audio/wav', body: long });
  });
  await speaker.click();
  await expect(speaker).toHaveAttribute('data-state', 'playing');
  // A tap pauses, the next goes on, ■ stops.
  await speaker.click();
  await expect(speaker).toHaveAttribute('data-state', 'paused');
  await speaker.click();
  await expect(speaker).toHaveAttribute('data-state', 'playing');
  await expect(page.getByTestId('mic')).toHaveCount(0);
  await page.getByTestId('speaker-stop').click();
  await expect(speaker).toHaveAttribute('data-state', 'idle');
  await expect(page.getByTestId('speaker-stop')).toHaveCount(0);
  await expect(page.getByTestId('mic')).toBeVisible();
  await page.waitForTimeout(300);
  expect(spoken).toEqual(['First piece.', 'Second piece.']);
});

test('Ctrl+. reads aloud, pauses and goes on, from the terminal and the column', async ({ page, isMobile }) => {
  test.skip(isMobile, 'a hardware keyboard');
  await page.route('**/speak/audio', async (route) => {
    const wav = await (await route.fetch()).body();
    const long = Buffer.concat([wav, Buffer.alloc(8000 * 2 * 30)]);
    long.writeUInt32LE(long.length - 8, 4);
    long.writeUInt32LE(long.length - 44, 40);
    await route.fulfill({ status: 200, contentType: 'audio/wav', body: long });
  });
  const sent = sentMessages(page);
  await page.goto('/');
  const speaker = page.getByTestId('speaker');
  await page.locator('.xterm').click();
  await page.keyboard.press('Control+Period');
  await expect(speaker).toHaveAttribute('data-state', 'playing');
  await page.keyboard.press('Control+Period');
  await expect(speaker).toHaveAttribute('data-state', 'paused');
  await page.keyboard.press('Control+w'); // the column has the keys now
  await page.keyboard.press('Control+Period');
  await expect(speaker).toHaveAttribute('data-state', 'playing');
  await page.getByTestId('speaker-stop').click();
  await expect(speaker).toHaveAttribute('data-state', 'idle');
  // Neither the terminal nor the column saw the key.
  expect(sent().some((m) => m.type === 'key' && (m.key === '.' || m.key === 'Period'))).toBe(false);
});

test('on the smallest phone the header keeps every button on screen, also while reading', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'a phone');
  await page.setViewportSize({ width: 320, height: 640 });
  await page.route('**/speak?*', (route) => route.fulfill({ status: 200, contentType: 'application/json', body: '{"pieces":["One."]}' }));
  await page.route('**/speak/audio', async (route) => {
    const wav = await (await route.fetch()).body();
    const long = Buffer.concat([wav, Buffer.alloc(8000 * 2 * 30)]);
    long.writeUInt32LE(long.length - 8, 4);
    long.writeUInt32LE(long.length - 44, 40);
    await route.fulfill({ status: 200, contentType: 'audio/wav', body: long });
  });
  // In another task than the one that needs you: ● next shows too.
  await page.goto('/?task=web/better-loading');
  const header = page.getByTestId('header');
  await expect(header.getByTestId('next')).toBeVisible();
  const offScreen = () =>
    header.evaluate((h) =>
      [...h.querySelectorAll('button')]
        .filter((b) => (b as HTMLElement).offsetParent)
        .filter((b) => b.getBoundingClientRect().right > window.innerWidth + 0.5 || b.getBoundingClientRect().left < 0)
        .map((b) => (b as HTMLElement).dataset.testid),
    );
  expect(await offScreen()).toEqual([]);
  await page.getByTestId('speaker').tap();
  await expect(page.getByTestId('speaker')).toHaveAttribute('data-state', 'playing');
  // ■ takes the microphone's place rather than adding a button.
  await expect(header.getByTestId('speaker-stop')).toBeVisible();
  await expect(header.getByTestId('mic')).toHaveCount(0);
  expect(await offScreen()).toEqual([]);
  await header.getByTestId('speaker-stop').tap();
  await expect(header.getByTestId('mic')).toBeVisible();
});

test('the speaker says why a piece could not be read', async ({ page }) => {
  await page.route('**/speak/audio', (route) => route.fulfill({ status: 502, contentType: 'text/plain', body: 'no voice model: set [read_aloud] model' }));
  await page.goto('/');
  const speaker = page.getByTestId('speaker');
  await speaker.click();
  await expect(page.getByRole('status')).toContainText('read aloud: no voice model');
  await expect(speaker).toHaveAttribute('data-state', 'idle');
});

test('the speaker says why there is nothing to read', async ({ page }) => {
  await page.route('**/speak?*', (route) => route.fulfill({ status: 422, contentType: 'text/plain', body: 'the agent has not replied since the last prompt' }));
  await page.goto('/');
  const speaker = page.getByTestId('speaker');
  await speaker.click();
  await expect(page.getByRole('status')).toContainText('read aloud: the agent has not replied since the last prompt');
  await expect(speaker).toHaveAttribute('data-state', 'idle');
});

test('the microphone says why it cannot listen, and stays off', async ({ page }) => {
  await page.addInitScript(() => {
    navigator.mediaDevices.getUserMedia = () => Promise.reject(new DOMException('denied', 'NotAllowedError'));
  });
  await page.goto('/');
  const mic = page.getByTestId('mic');
  await expect(mic).toHaveAttribute('data-state', 'idle');
  await mic.click();
  await expect(page.getByRole('status')).toContainText('no microphone');
  await expect(mic).toHaveAttribute('data-state', 'idle');
});

test('on touch, press and hold then drag selects like a mouse', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'touch screens only');
  const term = terminalInput(page);
  await page.goto('/?renderer=dom');
  const screen = page.locator('.xterm-screen');
  await expect(screen).toBeVisible();
  const b = (await screen.boundingBox())!;
  const touch = (x: number, y: number) => ({ pointerType: 'touch', pointerId: 31, isPrimary: true, bubbles: true, clientX: b.x + x, clientY: b.y + y });
  const host = page.getByTestId('terminal');
  await host.dispatchEvent('pointerdown', touch(5, 5));
  await page.waitForTimeout(550);
  await host.dispatchEvent('pointermove', touch(b.width / 2, 5));
  await host.dispatchEvent('pointerup', touch(b.width / 2, 5));
  // The left button pressed at the first cell, dragged, released: what tmux
  // gets from a mouse drag. No wheel notches (a scroll), no arrows (a tap).
  await expect.poll(() => term.raw()).toMatch(/\x1b\[<0;1;1M(\x1b\[<32;\d+;1M)+\x1b\[<0;\d+;1m/);
  expect(term.raw()).not.toMatch(/\x1b\[<6[45];|\x1b\[[ABCD]|\x1bO[ABCD]/);
});

test('on touch, a quick drag still scrolls and does not select', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'touch screens only');
  const term = terminalInput(page);
  await page.goto('/?renderer=dom');
  const screen = page.locator('.xterm-screen');
  await expect(screen).toBeVisible();
  const b = (await screen.boundingBox())!;
  const touch = (y: number) => ({ pointerType: 'touch', pointerId: 32, isPrimary: true, bubbles: true, clientX: b.x + 20, clientY: b.y + y });
  const host = page.getByTestId('terminal');
  await host.dispatchEvent('pointerdown', touch(20));
  for (let y = 40; y <= 200; y += 20) await host.dispatchEvent('pointermove', touch(y));
  await page.waitForTimeout(550);
  await host.dispatchEvent('pointerup', touch(200));
  await expect.poll(() => term.raw()).toContain('\x1b[<64;');
  expect(term.raw()).not.toContain('\x1b[<0;');
});

/** What the page types into the terminal (its `/ws`; not the audio it
 * streams to `/speech`): `raw`, and `line` as a terminal shows it — a DEL
 * erases the character before. */
function terminalInput(page: import('@playwright/test').Page) {
  let typed: string[] = [];
  page.on('websocket', (ws) => {
    if (!ws.url().includes('/speech')) ws.on('framesent', (f) => typeof f.payload !== 'string' && typed.push(f.payload.toString()));
  });
  const raw = () => typed.join('');
  return {
    raw,
    line: () => raw().split('').reduce((l, c) => (c === '\x7f' ? l.slice(0, -1) : l + c), ''),
    reset: () => (typed = []),
  };
}

/** A tone stands in for a voice: loud enough to count as speech. */
async function fakeMicrophone(page: import('@playwright/test').Page, language: string) {
  await page.addInitScript((language) => {
    localStorage.setItem('tenx.stt.language', language);
    navigator.mediaDevices.getUserMedia = async () => {
      const ctx = new AudioContext();
      const osc = ctx.createOscillator();
      const out = ctx.createMediaStreamDestination();
      osc.connect(out);
      osc.start();
      return out.stream;
    };
  }, language);
}

test('speech streams to the server and is typed at the cursor as it is heard', async ({ page }) => {
  await fakeMicrophone(page, 'de');
  const term = terminalInput(page);
  // The audio it streams to `/speech`.
  let audioBytes = 0;
  let speechUrl = '';
  page.on('websocket', (ws) => {
    if (!ws.url().includes('/speech')) return;
    speechUrl = ws.url();
    ws.on('framesent', (f) => typeof f.payload !== 'string' && (audioBytes += f.payload.length));
  });
  await page.goto('/');
  const mic = page.getByTestId('mic');
  await mic.click();
  await expect(mic).toHaveAttribute('data-state', 'recording');
  // Live: the text so far is typed while recording, revised in place.
  await expect.poll(term.line).toMatch(/^heard \d+ bytes$/);
  await expect.poll(term.raw).toContain('\x7f');
  await page.waitForTimeout(600);
  await mic.click();
  await expect(mic).toHaveAttribute('data-state', 'idle');
  expect(speechUrl).toContain('language=de');
  // 16 kHz 16-bit: about 32 kB a second.
  expect(audioBytes).toBeGreaterThan(16_000);
  // On stop, the final text replaces it; no Enter.
  await expect.poll(term.line).toBe('heard de ');
  expect(term.raw()).not.toContain('\r');
});

test('with a task form open, speech goes into its prompt; ⏎ stops and submits', async ({ page, isMobile }) => {
  test.skip(isMobile, 'the header + is in the column overlay on a phone');
  await fakeMicrophone(page, 'de');
  const term = terminalInput(page);
  const sent = sentMessages(page);
  await page.goto('/');
  await page.getByTestId('add').click();
  await page.getByRole('menuitem', { name: /^quick task/ }).click();
  const prompt = page.locator('textarea[data-field=prompt]');
  await prompt.pressSequentially('fix');
  term.reset();
  const mic = page.getByTestId('mic');
  await mic.click();
  await expect(mic).toHaveAttribute('data-state', 'recording');
  // Live, after what was typed, and the server's form state follows.
  await expect.poll(() => prompt.inputValue()).toMatch(/^fix heard \d+ bytes$/);
  await expect
    .poll(() => sent().some((m) => m.type === 'form' && m.op === 'set' && m.field === 'prompt' && /^fix heard \d+ bytes$/.test(String(m.value))))
    .toBe(true);
  await prompt.focus();
  await prompt.press('Enter');
  await expect(mic).toHaveAttribute('data-state', 'idle');
  await expect.poll(() => sent().some((m) => m.type === 'form' && m.op === 'set' && m.field === 'prompt' && m.value === 'fix heard de')).toBe(true);
  await expect.poll(() => sent().some((m) => m.type === 'form' && m.op === 'submit')).toBe(true);
  // Nothing went to the terminal.
  expect(term.raw()).toBe('');
});

test('a chime says when the microphone records, and is not sent as speech', async ({ page }) => {
  await fakeMicrophone(page, 'de');
  // The tones the page plays: the fake microphone's own is 440 Hz.
  await page.addInitScript(() => {
    const w = window as unknown as { tones: number[] };
    w.tones = [];
    // Safari's Audio Session API, which Chromium lacks.
    Object.defineProperty(navigator, 'audioSession', { value: { type: 'auto' } });
    const start = OscillatorNode.prototype.start;
    OscillatorNode.prototype.start = function (...args) {
      w.tones.push(this.frequency.value);
      return start.apply(this, args);
    };
  });
  let firstAudio = 0;
  page.on('websocket', (ws) => {
    if (!ws.url().includes('/speech')) return;
    ws.on('framesent', (f) => typeof f.payload !== 'string' && (firstAudio ||= Date.now()));
  });
  await page.goto('/');
  const tones = () => page.evaluate(() => (window as unknown as { tones: number[] }).tones.filter((hz) => hz !== 440));
  expect(await tones()).toEqual([]);
  const mic = page.getByTestId('mic');
  const tapped = Date.now();
  await mic.click();
  await expect(mic).toHaveAttribute('data-state', 'recording');
  await expect.poll(tones).toEqual([523, 784]);
  const sessionType = () => page.evaluate(() => (navigator as unknown as { audioSession: { type: string } }).audioSession.type);
  // So iOS plays the chime on the speaker while the microphone is on.
  expect(await sessionType()).toBe('play-and-record');
  // What the microphone heard during the chime is dropped.
  await expect.poll(() => firstAudio).toBeGreaterThan(0);
  expect(firstAudio - tapped).toBeGreaterThan(200);
  await mic.click();
  await expect(mic).toHaveAttribute('data-state', 'idle');
  expect(await sessionType()).toBe('auto');
});

test('an error from the server before the microphone is ready is shown, and the microphone stays off', async ({ page }) => {
  await fakeMicrophone(page, 'nomodel');
  await page.goto('/');
  const mic = page.getByTestId('mic');
  await mic.click();
  await expect(page.getByRole('status')).toContainText('no speech model set');
  await expect(mic).toHaveAttribute('data-state', 'idle');
});

test('when the server gives up mid-recording, the microphone goes off and says why', async ({ page }) => {
  await fakeMicrophone(page, 'fail');
  await page.goto('/');
  const mic = page.getByTestId('mic');
  await mic.click();
  await expect(page.getByRole('status').filter({ hasText: 'model went away' })).toBeVisible();
  await expect(mic).toHaveAttribute('data-state', 'idle');
});

test('Ctrl+M starts and stops the microphone from the terminal and the column', async ({ page, isMobile }) => {
  test.skip(isMobile, 'keyboard');
  await page.addInitScript(() => {
    navigator.mediaDevices.getUserMedia = async () => {
      const ctx = new AudioContext();
      const osc = ctx.createOscillator();
      const out = ctx.createMediaStreamDestination();
      osc.connect(out);
      osc.start();
      return out.stream;
    };
  });
  const term = terminalInput(page);
  await page.goto('/');
  const mic = page.getByTestId('mic');
  await expect(page.getByTestId('task')).toHaveCount(8);

  // The column has the keyboard.
  await page.keyboard.press('Control+KeyM');
  await expect(mic).toHaveAttribute('data-state', 'recording');
  await expect.poll(term.line).toMatch(/heard \d+ bytes$/);
  await page.keyboard.press('Control+KeyM');
  await expect.poll(term.line).toMatch(/heard default $/);

  // Typing the text moved the keyboard to the terminal; there the key
  // never reaches the shell as Enter (Ctrl+M's byte).
  await expect(page.locator('.xterm-helper-textarea')).toBeFocused();
  term.reset();
  await page.keyboard.press('Control+KeyM');
  await expect(mic).toHaveAttribute('data-state', 'recording');
  await expect.poll(term.line).toMatch(/heard \d+ bytes$/);
  await page.keyboard.press('Control+KeyM');
  await expect.poll(term.line).toMatch(/heard default $/);
  expect(term.raw()).not.toContain('\r');

  // Enter while recording: stop, type the text, then send it.
  term.reset();
  await page.keyboard.press('Control+KeyM');
  await expect(mic).toHaveAttribute('data-state', 'recording');
  await expect.poll(term.line).toMatch(/heard \d+ bytes$/);
  await page.keyboard.press('Enter');
  // The line as the terminal shows it: the live text corrected, no space
  // before the Enter.
  await expect.poll(term.line).toMatch(/heard default\r$/);
  await expect(mic).toHaveAttribute('data-state', 'idle');
  expect(term.raw().split('\r').length - 1).toBe(1);
});

test('on a phone, the key bar has the microphone and it keeps the keyboard', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'touch screens only');
  await page.addInitScript(() => {
    navigator.mediaDevices.getUserMedia = async () => {
      const ctx = new AudioContext();
      const osc = ctx.createOscillator();
      const out = ctx.createMediaStreamDestination();
      osc.connect(out);
      osc.start();
      return out.stream;
    };
  });
  const term = terminalInput(page);
  await page.goto('/');
  const textarea = page.locator('.xterm-helper-textarea');
  await textarea.focus();
  await page.evaluate(() => {
    const vv = window.visualViewport!;
    Object.defineProperty(vv, 'height', { configurable: true, get: () => 500 });
    vv.dispatchEvent(new Event('resize'));
  });
  const mic = page.getByTestId('keybar').getByTestId('kb-mic');
  await mic.tap();
  await expect(mic).toHaveAttribute('aria-pressed', 'true');
  await expect(textarea).toBeFocused();
  // Stopped once audio has reached the server (the text so far).
  await expect.poll(term.line).toMatch(/^heard \d+ bytes$/);
  await mic.tap();
  await expect.poll(term.line).toMatch(/heard default $/);
  await expect(textarea).toBeFocused();
});

test('on a phone, the key bar stays while recording even when the keyboard goes down', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'touch screens only');
  await page.addInitScript(() => {
    navigator.mediaDevices.getUserMedia = async () => {
      const ctx = new AudioContext();
      const osc = ctx.createOscillator();
      const out = ctx.createMediaStreamDestination();
      osc.connect(out);
      osc.start();
      return out.stream;
    };
  });
  const term = terminalInput(page);
  await page.goto('/');
  const textarea = page.locator('.xterm-helper-textarea');
  await textarea.focus();
  const keyboard = (up: boolean) =>
    page.evaluate((up) => {
      const vv = window.visualViewport!;
      const h = up ? 500 : window.innerHeight;
      Object.defineProperty(vv, 'height', { configurable: true, get: () => h });
      if (!up) (document.activeElement as HTMLElement | null)?.blur();
      vv.dispatchEvent(new Event('resize'));
    }, up);
  await keyboard(true);
  const mic = page.getByTestId('keybar').getByTestId('kb-mic');
  await mic.tap();
  await expect(mic).toHaveAttribute('aria-pressed', 'true');
  // iOS took the keyboard down (its microphone question does that).
  await keyboard(false);
  await page.waitForTimeout(800);
  await expect(mic).toBeVisible();
  await expect.poll(term.line).toMatch(/^heard \d+ bytes$/);
  await mic.tap();
  await expect.poll(term.line).toMatch(/heard default $/);
});

test('on touch, any tap gives the page the keyboard, without raising the soft one', async ({ page, isMobile }) => {
  // iPadOS sends a hardware keyboard's keys only to a focused element.
  test.skip(!isMobile, 'touch');
  // An iPad's size, so the column is shown and the wordmark is there to tap.
  await page.setViewportSize({ width: 1180, height: 820 });
  await page.goto('/');
  await expect(page.getByTestId('toggle')).toHaveAttribute('aria-expanded', 'true');
  // iPadOS ignores a focus() from code: the page opens with nothing focused.
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await page.locator('.wordmark').tap();
  await expect(page.getByTestId('key-sink')).toBeFocused();
  // The column's keys arrive: ^⇧n opens the quick task.
  await page.keyboard.press('Control+Shift+N');
  await expect(page.locator('textarea[data-field=prompt]')).toBeVisible();
});

test("a task's chips are links and the header lists the current task's", async ({ page, isMobile }) => {
  await page.goto('/');
  if (isMobile) await page.getByTestId('toggle').tap();
  const chips = page.getByTestId('task').filter({ hasText: 'web version' }).locator('a.lchip');
  await expect(chips).toHaveCount(2);
  await expect(chips.first()).toHaveAttribute('href', 'https://claude.ai/code/artifact/0f3a9c');
  // A port is reached through the host the page came from.
  await expect(chips.nth(1)).toHaveAttribute('href', 'http://127.0.0.1:7070');

  // The current task (Fix login timeout) has its PR.
  const button = page.getByTestId('links');
  await (isMobile ? button.tap() : button.click());
  const menu = page.getByTestId(isMobile ? 'links-sheet' : 'links-menu');
  await expect(menu).toContainText('#412');
  await expect(menu.locator('a.lopen')).toHaveAttribute('href', 'https://github.com/acme/repo/pull/412');
  await page.keyboard.press('Escape');
  await expect(menu).toHaveCount(0);
});

test.describe('on an iPad', () => {
  // The device without its browser choice, which a group can't change.
  const { defaultBrowserType: _, ...ipad } = devices['iPad Pro 11'];
  test.use(ipad);
  test('the links open as a menu under the header, not a sheet across the screen', async ({ page }, info) => {
    test.skip(info.project.name !== 'desktop', 'one run is enough');
    await page.goto('/');
    await page.getByTestId('links').tap();
    const menu = page.getByTestId('links-menu');
    await expect(menu).toContainText('#412');
    await expect(page.getByTestId('links-sheet')).toHaveCount(0);
    const box = (await menu.boundingBox())!;
    expect(box.width).toBeLessThanOrEqual(360);
    const row = (await menu.locator('a.lopen').first().boundingBox())!;
    expect(row.height).toBeGreaterThanOrEqual(44);
  });

  test("a row's long-press opens a card beside the finger, not a sheet across the screen", async ({ page }, info) => {
    test.skip(info.project.name !== 'desktop', 'one run is enough');
    await page.goto('/');
    const row = page.locator('[data-id="web/better-loading"]');
    const b = (await row.boundingBox())!;
    const at = { pointerType: 'touch', pointerId: 9, isPrimary: true, bubbles: true, clientX: b.x + 40, clientY: b.y + 10 };
    await row.dispatchEvent('pointerdown', at);
    const sheet = page.getByTestId('sheet');
    await expect(sheet).toBeVisible({ timeout: 2000 });
    await row.dispatchEvent('pointerup', at);
    await expect(sheet.getByRole('menuitem', { name: 'go to task' })).toBeVisible();
    const box = (await sheet.boundingBox())!;
    expect(box.width).toBeLessThanOrEqual(320);
    // Just below the finger, which is in the upper half.
    expect(box.y).toBeGreaterThan(b.y + 10);
    expect(box.y).toBeLessThan(b.y + 40);
  });
});
