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
  // The TUI's `Column::is_shown`: a closed task under the cursor (the list
  // has the keyboard) is what the terminal area shows; else the current window.
  const closedSel = st.keys !== false && st.focus === 'list' && st.mode === 'list' && !at.sub && TASKS.find((t) => t.id === at.task)?.closed ? at.task : null;
  const shownId = closedSel ?? st.current;
  for (const t of TASKS) {
    if (t.section[0] !== last) {
      items.push({ kind: 'header', label: t.section[0], count: TASKS.filter((x) => x.section[0] === t.section[0]).length, color: t.section[1] });
      last = t.section[0];
    }
    const current = t.id === shownId;
    const selected = at.task === t.id && !at.sub && st.focus === 'list';
    items.push({
      kind: 'task', id: t.id, ws: t.ws, ws_color: t.ws_color, slug: t.id.split('/')[1], title: t.title,
      title_color: current ? C.cur : t.closed ? C.mut : C.text, glyph: t.glyph, glyph_color: t.glyph_color, status: t.status,
      selected, current, closed: !!t.closed, pending: false, reason: t.reason ?? null, agent: t.agent ?? null, age: t.age ?? null,
      prs: t.prs ?? [], ports: t.ports ?? [],
      answerable: t.status === 'blocked', locked: t.glyph === '🔒' && !st.rejected?.includes(t.id),
      wants: t.glyph === '🔒' && !st.rejected?.includes(t.id) ? WANTS : [],
    });
    for (const s of t.subs ?? []) {
      items.push({ kind: 'sub', task: t.id, ...s, selected: at.task === t.id && at.sub === s.id && st.focus === 'list' });
    }
  }
  const hint = st.focus === 'search' ? 'filter · ↓↑ switch · ⏎ open' : TASKS.find((t) => t.id === at.task).status === 'blocked' ? 'A/D answer · ⏎ open' : '↓↑ switch · ⏎ open · ^n new · ? keys';
  let mode = { kind: 'list' };
  // As view.rs: in the list, the last message replaces the hint.
  let footer = st.mode === 'list' && st.status
    ? { kind: 'message', tag: null, text: st.status, hint: null, warn: null }
    : { kind: 'hint', tag: st.focus === 'search' ? 'INSERT' : 'NORMAL', text: hint, hint: null, warn: null };
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
    const f = st.form;
    mode = {
      kind: 'create', workspace: WORKSPACES[f.ws].name, workspace_index: f.ws + 1, workspaces: WORKSPACES.length,
      workspace_options: WORKSPACES.map((w) => ({ name: w.name, color: w.color })),
      agent_options: AGENTS, agent_index: f.agent,
      name: f.name, repos: f.repos, agent: AGENTS[f.agent], agent_inherits: f.agent === 0, focus: 'name', focus_repo: null,
      agent_default: 'claude', slug: f.name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, ''),
    };
    footer = { kind: 'hint', tag: null, text: '⏎ create   esc cancel   ⇥ next   space toggle repo   ←→ workspace / agent', hint: null, warn: null };
  } else if (st.mode === 'reject') {
    const t = TASKS.find((x) => x.id === st.rejecting);
    mode = { kind: 'reject', task: t.title, id: t.id, names: WANTS, note: st.buffer };
    footer = { kind: 'error', tag: null, text: `⏎ reject ${WANTS.map((w) => w.name).join(', ')}   esc cancel`, hint: null, warn: null };
  } else if (st.mode === 'rename') {
    mode = { kind: 'rename', buffer: st.buffer };
    footer = { kind: 'hint', tag: null, text: '⏎ save   esc cancel', hint: null, warn: null };
  } else if (st.mode === 'edit_repos') {
    mode = { kind: 'edit_repos', task: 'Fix login timeout', picks: st.picks, focus: 0, confirm: !!st.confirmDetach };
    footer = { kind: 'hint', tag: null, text: '⏎ apply   esc cancel', hint: null, warn: null };
  }
  return {
    tabs: [{ label: 'Tasks', active: true, running: 0 }, { label: 'Repos', active: false, running: 0 }, { label: 'Work', active: false, running: 1 }],
    focus: st.focus, filter: st.filter, current: shownId, items,
    shown_closed: closedSel ? (({ id, title, ws, ws_color }) => ({ id, title, ws, ws_color }))(TASKS.find((t) => t.id === closedSel)) : null, mode, footer, help: HELP, status: st.status ?? null,
    jobs: st.jobs ?? [],
  };
}

// What the locked task (Cloud tasks) is waiting on, as the agent asked.
const WANTS = [
  { name: 'OPENAI_API_KEY', why: 'run the embedding eval against the real API' },
  { name: 'SENTRY_DSN', why: '' },
];

// The create form's pickers: a workspace brings its own repos, as in tenx.
const WORKSPACES = [
  { name: 'acme', color: '#5eb4aa', repos: ['api', 'web', 'infra'] },
  { name: 'notes', color: '#ce86a8', repos: ['notes'] },
  { name: 'tenx', color: '#78b4ce', repos: ['tenx'] },
  // tenx's own adhoc workspace: sessions without repos (`tenx ask`).
  { name: 'adhoc', color: '#aab064', repos: [] },
];
const AGENTS = ['default', 'claude', 'codex', 'pi'];
const repoChecks = (ws) => WORKSPACES[ws].repos.map((name) => ({ name, checked: true }));

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
  'https://github.com/aluedeke/tenx/pull/42',
  `${E}32mapi${E}0m ${E}35mfix-login-timeout${E}0m ❯ `,
].join('\r\n');

const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x');
  // `tenx web` hands this to its machine's speech server; here, a fixed
  // answer for any WAV, so the page's mic path can be driven end to end.
  if (req.method === 'POST' && url.pathname === '/transcribe') {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const body = Buffer.concat(chunks);
    const isWav = req.headers['content-type'] === 'audio/wav' && body.subarray(0, 4).toString() === 'RIFF';
    res.writeHead(isWav ? 200 : 502, { 'content-type': isWav ? 'application/json' : 'text/plain' });
    res.end(isWav ? JSON.stringify({ text: `heard ${url.searchParams.get('language') ?? 'default'}` }) : 'not a WAV');
    return;
  }
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
wss.on('connection', (ws, req) => {
  let jobIds = 0;
  const st = { sel: 1, focus: 'list', filter: '', current: 'acme/fix-login-timeout', mode: 'list', buffer: '', ack: 0 };
  // Test knobs, as cookies on the socket's request: `mocklag=<ms>` delays
  // every view (a slow connection); `mockskip=1` makes j move two rows, so a
  // prediction of one has to be corrected.
  const cookie = (name) => (req.headers.cookie ?? '').split(';').map((c) => c.trim().split('=')).find(([k]) => k === name)?.[1];
  const lag = Number(cookie('mocklag') ?? 0);
  const skip = cookie('mockskip') === '1';
  const json = (m) => ws.send(JSON.stringify(m));
  const push = () => {
    const msg = { type: 'view', ack: st.ack, view: view(st) };
    if (lag > 0) setTimeout(() => ws.readyState === ws.OPEN && json(msg), lag);
    else json(msg);
  };
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
    if (typeof m.seq === 'number') st.ack = Math.max(st.ack, m.seq);
    switch (m.type) {
      case 'viewport':
        json({ type: 'layout', column_cols: Math.max(30, Math.min(48, Math.round(m.cols * 0.28))), narrow: m.cols < 100 });
        return;
      case 'focus':
        st.keys = m.column;
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
      case 'form':
        form(m);
        push();
        return;
      case 'action': {
        // The list keys the real server presses for a button.
        const keys = { help: '?', open: 'Enter', next: 'n' };
        if (m.name === 'new') key({ key: 'n', ctrl: true });
        else if (m.name === 'rename') {
          st.mode = 'rename';
          st.buffer = TASKS.find((t) => t.id === CURSOR[st.sel].task).title;
        } else if (m.name === 'edit_repos') {
          st.mode = 'edit_repos';
          st.picks = [{ name: 'api', checked: true, present: true }, { name: 'web', checked: false, present: false }];
        }
        else if (m.name === 'reject') {
          const t = TASKS.find((x) => x.id === CURSOR[st.sel].task);
          if (t.glyph === '🔒' && !st.rejected?.includes(t.id)) {
            st.mode = 'reject';
            st.rejecting = t.id;
            st.buffer = '';
          } else st.status = 'no pending secrets for this task';
        }
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
  // `form` messages, as view.rs `handle_form` takes them.
  function form(m) {
    st.status = null;
    if (m.op === 'cancel') {
      st.mode = 'list';
      return;
    }
    if (st.mode === 'create') {
      const f = st.form;
      if (m.op === 'set' && m.field === 'name') f.name = m.value;
      if (m.op === 'check' && m.field === 'repo' && f.repos[m.index]) f.repos[m.index].checked = m.on;
      if (m.op === 'pick' && m.field === 'workspace' && WORKSPACES[m.index] && m.index !== f.ws) {
        f.ws = m.index;
        f.repos = repoChecks(m.index);
      }
      if (m.op === 'pick' && m.field === 'agent' && AGENTS[m.index]) f.agent = m.index;
      if (m.op === 'submit') {
        if (!f.name.trim()) st.status = 'name the task first';
        else {
          // As tenx: the form closes and a job starts; it reports progress,
          // then lands.
          st.mode = 'list';
          const job = { id: ++jobIds, title: `creating '${f.name}'`, counter: '1/2 api', fraction: 0 };
          st.jobs = [...(st.jobs ?? []), job];
          const tick = setInterval(() => {
            job.fraction = Math.min(1, job.fraction + 0.2);
            if (job.fraction >= 1) {
              clearInterval(tick);
              st.jobs = st.jobs.filter((j) => j !== job);
              st.status = `created '${f.name}'`;
            }
            push();
          }, Number(process.env.MOCK_JOB_TICK_MS ?? 400));
        }
      }
    } else if (st.mode === 'reject') {
      if (m.op === 'set' && m.field === 'note') st.buffer = m.value;
      if (m.op === 'submit') {
        st.rejected = [...(st.rejected ?? []), st.rejecting];
        st.status = `rejected ${WANTS.map((w) => w.name).join(', ')} for '${st.rejecting.split('/')[1]}'`;
        st.mode = 'list';
      }
    } else if (st.mode === 'rename') {
      if (m.op === 'set' && m.field === 'title') st.buffer = m.value;
      if (m.op === 'submit') {
        if (!st.buffer.trim()) st.status = 'title cannot be empty';
        else st.mode = 'list';
      }
    } else if (st.mode === 'edit_repos') {
      if (m.op === 'check' && st.picks[m.index]) st.picks[m.index].checked = m.on;
      if (m.op === 'submit') {
        // Detaching asks first, as tenx does.
        if (!st.confirmDetach && st.picks.some((p) => p.present && !p.checked)) st.confirmDetach = true;
        else {
          st.confirmDetach = false;
          st.mode = 'list';
        }
      }
    }
  }
  function key(m) {
    if (st.mode === 'edit_repos' && st.confirmDetach) {
      if (m.key === 'y') st.mode = 'list';
      if (m.key === 'y' || m.key === 'Escape') st.confirmDetach = false;
      return;
    }
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
      st.form = { ws: 0, name: '', repos: repoChecks(0), agent: 0 };
      return;
    }
    if (st.focus === 'search') {
      if (m.key === 'Escape' || m.key === 'ArrowDown') st.focus = 'list';
      else if (m.key === 'Backspace') st.filter = st.filter.slice(0, -1);
      else if (m.key.length === 1) st.filter += m.key;
      return;
    }
    if (m.key === 'j' || m.key === 'ArrowDown') st.sel = Math.min(CURSOR.length - 1, st.sel + (skip ? 2 : 1));
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
