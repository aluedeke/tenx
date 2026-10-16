// A `patch` message applied to the last view (docs/web-protocol.md): the
// server sends the whole column once per socket, then only what changed —
// top-level fields whole (`set`, `unset`), and the rows as a list whose
// entries are either the index of an unchanged row in the old list or a new
// row. Mirrors tenx-core/src/viewpatch.rs `apply`; pure, for the unit tests.

import type { ColumnView, Item } from '../protocol.ts';

export interface ViewPatch {
  set?: Partial<ColumnView>;
  unset?: string[];
  items?: (number | Item)[];
}

/** `view` itself when the patch changes nothing (only the ack moved). */
export function applyPatch(view: ColumnView, patch: ViewPatch): ColumnView {
  if (!patch.set && !patch.unset && !patch.items) return view;
  const out: Record<string, unknown> = { ...view, ...(patch.set ?? {}) };
  for (const key of patch.unset ?? []) delete out[key];
  if (patch.items) out.items = patch.items.map((e) => (typeof e === 'number' ? view.items[e] : e));
  return out as unknown as ColumnView;
}
