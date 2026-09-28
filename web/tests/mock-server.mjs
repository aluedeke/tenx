// A stand-in for `tenx web` for the smoke test and for working on the page
// without a tmux server: serves web/out and speaks docs/web-protocol.md with
// a canned ColumnView. Just enough of the column to click and key through —
// j/k move, ? help, : command line, ^n the new-task form, esc/q hand the
// keyboard back — none of it is tenx's real logic.
//
//   node tests/mock-server.mjs [port]      (default 7071)

import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebSocketServer } from 'ws';

const root = fileURLToPath(new URL('../out/', import.meta.url));
const port = Number(process.argv[2] ?? process.env.PORT ?? 7071);

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.woff2': 'font/woff2',
  '.woff': 'font/woff',
  '.json': 'application/json',
  '.txt': 'text/plain',
  '.webmanifest': 'application/manifest+json',
};

const C = { warn: '#e4a854', info: '#6896dc', ok: '#7ac27c', acc: '#a78bfa', mut: '#787f8c', idle: '#606878', text: '#d6dbe3', cur: '#78a0e6' };

const TASKS = [
  { section: ['SECRETS PENDING', C.acc], id: 'tenx/cloud-tasks', ws: 'tenx', ws_color: '#78b4ce', title: 'Cloud tasks', glyph: '🔒', glyph_color: C.acc, status: 'idle', reason: { label: 'wants OPENAI_API_KEY', fg: C.acc, bg: '#2a2440' } },
  { section: ['WAITING FOR INPUT', C.warn], id: 'acme/fix-login-timeout', ws: 'acme', ws_color: '#5eb4aa', title: 'Fix login timeout', glyph: '●', glyph_color: C.warn, status: 'blocked', reason: { label: 'permission: Bash', fg: C.warn, bg: '#3a2e18' }, age: '4m', prs: [{ label: '#412 ✓', fg: C.ok }] },
  { section: ['WAITING FOR INPUT', C.warn], id: 'web/better-loading', ws: 'web', ws_color: '#ce86a8', title: 'Better loading indicators', glyph: '✔', glyph_color: C.ok, status: 'done', age: '12m', prs: [{ label: '#398 ✓', fg: C.ok }] },
  {
    section: ['WORKING', C.info], id: 'tenx/web-version', ws: 'tenx', ws_color: '#78b4ce', title: 'web version', glyph: '◐', glyph_color: C.info, status: 'working', ports: [7070],
    subs: [
      { id: 'a1', glyph: '◐', glyph_color: C.info, label: 'Map column keymap', extras: ['bg', 'Explore'], finished: false },
      { id: 'a2', glyph: '✔', glyph_color: C.ok, label: 'Map tenx architecture', extras: ['Explore'], finished: true },
    ],
  },
  { section: ['WORKING', C.info], id: 'tenx/mobile-fixes', ws: 'tenx', ws_color: '#78b4ce', title: 'Mobile fixes', glyph: '◐', glyph_color: C.info, status: 'working', agent: 'codex' },
  { section: ['WORKING', C.info], id: 'tenx/release', ws: 'tenx', ws_color: '#78b4ce', title: 'Release and install process', glyph: '◐', glyph_color: C.info, status: 'working', prs: [{ label: '#420 …', fg: C.info }] },
  { section: ['INACTIVE', C.mut], id: 'notes/competitor-analysis', ws: 'notes', ws_color: '#ce86a8', title: 'Competitor analysis', glyph: '·', glyph_color: C.idle, status: 'idle', closed: true },
  { section: ['INACTIVE', C.mut], id: 'tenx/zero-permission', ws: 'tenx', ws_color: '#78b4ce', title: 'Zero permission', glyph: '·', glyph_color: C.idle, status: 'idle', closed: true },
];

const HELP = [
  { section: 'anywhere', keys: [['^w', 'into the column / hide it'], ['?', 'this help (list mode)'], [':', 'command line']] },
  { section: 'list', keys: [['j k ↓ ↑', 'move'], ['⏎ o l', 'open task / agent'], ['A D', 'approve / deny permission'], ['esc q ^c', 'back to the task']] },
];

// The cursor walks tasks and their subagent lines, like the TUI's.
const CURSOR = TASKS.flatMap((t) => [{ task: t.id }, ...(t.subs ?? []).map((s) => ({ task: t.id, sub: s.id }))]);

function view(st) {
  const items = [];
  let last = null;
  const at = CURSOR[st.sel];
  for (const t of TASKS) {
    if (t.section[0] !== last) {
      items.push({ kind: 'header', label: t.section[0], count: TASKS.filter((x) => x.section[0] === t.section[0]).length, color: t.section[1] });
      last = t.section[0];
    }
    const current = t.id === st.current;
    const selected = at.task === t.id && !at.sub && st.focus === 'list';
    items.push({
      kind: 'task', id: t.id, ws: t.ws, ws_color: t.ws_color, slug: t.id.split('/')[1], title: t.title,
      title_color: current ? C.cur : t.closed ? C.mut : C.text, glyph: t.glyph, glyph_color: t.glyph_color, status: t.status,
      selected, current, closed: !!t.closed, pending: false, reason: t.reason ?? null, agent: t.agent ?? null, age: t.age ?? null,
      prs: t.prs ?? [], ports: t.ports ?? [],
      answerable: t.status === 'blocked', locked: t.glyph === '🔒',
    });
    for (const s of t.subs ?? []) {
      items.push({ kind: 'sub', task: t.id, ...s, selected: at.task === t.id && at.sub === s.id && st.focus === 'list' });
    }
  }
  const hint = st.focus === 'search' ? 'filter · ↓↑ switch · ⏎ open' : TASKS.find((t) => t.id === at.task).status === 'blocked' ? 'A/D answer · ⏎ open' : '↓↑ switch · ⏎ open · ^n new · ? keys';
  let mode = { kind: 'list' };
  let footer = { kind: 'hint', tag: st.focus === 'search' ? 'INSERT' : 'NORMAL', text: hint, hint: null, warn: null };
  if (st.mode === 'help') {
    mode = { kind: 'help', scroll: 0 };
    footer = { kind: 'hint', tag: null, text: 'j/k scroll · any key closes', hint: null, warn: null };
  } else if (st.mode === 'command') {
    mode = { kind: 'command', buffer: st.buffer };
    footer = { kind: 'command', tag: null, text: st.buffer, hint: st.buffer ? null : 'new · open · delete · rename · close · help · quit', warn: null };
  } else if (st.mode === 'confirm') {
    const t = TASKS.find((x) => x.id === at.task);
    mode = { kind: 'confirm', title: t.title };
    footer = { kind: 'confirm', tag: null, text: `delete '${t.title}' + worktrees?   y = delete   n/esc = cancel`, hint: null, warn: null };
  } else if (st.mode === 'create') {
    mode = { kind: 'create', workspace: 'acme', workspace_index: 1, workspaces: 3, name: st.buffer, repos: [{ name: 'api', checked: true }, { name: 'web', checked: true }, { name: 'infra', checked: false }], agent: 'claude', agent_inherits: true, focus: 'name', focus_repo: null };
    footer = { kind: 'hint', tag: null, text: '⏎ create   esc cancel   ⇥ next   space toggle repo   ←→ workspace / agent', hint: null, warn: null };
  }
  return {
    tabs: [{ label: 'Tasks', active: true, running: 0 }, { label: 'Repos', active: false, running: 0 }, { label: 'Work', active: false, running: 1 }],
    focus: st.focus, filter: st.filter, current: st.current, items, mode, footer, help: HELP,
  };
}

const E = '\x1b[';
const SCREEN = [
  `${E}35m✻${E}0m ${E}1;97mClaude Code${E}0m  ${E}90m~/work/tasks/fix-login-timeout${E}0m`,
  '',
  `${E}90m>${E}0m the session cookie expires after 5 min on staging only`,
  '',
  `${E}32m⏺${E}0m ${E}1mBash${E}0m(cargo test -p api session::)`,
  `${E}35m╭──────────────────────────────────────────╮${E}0m`,
  `${E}35m│${E}0m ${E}1;97mBash command${E}0m                             ${E}35m│${E}0m`,
  `${E}35m│${E}0m   cargo test -p api session::             ${E}35m│${E}0m`,
  `${E}35m│${E}0m Do you want to proceed?                  ${E}35m│${E}0m`,
  `${E}35m│${E}0m ${E}35m❯ 1. Yes${E}0m                                 ${E}35m│${E}0m`,
  `${E}35m│${E}0m   2. No                                  ${E}35m│${E}0m`,
  `${E}35m╰──────────────────────────────────────────╯${E}0m`,
  '',
  `${E}32mapi${E}0m ${E}35mfix-login-timeout${E}0m ❯ `,
].join('\r\n');

const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x');
  let path = normalize(decodeURIComponent(url.pathname)).replace(/^(\.\.[/\\])+/, '');
  if (path.endsWith('/')) path += 'index.html';
  let file = join(root, path);
  try {
    if (!(await stat(file)).isFile()) throw new Error();
  } catch {
    try {
      file = join(root, path + '.html');
      await stat(file);
    } catch {
      res.writeHead(404).end('not found');
      return;
    }
  }
  res.writeHead(200, { 'content-type': TYPES[extname(file)] ?? 'application/octet-stream' });
  res.end(await readFile(file));
});

const wss = new WebSocketServer({ server, path: '/ws' });
let sessions = 0;
wss.on('connection', (ws) => {
  const st = { sel: 1, focus: 'list', filter: '', current: 'acme/fix-login-timeout', mode: 'list', buffer: '' };
  const json = (m) => ws.send(JSON.stringify(m));
  const push = () => json({ type: 'view', view: view(st) });
  json({ type: 'hello', session: `mock${++sessions}`, host: 'mock', version: '0.0.0' });
  ws.send(Buffer.from(SCREEN));
  push();
  ws.on('message', (data, binary) => {
    if (binary) {
      // Echo keyboard input, as a shell at a prompt would.
      ws.send(Buffer.from(data.toString().replace(/\r/g, '\r\n')));
      return;
    }
    const m = JSON.parse(data.toString());
    switch (m.type) {
      case 'viewport':
        json({ type: 'layout', column_cols: Math.max(30, Math.min(48, Math.round(m.cols * 0.28))), narrow: m.cols < 100 });
        return;
      case 'focus':
        if (m.column) st.sel = Math.max(0, CURSOR.findIndex((c) => c.task === st.current && !c.sub));
        push();
        return;
      case 'click':
        if (m.kind === 'task') st.sel = CURSOR.findIndex((c) => c.task === m.id && c.sub === m.sub);
        if (m.kind === 'search') st.focus = 'search';
        else st.focus = 'list';
        push();
        return;
      case 'key':
        key(m);
        push();
        return;
      case 'action': {
        // The list keys the real server presses for a button.
        const keys = { help: '?', open: 'Enter', next: 'n' };
        if (m.name === 'new') key({ key: 'n', ctrl: true });
        else if (m.name === 'delete') {
          st.focus = 'list';
          st.mode = 'confirm';
        }
        else if (keys[m.name]) {
          st.focus = 'list';
          key({ key: keys[m.name] });
        }
        push();
        return;
      }
    }
  });
  function key(m) {
    if (st.mode === 'confirm') {
      if (['y', 'n', 'Escape'].includes(m.key)) st.mode = 'list';
      return;
    }
    if (st.mode === 'help') {
      st.mode = 'list';
      return;
    }
    if (st.mode === 'command' || st.mode === 'create') {
      if (m.key === 'Escape' || m.key === 'Enter') {
        st.mode = 'list';
        st.buffer = '';
      } else if (m.key === 'Backspace') st.buffer = st.buffer.slice(0, -1);
      else if (m.key.length === 1) st.buffer += m.key;
      return;
    }
    if (m.ctrl && m.key === 'n') {
      st.mode = 'create';
      return;
    }
    if (st.focus === 'search') {
      if (m.key === 'Escape' || m.key === 'ArrowDown') st.focus = 'list';
      else if (m.key === 'Backspace') st.filter = st.filter.slice(0, -1);
      else if (m.key.length === 1) st.filter += m.key;
      return;
    }
    if (m.key === 'j' || m.key === 'ArrowDown') st.sel = Math.min(CURSOR.length - 1, st.sel + 1);
    else if (m.key === 'k' || m.key === 'ArrowUp') st.sel = Math.max(0, st.sel - 1);
    else if (m.key === '?') st.mode = 'help';
    else if (m.key === ':') st.mode = 'command';
    else if (m.key === '/' || m.key === 'i') st.focus = 'search';
    else if (m.key === 'Enter' || m.key === 'o') {
      st.current = CURSOR[st.sel].task;
      json({ type: 'request', request: 'focus_terminal' });
    } else if (m.key === 'Escape' || m.key === 'q') json({ type: 'request', request: 'focus_terminal' });
  }
});

server.listen(port, '127.0.0.1', () => console.log(`mock tenx web on http://127.0.0.1:${port}/`));
