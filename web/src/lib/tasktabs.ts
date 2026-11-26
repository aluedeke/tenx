// Which tab the task tab bar lights (components/TaskTabs). Pure.
//
// A click lights its tab at once; the view confirms it a moment later. Until
// it does, a view naming another tab is one from before the click — or an
// intermediate one, after quick clicks on two tabs — and must not move the
// light back and forth. Only a click the view never confirms gives up, after
// `GIVE_UP_MS`, and the bar shows what the window shows.

import type { WindowTab } from '@/protocol';

/** A tab asked for by a click, and when. */
export interface Asked {
  tab: WindowTab;
  at: number;
}

export const GIVE_UP_MS = 3000;

/** The tab to light, given what the view says (`shown`), the last click
 * (`asked`) and the time. */
export function litTab(shown: WindowTab, asked: Asked | null, now: number): WindowTab {
  if (!asked || asked.tab === shown) return shown;
  return now - asked.at < GIVE_UP_MS ? asked.tab : shown;
}
