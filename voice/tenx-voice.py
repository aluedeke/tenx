# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#     "pipecat-ai[anthropic,kokoro,mcp,mlx-whisper,runner,silero,webrtc,whisper]==1.15.0",
# ]
# ///
"""tenx-voice: the voice assistant behind `tenx web`'s talk mode.

Run it yourself, on the machine `tenx web` runs on:

    uv run --script voice/tenx-voice.py

It listens on a Unix socket, `~/.config/tenx/voice.sock` (TENX_VOICE_SOCKET
overrides; `tenx_core::voice::socket_path`), and `tenx web` connects to it
within a couple of seconds: the page shows its 🎧 from then on, and hides it
when this program stops. Several `tenx web`s may connect at once; each gets
its own replies, news and pages.

One process serves every connected page: a Pipecat pipeline per WebRTC
connection — Silero VAD, speech to text, Claude, Kokoro — whose tools are
tenx's own, through `tenx web`'s `/mcp`.

What it runs with comes from `tenx web` (`setup`, the first line and again
after a settings change): the `/mcp` address and token, the API key,
`[voice]`, `[speech]`'s model and glossary for `tenx-whisper` — the
microphone button's, a model tuned to its user's voice and words; one
process per model, spoken to as `tenx web` does (`tenx_core::whisper`);
MLX Whisper without one — and `[say]`.

The conversation with `tenx web` is `tenx_core::voice`: JSON lines in
(`setup`, `offer`, `ice`, `context`, `pause`, `event`) and out (`ready`,
`answer`, `ok`, `error`, `sessions`). A closed connection ends its pages'
conversations.

The `runner` extra is only for fastapi, which the WebRTC request handler
imports.
"""

import asyncio
import json
import os
import re
import stat
from dataclasses import dataclass, field

from loguru import logger
from mcp.client.session_group import StreamableHttpParameters

from pipecat.audio.vad.silero import SileroVADAnalyzer
from pipecat.audio.vad.vad_analyzer import VADParams
import numpy as np

from pipecat.frames.frames import (
    BotStartedSpeakingFrame,
    Frame,
    LLMFullResponseEndFrame,
    LLMFullResponseStartFrame,
    LLMTextFrame,
    TTSSpeakFrame,
    ErrorFrame,
    FunctionCallsStartedFrame,
    InterruptionFrame,
    TranscriptionFrame,
    BotStoppedSpeakingFrame,
    LLMMessagesAppendFrame,
    LLMRunFrame,
    UserStartedSpeakingFrame,
    UserStoppedSpeakingFrame,
)
from pipecat.observers.base_observer import BaseObserver, FramePushed
from pipecat.pipeline.pipeline import Pipeline
from pipecat.pipeline.worker import PipelineParams, PipelineWorker
from pipecat.processors.frame_processor import FrameDirection, FrameProcessor
from pipecat.processors.aggregators.llm_context import LLMContext
from pipecat.processors.aggregators.llm_response_universal import (
    LLMContextAggregatorPair,
    LLMUserAggregatorParams,
)
from pipecat.services.anthropic.llm import AnthropicLLMService
from pipecat.services.kokoro.tts import KokoroTTSService
from pipecat.services.mcp_service import MCPClient
from pipecat.services.settings import STTSettings
from pipecat.services.stt_service import SegmentedSTTService
from pipecat.transcriptions.language import Language
from pipecat.turns.user_turn_completion_mixin import UserTurnCompletionConfig
from pipecat.turns.user_turn_strategies import FilterIncompleteUserTurnStrategies
from pipecat.transports.base_transport import TransportParams
from pipecat.transports.smallwebrtc.request_handler import (
    IceCandidate,
    SmallWebRTCPatchRequest,
    SmallWebRTCRequest,
    SmallWebRTCRequestHandler,
)
from pipecat.transports.smallwebrtc.transport import SmallWebRTCTransport
from pipecat.utils.time import time_now_iso8601
from pipecat.workers.runner import WorkerRunner

SOCKET = os.environ.get("TENX_VOICE_SOCKET") or os.path.expanduser("~/.config/tenx/voice.sock")


@dataclass
class Setup:
    """`tenx_core::voice::Setup`: what `tenx web` says to run with. Empty
    strings are the defaults below."""

    mcp_url: str = ""
    mcp_token: str = ""
    api_key: str = ""
    model: str = ""
    tts_voice: str = ""
    stt_model: str = ""
    language: str = ""
    whisper: str = ""
    whisper_model: str = ""
    whisper_prompt_file: str = ""
    # Words the voice gets wrong: [["tenx", "ten x"], …], from `[say]` and the
    # built-in ones (`tenx_core::readaloud::say_table`).
    say: dict = field(default_factory=dict)

    @staticmethod
    def of(msg: dict) -> "Setup":
        known = {k: v for k, v in msg.items() if k in Setup.__dataclass_fields__}
        known["say"] = dict(known.get("say") or [])
        return Setup(**known)

    @property
    def claude(self) -> str:
        # TENX_VOICE_MODEL: try another model without touching the config.
        return os.environ.get("TENX_VOICE_MODEL") or self.model or "claude-sonnet-5-5"

    @property
    def voice(self) -> str:
        return self.tts_voice or "af_heart"

    @property
    def mlx_model(self) -> str:
        return self.stt_model or "mlx-community/whisper-large-v3-turbo"

    @property
    def spoken(self) -> str:
        """The language spoken, a Whisper code; "auto" lets Whisper tell, so
        German and English both work."""
        return self.language or "auto"

    @property
    def heard(self):
        return None if self.spoken == "auto" else Language(self.spoken)


MAX_TOOL_CHARS = 6000

# Tools about one task: using one shows that task on the page first
# (`Session.follow`), so its history is on screen while the assistant talks
# about it — not left to the model, which forgets.
# Tools that change something: never cancelled because the user spoke while
# one runs — "create the task" said again over a running create made a second
# one.
ACTS = {"tenx_create_task", "tenx_send_message", "tenx_answer_permission"}

# What the assistant answers when nothing needs saying; `Hush` drops it.
QUIET = "[quiet]"

# The turn markers each response starts with (Pipecat's turn completion):
# ASCII, not Pipecat's ● ◐ ○ — Sonnet writes those as single bytes, which
# reach the voice as "โ—Ź" instead of being taken off.
DONE, CUT, WAIT = "^", "~", "="

FOLLOWS = {"tenx_get_task", "tenx_get_output", "tenx_read_screen", "tenx_send_message", "tenx_answer_permission"}

# tenx's MCP tools the voice may use. Not `tenx_wait`: it blocks for up to a
# minute, and a conversation can't go quiet that long — tenx tells the
# assistant when a task changes instead (`event`).
TOOLS = [
    "tenx_focus_task",
    "tenx_list_workspaces",
    "tenx_list_tasks",
    "tenx_get_task",
    "tenx_get_output",
    "tenx_read_screen",
    "tenx_create_task",
    "tenx_send_message",
    "tenx_answer_permission",
]

SYSTEM = """\
You are the voice assistant for tenx, a tool that runs coding tasks. Each task
has its own coding agent (usually Claude Code) that does the real work; you
never write code yourself. You help the user see what their tasks are doing
and steer them, through the tenx tools. A task is named by its workspace and
its slug; tool results give both.

You speak with the user directly: say "you", never "the user".

The user may speak German or English. Answer in English only, every
word of it, without remarks about the language: your voice cannot
pronounce anything else.

Your words are spoken aloud: plain sentences, no Markdown, no lists, no code,
no URLs, no file paths unless asked. Call tasks by their title, not their
slug, unless two share a title.

Say as little as you can. The user finds every extra word annoying:
- One short sentence, about fifteen words at most. More only when the user
  asks for details. Name at most three tasks, then say how many more.
- Never acknowledge ("okay", "got it", "understood", "good", "sure", "great").
- Never say what you are about to do or look up: call the tool with no text
  before the call, then answer.
- Never repeat what you said before, and never explain why you did something
  unless asked.
- Never end with a question or an offer ("want me to…", "shall I…",
  "anything else?"), except the one yes-or-no question before an action.
- Ask at most one question at a time, and only for what you can't do without.
- When nothing needs saying, reply with exactly [quiet] and nothing else:
  the user thanks you, agrees or says it's fine without asking for
  anything, or talks to someone else. Never stay quiet when the user
  answers your question or asks something.

Good answers:
- "Which tasks are working?" → "Four: checkly cli, symbolication, pipecat and mobile apps."
- "Thanks." → [quiet]
- "Create a task in checkly rum that compares Checkly with Sentry." → "A checkly-rum task to compare Checkly and Sentry error tracking. Okay?"
- After creating it → "Created."
- "What did pipecat say?" → "It finished the voice settings and waits for your review."
Bad answers: "Okay, what should the prompt say, and should it use all repos?"
(an acknowledgement, two questions); "I'll check which tasks need you." (says
what it is about to do); "I need the prompt before I create anything. Shall
I…" (explains itself).

What the user says a new task should do is its prompt; make up a short title
from it. Use the workspace's repos unless they name some.

Statuses: blocked means the task's agent waits on a dialog (often a
permission prompt); signaled means it rang a bell; working means busy; done
means it finished its turn and waits for the next message; idle means no
agent is running.

A task you read, message or answer for is shown on the user's screen by
itself. Use tenx_focus_task when they ask to see a task.

To find a task by name, call tenx_list_tasks without a status: it lists
every task. Never say a task doesn't exist before you have.

When the user asks what a task said, read it with tenx_get_output and sum it
up in a sentence or two; offer details only if asked. To see what a blocked
task asks, use tenx_read_screen.

Before anything that changes something — sending a message to a task,
approving or denying a permission, creating a task — say in a few words
what you will do, ending "okay?", and wait for a yes. Speech recognition
mishears; never act on a guess. The user often dictates a long message in
pieces with pauses: put the pieces together as one. Speech recognition ends
every piece with a period, even mid-sentence. A piece that leaves something
open is cut off, whatever its period — wait for the rest (~): "create a
new.", "the agent should compare.", "it should make a plan how we.", "tell
pipecat that.", a piece ending on "that", "and", "with", "the" or "to". The
user names what to compare, send or create in the next piece.

Messages that start with "[tenx]" come from tenx, not from the user. They
say which task the user is looking at ("it", "this task" mean that one), or
that something happened: tell the user in a few words ("Pipecat is done.",
"Fix login needs you: a permission for Bash.").
"""


def clip(output):
    text = output if isinstance(output, str) else json.dumps(output)
    if len(text) <= MAX_TOOL_CHARS:
        return text
    # The end of a transcript or a screen is the part that matters.
    return "…(earlier part left out)…\n" + text[-MAX_TOOL_CHARS:]


def compact_tasks(output):
    """`tenx_list_tasks`, one short line per task: all of them fit, even a
    workspace with a hundred and more — an idle task needs only its name, to
    be found when the user asks for it."""
    text = output if isinstance(output, str) else json.dumps(output)
    try:
        tasks = json.loads(text)
    except (json.JSONDecodeError, TypeError):
        return clip(output)
    lines = []
    for t in tasks:
        line = f"{t.get('ws')}/{t.get('slug')} \"{t.get('title')}\" {t.get('status')}"
        if t.get("waiting_for"):
            line += f" — waits for: {t['waiting_for']}"
        prs = [p.get("chip") for p in t.get("prs") or [] if p.get("chip")]
        if prs:
            line += f" — PRs: {', '.join(prs)}"
        if t.get("secrets_pending"):
            line += f" — secrets pending: {', '.join(t['secrets_pending'])}"
        lines.append(line)
    return "\n".join(lines) or "no tasks"


class Whisper:
    """`tenx-whisper`, one process for every session, one segment at a time:
    a header line `{"samples": N}`, N little-endian f32 samples at 16 kHz,
    and a `{"text": …}` or `{"error": …}` line back. Started on the first
    segment, again if it died; it ends with this program (EOF on its stdin).
    """

    RATE = 16_000

    def __init__(self, program: str, model: str, language: str, prompt_file: str):
        self.args = [program or "tenx-whisper", "--model", model, "--language", language]  # takes "auto"
        if prompt_file:
            self.args += ["--prompt-file", prompt_file]
        self.proc: asyncio.subprocess.Process | None = None
        self.lock = asyncio.Lock()

    async def _start(self):
        args = self.args
        self.proc = await asyncio.create_subprocess_exec(
            *args, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=None
        )
        reply = json.loads(await self.proc.stdout.readline() or b'{"error":"tenx-whisper exited"}')
        if "error" in reply:
            raise RuntimeError(f"tenx-whisper: {reply['error']}")

    async def transcribe(self, samples: np.ndarray) -> str:
        async with self.lock:
            for attempt in range(2):
                if self.proc is None or self.proc.returncode is not None:
                    await self._start()
                try:
                    header = json.dumps({"samples": len(samples)}).encode() + b"\n"
                    self.proc.stdin.write(header + samples.astype("<f4").tobytes())
                    await self.proc.stdin.drain()
                    line = await self.proc.stdout.readline()
                    if not line:
                        raise RuntimeError("tenx-whisper exited")
                    reply = json.loads(line)
                    if "error" in reply:
                        raise RuntimeError(reply["error"])
                    return reply.get("text", "").strip()
                except (BrokenPipeError, ConnectionResetError, RuntimeError):
                    if self.proc and self.proc.returncode is None:
                        self.proc.kill()
                    self.proc = None
                    if attempt == 1:
                        raise
        return ""


# One `tenx-whisper` per (program, model, language, glossary) asked for.
WHISPERS: dict[tuple, Whisper] = {}


def whisper_for(setup: Setup) -> Whisper:
    key = (setup.whisper, setup.whisper_model, setup.spoken, setup.whisper_prompt_file)
    if key not in WHISPERS:
        WHISPERS[key] = Whisper(*key)
    return WHISPERS[key]


class TenxWhisperSTT(SegmentedSTTService):
    """Each utterance the VAD cut, through `tenx-whisper`."""

    def __init__(self, whisper: Whisper, heard, **kwargs):
        super().__init__(**kwargs)
        self.whisper = whisper
        self.heard = heard

    @property
    def wants_wav_segments(self) -> bool:
        return False

    def can_generate_metrics(self) -> bool:
        return True

    async def run_stt(self, audio: bytes):
        samples = np.frombuffer(audio, dtype=np.int16).astype(np.float32) / 32768.0
        if self.sample_rate != Whisper.RATE:
            n = int(len(samples) * Whisper.RATE / self.sample_rate)
            samples = np.interp(np.linspace(0, len(samples), n, endpoint=False), np.arange(len(samples)), samples)
        await self.start_processing_metrics()
        try:
            text = await self.whisper.transcribe(samples.astype(np.float32))
        except Exception as e:
            yield ErrorFrame(error=f"tenx-whisper: {e}")
            return
        finally:
            await self.stop_processing_metrics()
        if text:
            logger.debug(f"Transcription: [{text}]")
            # The whole utterance, never revised: the turn may end now.
            yield TranscriptionFrame(text, self._user_id, time_now_iso8601(), self.heard, finalized=True)


WORD = re.compile(r"[^\W_]+")


def respeller(say: dict):
    """`tenx_core::readaloud::respell`: each word in `say`, matched whole as
    a run of letters and digits, in any case, said its way. Only the voice
    hears it; the captions keep the word as written."""

    async def respell(text: str, text_type) -> str:
        return WORD.sub(lambda m: say.get(m.group(0).lower(), m.group(0)), text) if say else text

    return respell


TASK_LINE = re.compile(r'^\S+ "(.*)" (\w+)')


async def greeting(mcp: MCPClient) -> str:
    """A friendly hello, and the tasks that need the user, by title."""
    try:
        # Through `compact_tasks`: `ws/slug "title" status — …`, a line each.
        listing = await mcp._call_tool_text("tenx_list_tasks", {})
    except Exception as e:
        logger.warning(f"greeting: {e}")
        return "Hi! What can I do for you?"
    need = []
    for line in listing.splitlines():
        m = TASK_LINE.match(line)
        if m and (m.group(2) in ("blocked", "signaled") or "secrets pending:" in line):
            need.append(m.group(1))
    if not need:
        return "Hi! What can I do for you?"
    if len(need) == 1:
        return f"Hi! {need[0]} needs you."
    if len(need) <= 3:
        return f"Hi! {', '.join(need[:-1])} and {need[-1]} need you."
    return f"Hi! {len(need)} tasks need you: {', '.join(need[:2])} and {len(need) - 2} more."


class Hush(FrameProcessor):
    """Says a response only once it is whole, and not at all when it is
    `QUIET` (the assistant's way to say nothing) or leads to a tool call —
    "I'll check that first." is what the model says however it is told not
    to. Responses are a sentence or two, so waiting for the whole costs a
    fraction of a second."""

    def __init__(self):
        super().__init__()
        self.held = ""
        self.calling = False

    async def process_frame(self, frame: Frame, direction: FrameDirection):
        await super().process_frame(frame, direction)
        if isinstance(frame, LLMFullResponseStartFrame):
            self.held, self.calling = "", False
        elif isinstance(frame, LLMTextFrame) and direction == FrameDirection.DOWNSTREAM:
            self.held += frame.text
            return
        elif isinstance(frame, FunctionCallsStartedFrame):
            self.calling = True
        elif isinstance(frame, LLMFullResponseEndFrame):
            text = self.held.strip()
            logger.debug(f"assistant said: {text!r}{' (before a tool call)' if self.calling else ''}")
            if text and not self.calling and not text.startswith(QUIET):
                await self.push_frame(LLMTextFrame(text), direction)
        await self.push_frame(frame, direction)


# When the user stopped mid-sentence: no nudging ("go ahead, I'm
# listening") — the default prompts' — after the wait, only an answer to what
# they said so far, if it asks for one.
AFTER_PAUSE = f"""The user stopped talking. Take everything they said since your last
reply as one message. If it asks you something or asks you to do something,
answer now: {DONE} and your reply. Otherwise — also when you heard nothing new —
write {DONE} {QUIET} and nothing else. Never say you didn't catch something.
Do NOT output {CUT} or {WAIT}."""


def speech_to_text(setup: Setup):
    if setup.whisper_model:
        return TenxWhisperSTT(
            whisper_for(setup),
            setup.heard,
            settings=STTSettings(model=setup.whisper_model, language=setup.heard),
            # How long the turn may wait for a transcript (it ends as soon as
            # one arrives, `finalized`): longer than tenx-whisper ever takes,
            # or a turn ends on the text before it and is answered twice.
            ttfs_p99_latency=3.0,
        )
    from pipecat.services.whisper.stt import WhisperSTTServiceMLX

    return WhisperSTTServiceMLX(model=setup.mlx_model, language=setup.heard)


class Talking(BaseObserver):
    """Whether someone is speaking, so an announcement never cuts in."""

    def __init__(self):
        super().__init__()
        self.user = False
        self.bot = False

    async def on_push_frame(self, data: FramePushed):
        f = data.frame
        if isinstance(f, UserStartedSpeakingFrame):
            self.user = True
        elif isinstance(f, UserStoppedSpeakingFrame):
            self.user = False
        elif isinstance(f, BotStartedSpeakingFrame):
            self.bot = True
        elif isinstance(f, BotStoppedSpeakingFrame):
            self.bot = False

    def quiet(self) -> bool:
        return not self.user and not self.bot


class Session:
    """One connected page: its pipeline, and what it is told."""

    def __init__(self, link: "Link", connection, task: str | None, page: str | None):
        self.link = link
        # What it runs with: the setup when it started.
        self.setup = link.setup
        self.connection = connection
        self.task = task
        # The page's tenx web session: `tenx_focus_task` shows tasks there.
        self.page = page
        self.talking = Talking()
        self.pending: list[str] = []
        # The page paused the conversation: it hears nothing (its microphone
        # is off) and nothing is said until it goes on.
        self.paused = False
        self.worker: PipelineWorker | None = None
        self.runner: WorkerRunner | None = None

    async def run(self):
        transport = SmallWebRTCTransport(
            webrtc_connection=self.connection,
            params=TransportParams(audio_in_enabled=True, audio_out_enabled=True),
        )
        setup = self.setup
        stt = speech_to_text(setup)
        tts = KokoroTTSService(
            settings=KokoroTTSService.Settings(voice=setup.voice), text_transforms=[("*", respeller(setup.say))]
        )
        llm = AnthropicLLMService(
            api_key=setup.api_key,
            settings=AnthropicLLMService.Settings(model=setup.claude, system_instruction=SYSTEM),
        )
        mcp = MCPClient(
            server_params=StreamableHttpParameters(
                url=setup.mcp_url,
                headers={"Authorization": f"Bearer {setup.mcp_token}"}
                | ({"tenx-session": self.page} if self.page else {}),
            ),
            tools_filter=TOOLS,
            tools_output_filters={name: clip for name in TOOLS} | {"tenx_list_tasks": compact_tasks},
        )
        context = LLMContext(tools=await mcp.tools())

        @llm.event_handler("on_function_calls_started")
        async def on_calls(service, calls):
            for call in calls:
                if call.function_name in FOLLOWS:
                    await self.follow(mcp, call.arguments)
        for name in ACTS & set(TOOLS):
            llm.register_function(name, mcp._tool_wrapper_with_cleanup, cancel_on_interruption=False)
        user, assistant = LLMContextAggregatorPair(
            context,
            user_params=LLMUserAggregatorParams(
                # 0.6 s, not 0.2: a breath mid-sentence must not cut it in
                # two pieces, each of which the model would answer.
                vad_analyzer=SileroVADAnalyzer(params=VADParams(stop_secs=0.6)),
                # The model tells a finished message from a pause mid-sentence.
                user_turn_strategies=FilterIncompleteUserTurnStrategies(
                    config=UserTurnCompletionConfig(
                        complete_marker=DONE,
                        incomplete_short_marker=CUT,
                        incomplete_long_marker=WAIT,
                        incomplete_short_timeout=6.0,
                        incomplete_long_timeout=15.0,
                        incomplete_short_prompt=AFTER_PAUSE,
                        incomplete_long_prompt=AFTER_PAUSE,
                    )
                ),
            ),
        )
        pipeline = Pipeline([transport.input(), stt, user, llm, Hush(), tts, transport.output(), assistant])
        self.worker = PipelineWorker(
            pipeline,
            params=PipelineParams(enable_metrics=True, enable_usage_metrics=True),
            observers=[self.talking],
        )
        self.runner = WorkerRunner(handle_sigint=False)
        await self.runner.add_workers(self.worker)

        @transport.event_handler("on_client_connected")
        async def on_connected(transport, client):
            # A fixed greeting from the task list, not the model's: it can't
            # resist saying what it is about to look up first.
            looking = f" They are looking at task {self.task}." if self.task else ""
            context.add_message({"role": "user", "content": "[tenx] The conversation starts now." + looking})
            await self.worker.queue_frames([TTSSpeakFrame(await greeting(mcp))])

        @transport.event_handler("on_client_disconnected")
        async def on_disconnected(transport, client):
            await self.runner.cancel()

        await self.runner.run()

    async def end(self):
        if self.runner:
            await self.runner.cancel()

    async def follow(self, mcp: MCPClient, args):
        """Show the task a tool is about, unless the page shows it already."""
        ws, task = (args or {}).get("workspace"), (args or {}).get("task")
        if not self.page or not ws or not task or f"{ws}/{task}" == self.task:
            return
        try:
            await mcp._call_tool_text("tenx_focus_task", {"workspace": ws, "task": task})
            # The page reports the change too (`context`); this keeps a
            # second call for the same task from showing it again.
            self.task = f"{ws}/{task}"
        except Exception as e:
            logger.warning(f"tenx_focus_task {ws}/{task}: {e}")

    async def tell(self, text: str):
        """Queue `text` for the assistant; said when nobody is talking."""
        self.pending.append(text)

    async def flush(self):
        if self.worker and self.pending and not self.paused and self.talking.quiet():
            text = "[tenx] " + " ".join(self.pending)
            self.pending = []
            await self.worker.queue_frames(
                [LLMMessagesAppendFrame(messages=[{"role": "user", "content": text}], run_llm=True)]
            )

    async def pause(self, paused: bool):
        self.paused = paused
        if paused and self.worker:
            # Mid-sentence: stop talking now, as when the user cuts in.
            await self.worker.queue_frames([InterruptionFrame()])

    async def look_at(self, task: str | None):
        if task == self.task:
            return
        self.task = task
        if self.worker:
            # Context, not news: no reply until the user says something.
            text = f"[tenx] The user is now looking at task {task}." if task else "[tenx] The user is looking at no task now."
            await self.worker.queue_frames(
                [LLMMessagesAppendFrame(messages=[{"role": "user", "content": text}], run_llm=False)]
            )


class Link:
    """One `tenx web` connected through the socket: its setup, its pages, and
    where their replies go."""

    def __init__(self, voice: "Voice", writer: asyncio.StreamWriter):
        self.voice = voice
        self.writer = writer
        self.setup: Setup | None = None
        self.sessions: dict[str, Session] = {}

    def send(self, msg: dict):
        if not self.writer.is_closing():
            self.writer.write((json.dumps(msg) + "\n").encode())

    def count_changed(self):
        self.send({"type": "sessions", "count": len(self.sessions)})

    async def offer(self, msg: dict):
        if not self.setup:
            raise RuntimeError("no setup from tenx web yet")
        if not self.setup.api_key:
            raise RuntimeError("no Anthropic API key: put it in ~/.config/tenx/anthropic-key (chmod 600)")
        task = msg.get("task")
        page = msg.get("session")

        async def on_connection(connection):
            session = Session(self, connection, task, page)

            async def run():
                pc_id = connection.pc_id
                self.sessions[pc_id] = session
                self.count_changed()
                try:
                    await session.run()
                except Exception as e:
                    logger.exception(f"voice session {pc_id} failed: {e}")
                finally:
                    self.sessions.pop(pc_id, None)
                    self.count_changed()

            asyncio.create_task(run())

        request = SmallWebRTCRequest(
            sdp=msg["sdp"],
            type=msg["sdp_type"],
            pc_id=msg.get("pc_id"),
            restart_pc=msg.get("restart_pc", False),
        )
        answer = await self.voice.handler.handle_web_request(request, on_connection)
        self.send({"type": "answer", "id": msg["id"], "sdp": answer["sdp"], "sdp_type": answer["type"], "pc_id": answer["pc_id"]})

    async def ice(self, msg: dict):
        candidates = [IceCandidate(c["candidate"], c["sdp_mid"], c["sdp_mline_index"]) for c in msg["candidates"]]
        await self.voice.handler.handle_patch_request(SmallWebRTCPatchRequest(pc_id=msg["pc_id"], candidates=candidates))
        self.send({"type": "ok", "id": msg["id"]})

    async def handle(self, msg: dict):
        kind = msg.get("type")
        try:
            if kind == "setup":
                # New sessions run with it; a running one keeps its own.
                self.setup = Setup.of(msg)
                self.send({"type": "ready"})
            elif kind == "offer":
                await self.offer(msg)
            elif kind == "ice":
                await self.ice(msg)
            elif kind == "context":
                session = self.sessions.get(msg["pc_id"])
                if session:
                    await session.look_at(msg.get("task"))
            elif kind == "pause":
                session = self.sessions.get(msg["pc_id"])
                if session:
                    await session.pause(bool(msg.get("paused")))
            elif kind == "event":
                for session in list(self.sessions.values()):
                    await session.tell(msg["text"])
        except Exception as e:
            logger.exception(f"{kind} failed: {e}")
            self.send({"type": "error", "id": msg.get("id"), "error": str(e) or type(e).__name__})

    async def serve(self, reader: asyncio.StreamReader):
        logger.info("tenx web connected")
        try:
            while line := await reader.readline():
                try:
                    msg = json.loads(line)
                except json.JSONDecodeError:
                    self.send({"type": "error", "error": f"not JSON: {line[:80]!r}"})
                    continue
                if msg.get("type") == "setup":
                    await self.handle(msg)  # in order: an offer after it uses it
                else:
                    asyncio.create_task(self.handle(msg))
        except ConnectionError:
            pass
        finally:
            logger.info("tenx web went away: ending its conversations")
            for session in list(self.sessions.values()):
                await session.end()
            self.writer.close()


class Voice:
    def __init__(self):
        self.handler = SmallWebRTCRequestHandler()
        self.links: set[Link] = set()

    async def connected(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
        link = Link(self, writer)
        self.links.add(link)
        try:
            await link.serve(reader)
        finally:
            self.links.discard(link)

    async def flusher(self):
        while True:
            await asyncio.sleep(0.5)
            for link in list(self.links):
                for session in list(link.sessions.values()):
                    try:
                        await session.flush()
                    except Exception as e:
                        logger.warning(f"announcement failed: {e}")


async def listen(path: str, voice: Voice):
    """The socket, only for this user. One that answers is another instance:
    leave it be. One that doesn't was left behind by a killed one."""
    if os.path.exists(path):
        try:
            _, w = await asyncio.open_unix_connection(path)
            w.close()
            raise SystemExit(f"tenx-voice already runs ({path})")
        except (ConnectionRefusedError, FileNotFoundError):
            os.unlink(path)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    old = os.umask(0o177)
    try:
        server = await asyncio.start_unix_server(voice.connected, path)
    finally:
        os.umask(old)
    os.chmod(path, stat.S_IRUSR | stat.S_IWUSR)
    return server


async def main():
    voice = Voice()
    server = await listen(SOCKET, voice)
    asyncio.create_task(voice.flusher())
    logger.info(f"tenx-voice listens on {SOCKET}; tenx web connects within a couple of seconds")
    try:
        async with server:
            await server.serve_forever()
    finally:
        await voice.handler.close()
        if os.path.exists(SOCKET):
            os.unlink(SOCKET)


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
