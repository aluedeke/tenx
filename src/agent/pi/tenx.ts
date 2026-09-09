// TENX_INTEGRATION_VERSION=1
//
// Installed by `tenx agent setup pi`. Reports pi's session state to tenx so a
// pi task shows the right status in the tenx overlay and status bar. Managed by
// tenx — reinstalling overwrites this file when the version changes; add your
// own extensions in separate files rather than editing this one.
//
// It maps pi's lifecycle events to tenx's session record by piping a small JSON
// payload to `tenx internal session-event --agent pi --pid <pid>`, the same sink
// Claude Code's and Codex's hooks use. Everything is best-effort and never
// throws: a reporting failure must not disturb pi.
import { spawn } from "node:child_process";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

function report(event: string, message: string | undefined, ctx: any): void {
  try {
    let session_id: string | undefined;
    let transcript_path: string | undefined;
    try {
      session_id = ctx?.sessionManager?.getSessionId?.();
    } catch {}
    try {
      const f = ctx?.sessionManager?.getSessionFile?.();
      if (typeof f === "string") transcript_path = f;
    } catch {}
    const payload = JSON.stringify({
      hook_event_name: event,
      cwd: process.cwd(),
      session_id,
      transcript_path,
      message,
    });
    const child = spawn(
      "tenx",
      ["internal", "session-event", "--agent", "pi", "--pid", String(process.pid)],
      { stdio: ["pipe", "ignore", "ignore"] },
    );
    child.on("error", () => {});
    child.stdin.on("error", () => {});
    child.stdin.end(payload);
  } catch {}
}

export default function (pi: ExtensionAPI) {
  pi.on("session_start", async (_e: any, ctx: any) => report("session_start", undefined, ctx));
  pi.on("agent_start", async (_e: any, ctx: any) => report("agent_start", undefined, ctx));
  // The only "waiting on the user" signal pi exposes: it brackets a blocking
  // ui.confirm/select/input prompt.
  pi.on("ui_prompt_start", async (e: any, ctx: any) => report("ui_prompt_start", e?.title || e?.kind, ctx));
  pi.on("ui_prompt_end", async (_e: any, ctx: any) => report("ui_prompt_end", undefined, ctx));
  // Fires when pi will not continue on its own — the true "your move" moment.
  pi.on("agent_settled", async (_e: any, ctx: any) => report("agent_settled", undefined, ctx));
  // The delete is best-effort; if pi exits before the child writes, tenx's
  // watcher prunes the record when the pid goes away.
  pi.on("session_shutdown", async (_e: any, ctx: any) => report("session_shutdown", undefined, ctx));
}
