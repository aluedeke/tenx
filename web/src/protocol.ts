// The wire contract with `tenx web` (docs/web-protocol.md). The view types
// mirror src/tui/column/view.rs field for field; change both together.

export interface ColumnView {
  tabs: TabView[];
  /** Where the cursor is: the search field (Insert) or the list (Normal). */
  focus: 'search' | 'list';
  filter: string;
  /** The current task's row id, if the session's current window is one. */
  current: string | null;
  items: Item[];
  mode: ModeView;
  footer: Footer;
  help: HelpSection[];
}

export interface TabView {
  label: string;
  active: boolean;
  /** Running jobs, on the Work tab only; 0 means no `[n]`. */
  running: number;
}

export interface Chip {
  label: string;
  fg: string;
  bg?: string;
}

export type Item =
  | { kind: 'header'; label: string; count: number | null; color: string }
  | ({ kind: 'task' } & TaskItem)
  | ({ kind: 'sub' } & SubItem)
  | ({ kind: 'repo' } & RepoItem)
  | ({ kind: 'job' } & JobItem)
  | { kind: 'empty'; lines: string[] };

export type TaskStatus = 'working' | 'blocked' | 'signaled' | 'done' | 'idle';

export interface TaskItem {
  /** `<workspace>/<slug>`. */
  id: string;
  ws: string;
  ws_color: string;
  slug: string;
  title: string;
  title_color: string;
  glyph: string;
  glyph_color: string;
  status: TaskStatus;
  selected: boolean;
  current: boolean;
  closed: boolean;
  pending: boolean;
  reason: Chip | null;
  agent: string | null;
  age: string | null;
  prs: Chip[];
  ports: number[];
}

export interface SubItem {
  task: string;
  id: string;
  glyph: string;
  glyph_color: string;
  label: string;
  extras: string[];
  finished: boolean;
  selected: boolean;
}

export interface RepoItem {
  pos: number;
  ws: string;
  name: string;
  cloned: boolean;
  detail: string;
  selected: boolean;
}

export interface JobItem {
  pos: number;
  title: string;
  state: 'running' | 'done' | 'failed';
  counter: string | null;
  steps: StepItem[];
  fraction: number | null;
  transfer: string | null;
  outcome: string | null;
  selected: boolean;
}

export interface StepItem {
  label: string;
  state: 'pending' | 'running' | 'done' | 'failed';
  note: string;
}

export interface Check {
  name: string;
  checked: boolean;
}

export interface Pick {
  name: string;
  checked: boolean;
  present: boolean;
}

export type ModeView =
  | { kind: 'list' }
  | { kind: 'command'; buffer: string }
  | {
      kind: 'create';
      workspace: string;
      workspace_index: number;
      workspaces: number;
      name: string;
      repos: Check[];
      agent: string;
      agent_inherits: boolean;
      focus: 'workspace' | 'name' | 'repo' | 'agent';
      focus_repo: number | null;
    }
  | { kind: 'add_repo'; workspace: string; url: string; name: string; focus: 'url' | 'name' }
  | {
      kind: 'new_workspace';
      path: string;
      name: string;
      repo_url: string;
      skills: boolean;
      focus: 'path' | 'name' | 'repo_url' | 'skills';
    }
  | { kind: 'edit_repos'; task: string; picks: Pick[]; focus: number; confirm: boolean }
  | { kind: 'confirm'; title: string }
  | { kind: 'rename'; buffer: string }
  | { kind: 'help'; scroll: number };

export interface HelpSection {
  section: string;
  /** (keys, action) pairs. */
  keys: [string, string][];
}

export interface Footer {
  kind: 'hint' | 'command' | 'confirm' | 'message' | 'error';
  tag: 'INSERT' | 'NORMAL' | null;
  text: string;
  hint: string | null;
  warn: string | null;
}

// ── Messages ────────────────────────────────────────────────────────────────

export type ServerMessage =
  | { type: 'hello'; session: string; host: string; version: string }
  | { type: 'view'; view: ColumnView }
  | { type: 'layout'; column_cols: number; narrow: boolean }
  | { type: 'request'; request: 'focus_terminal' | 'focus_column' | 'hide' | 'quit' }
  | { type: 'error'; message: string };

export interface KeyMessage {
  type: 'key';
  key: string;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
}

export type Click =
  | { kind: 'tab'; index: number }
  | { kind: 'search' }
  | { kind: 'task'; id: string; sub?: string }
  | { kind: 'item'; pos: number };

export type ClientMessage =
  | KeyMessage
  | ({ type: 'click' } & Click)
  | { type: 'resize'; cols: number; rows: number }
  | { type: 'viewport'; cols: number }
  | { type: 'focus'; column: boolean }
  | { type: 'visible' };
