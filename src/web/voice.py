# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#     "pipecat-ai[anthropic,kokoro,mcp,mlx-whisper,runner,silero,webrtc,whisper]==1.15.0",
# ]
# ///
"""tenx-voice: the voice assistant behind `tenx web`'s talk mode.

`tenx web` writes this file out and runs it with `uv run --script`. One
process serves every connected page: a Pipecat pipeline per WebRTC
connection — Silero VAD, speech to text, Claude, Kokoro — whose tools are
tenx's own, through `tenx web`'s `/mcp`.

Speech to text is `tenx-whisper` with `[speech]`'s model and glossary — the
microphone button's, a model tuned to its user's voice and words — one
process for every session, spoken to as `tenx web` does
(`tenx_core::whisper`). Without a `[speech]` model: MLX Whisper.

The conversation with `tenx web` is `tenx_core::voice`: JSON lines on stdin
(`offer`, `ice`, `context`, `pause`, `event`) and on stdout (`ready`, `answer`, `ok`,
`error`, `sessions`). Logs go to stderr; nothing else may reach stdout, so
the protocol gets the original stdout and fd 1 is pointed at stderr.

The `runner` extra is only for fastapi, which the WebRTC request handler
imports.

Environment, set by `tenx web`: ANTHROPIC_API_KEY, TENX_MCP_URL,
TENX_MCP_TOKEN, TENX_VOICE_MODEL, TENX_VOICE_TTS, TENX_VOICE_STT,
TENX_VOICE_LANGUAGE; for `tenx-whisper`: TENX_WHISPER (the program),
TENX_WHISPER_MODEL, TENX_WHISPER_PROMPT_FILE.
"""

import asyncio
import json
import os
import re
import sys

# The protocol's stdout, before anything can print to it.
PROTO = os.fdopen(os.dup(1), "w", buffering=1)
os.dup2(2, 1)

from loguru import logger  # noqa: E402
from mcp.client.session_group import StreamableHttpParameters  # noqa: E402

from pipecat.audio.vad.silero import SileroVADAnalyzer  # noqa: E402
import numpy as np  # noqa: E402

from pipecat.frames.frames import (  # noqa: E402
    BotStartedSpeakingFrame,
    ErrorFrame,
    InterruptionFrame,
    TranscriptionFrame,
    BotStoppedSpeakingFrame,
    LLMMessagesAppendFrame,
    LLMRunFrame,
    UserStartedSpeakingFrame,
    UserStoppedSpeakingFrame,
)
from pipecat.observers.base_observer import BaseObserver, FramePushed  # noqa: E402
from pipecat.pipeline.pipeline import Pipeline  # noqa: E402
from pipecat.pipeline.worker import PipelineParams, PipelineWorker  # noqa: E402
from pipecat.processors.aggregators.llm_context import LLMContext  # noqa: E402
from pipecat.processors.aggregators.llm_response_universal import (  # noqa: E402
    LLMContextAggregatorPair,
    LLMUserAggregatorParams,
)
from pipecat.services.anthropic.llm import AnthropicLLMService  # noqa: E402
from pipecat.services.kokoro.tts import KokoroTTSService  # noqa: E402
from pipecat.services.mcp_service import MCPClient  # noqa: E402
from pipecat.services.settings import STTSettings  # noqa: E402
from pipecat.services.stt_service import SegmentedSTTService  # noqa: E402
from pipecat.transcriptions.language import Language  # noqa: E402
from pipecat.transports.base_transport import TransportParams  # noqa: E402
from pipecat.transports.smallwebrtc.request_handler import (  # noqa: E402
    IceCandidate,
    SmallWebRTCPatchRequest,
    SmallWebRTCRequest,
    SmallWebRTCRequestHandler,
)
from pipecat.transports.smallwebrtc.transport import SmallWebRTCTransport  # noqa: E402
from pipecat.utils.time import time_now_iso8601  # noqa: E402
from pipecat.workers.runner import WorkerRunner  # noqa: E402

MODEL = os.environ.get("TENX_VOICE_MODEL") or "claude-haiku-5-5"
TTS_VOICE = os.environ.get("TENX_VOICE_TTS") or "af_heart"
STT_MODEL = os.environ.get("TENX_VOICE_STT") or "mlx-community/whisper-large-v3-turbo"
LANGUAGE = os.environ.get("TENX_VOICE_LANGUAGE") or "en"
WHISPER = os.environ.get("TENX_WHISPER") or "tenx-whisper"
WHISPER_MODEL = os.environ.get("TENX_WHISPER_MODEL") or ""
WHISPER_PROMPT_FILE = os.environ.get("TENX_WHISPER_PROMPT_FILE") or ""
# Words the voice gets wrong: [["tenx", "ten x"], …], from `[say]` and the
# built-in ones (`tenx_core::readaloud::say_table`).
SAY = dict(json.loads(os.environ.get("TENX_VOICE_SAY") or "[]"))

# Long tool output is for the model to sum up, not to read out; this keeps
# each turn's context — and its latency — small.
MAX_TOOL_CHARS = 6000

# Tools about one task: using one shows that task on the page first
# (`Session.follow`), so its history is on screen while the assistant talks
# about it — not left to the model, which forgets.
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

Your words are spoken aloud: plain sentences, no Markdown, no lists, no code,
no URLs, no file paths unless asked. Be brief — one or two sentences unless
the user asks for more. Call tasks by their title, not their slug, unless two
share a title.

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
approving or denying a permission, creating a task — say in one short
sentence exactly what you will do and wait for the user to confirm. Speech
recognition mishears; never act on a guess.

Messages that start with "[tenx]" come from tenx, not from the user. They
say which task the user is looking at ("it", "this task" mean that one), or
that something happened: tell the user about it in one short sentence.
"""


def send(msg: dict):
    PROTO.write(json.dumps(msg) + "\n")


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

    def __init__(self):
        self.proc: asyncio.subprocess.Process | None = None
        self.lock = asyncio.Lock()

    async def _start(self):
        args = [WHISPER, "--model", WHISPER_MODEL, "--language", LANGUAGE]
        if WHISPER_PROMPT_FILE:
            args += ["--prompt-file", WHISPER_PROMPT_FILE]
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


WHISPER_PROGRAM = Whisper()


class TenxWhisperSTT(SegmentedSTTService):
    """Each utterance the VAD cut, through `tenx-whisper`."""

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
            text = await WHISPER_PROGRAM.transcribe(samples.astype(np.float32))
        except Exception as e:
            yield ErrorFrame(error=f"tenx-whisper: {e}")
            return
        finally:
            await self.stop_processing_metrics()
        if text:
            logger.debug(f"Transcription: [{text}]")
            yield TranscriptionFrame(text, self._user_id, time_now_iso8601(), Language(LANGUAGE))


WORD = re.compile(r"[^\W_]+")


async def respell(text: str, text_type) -> str:
    """`tenx_core::readaloud::respell`: each word in SAY, matched whole as a
    run of letters and digits, in any case, said its way. Only the voice
    hears it; the captions keep the word as written."""
    return WORD.sub(lambda m: SAY.get(m.group(0).lower(), m.group(0)), text) if SAY else text


def speech_to_text():
    if WHISPER_MODEL:
        return TenxWhisperSTT(settings=STTSettings(model=WHISPER_MODEL, language=Language(LANGUAGE)))
    from pipecat.services.whisper.stt import WhisperSTTServiceMLX

    return WhisperSTTServiceMLX(model=STT_MODEL, language=Language(LANGUAGE))


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

    def __init__(self, voice: "Voice", connection, task: str | None, page: str | None):
        self.voice = voice
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
        stt = speech_to_text()
        tts = KokoroTTSService(settings=KokoroTTSService.Settings(voice=TTS_VOICE), text_transforms=[("*", respell)])
        llm = AnthropicLLMService(
            api_key=os.environ["ANTHROPIC_API_KEY"],
            settings=AnthropicLLMService.Settings(model=MODEL, system_instruction=SYSTEM),
        )
        mcp = MCPClient(
            server_params=StreamableHttpParameters(
                url=os.environ["TENX_MCP_URL"],
                headers={"Authorization": f"Bearer {os.environ['TENX_MCP_TOKEN']}"}
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
        user, assistant = LLMContextAggregatorPair(
            context, user_params=LLMUserAggregatorParams(vad_analyzer=SileroVADAnalyzer())
        )
        pipeline = Pipeline([transport.input(), stt, user, llm, tts, transport.output(), assistant])
        self.worker = PipelineWorker(
            pipeline,
            params=PipelineParams(enable_metrics=True, enable_usage_metrics=True),
            observers=[self.talking],
        )
        self.runner = WorkerRunner(handle_sigint=False)
        await self.runner.add_workers(self.worker)

        @transport.event_handler("on_client_connected")
        async def on_connected(transport, client):
            looking = f" They are looking at task {self.task}." if self.task else ""
            context.add_message(
                {
                    "role": "user",
                    "content": "[tenx] The conversation starts now." + looking + " Greet them in one "
                    "short sentence, speaking to them as you, and say which of their tasks need them, if any.",
                }
            )
            await self.worker.queue_frames([LLMRunFrame()])

        @transport.event_handler("on_client_disconnected")
        async def on_disconnected(transport, client):
            await self.runner.cancel()

        await self.runner.run()

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


class Voice:
    def __init__(self):
        self.handler = SmallWebRTCRequestHandler()
        self.sessions: dict[str, Session] = {}

    def count_changed(self):
        send({"type": "sessions", "count": len(self.sessions)})

    async def offer(self, msg: dict):
        task = msg.get("task")
        page = msg.get("session")
        started: list[Session] = []

        async def on_connection(connection):
            session = Session(self, connection, task, page)
            started.append(session)

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
        answer = await self.handler.handle_web_request(request, on_connection)
        send({"type": "answer", "id": msg["id"], "sdp": answer["sdp"], "sdp_type": answer["type"], "pc_id": answer["pc_id"]})

    async def ice(self, msg: dict):
        candidates = [IceCandidate(c["candidate"], c["sdp_mid"], c["sdp_mline_index"]) for c in msg["candidates"]]
        await self.handler.handle_patch_request(SmallWebRTCPatchRequest(pc_id=msg["pc_id"], candidates=candidates))
        send({"type": "ok", "id": msg["id"]})

    async def handle(self, msg: dict):
        kind = msg.get("type")
        try:
            if kind == "offer":
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
            send({"type": "error", "id": msg.get("id"), "error": str(e) or type(e).__name__})

    async def flusher(self):
        while True:
            await asyncio.sleep(0.5)
            for session in list(self.sessions.values()):
                try:
                    await session.flush()
                except Exception as e:
                    logger.warning(f"announcement failed: {e}")


async def main():
    voice = Voice()
    loop = asyncio.get_running_loop()
    reader = asyncio.StreamReader()
    await loop.connect_read_pipe(lambda: asyncio.StreamReaderProtocol(reader), sys.stdin)
    asyncio.create_task(voice.flusher())
    send({"type": "ready"})
    while line := await reader.readline():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            send({"type": "error", "error": f"not JSON: {line[:80]!r}"})
            continue
        asyncio.create_task(voice.handle(msg))
    # EOF: tenx web is done with us.
    await voice.handler.close()


if __name__ == "__main__":
    asyncio.run(main())
