//! `tenx-whisper --model <ggml file> [--prompt-file <file>] [--language <code>]
//! [--threads <n>] [--verbose]`: speech to text for `tenx web`, one recording
//! at a time, over stdin/stdout (the conversation: `tenx_core::whisper`).
//! `tenx web` starts it on the first recording and ends it — by closing its
//! stdin — when unused for a while; nobody runs it by hand.
//!
//! whisper.cpp is only built in on Apple silicon (Cargo.toml); elsewhere this
//! is a stub that answers every start with an error saying so.

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn main() {
    engine::main()
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn main() {
    use tenx_core::whisper::{Reply, reply_line};
    let error = "speech to text needs a Mac with Apple silicon".to_string();
    print!("{}", reply_line(&Reply::Error { error }));
    std::process::exit(1);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod engine {
    use std::fs::File;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::fd::FromRawFd;

    use anyhow::{Context, Result, bail};
    use tenx_core::whisper::{self, Reply, Request};
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

    struct Args {
        model: String,
        prompt: String,
        language: String,
        threads: i32,
        verbose: bool,
    }

    fn args() -> Result<Args> {
        let mut a = Args { model: String::new(), prompt: String::new(), language: "auto".into(), threads: 0, verbose: false };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
            match arg.as_str() {
                "--model" => a.model = value()?,
                "--prompt-file" => {
                    let path = value()?;
                    a.prompt = std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?.trim().to_string();
                }
                "--language" => a.language = value()?,
                "--threads" => a.threads = value()?.parse().context("--threads")?,
                "--verbose" => a.verbose = true,
                "--version" => {
                    println!("tenx-whisper {}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                _ => bail!("unknown argument {arg} (see `tenx web` — it runs this)"),
            }
        }
        if a.model.is_empty() {
            bail!("--model <ggml file> is required");
        }
        if a.threads <= 0 {
            a.threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8) as i32);
        }
        Ok(a)
    }

    /// The real stdout, for replies only; fd 1 itself goes to stderr, so nothing
    /// whisper.cpp prints can land in the middle of a reply.
    fn private_stdout() -> Result<File> {
        // SAFETY: plain descriptor calls on this process's own fds 1 and 2.
        unsafe {
            let fd = libc::dup(1);
            if fd < 0 || libc::dup2(2, 1) < 0 {
                bail!("redirect stdout: {}", std::io::Error::last_os_error());
            }
            Ok(File::from_raw_fd(fd))
        }
    }

    fn send(out: &mut File, reply: Reply) -> Result<()> {
        out.write_all(whisper::reply_line(&reply).as_bytes())?;
        out.flush()?;
        Ok(())
    }

    pub fn main() {
        let mut out = match private_stdout() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("tenx-whisper: {e:#}");
                std::process::exit(1);
            }
        };
        if let Err(e) = run(&mut out) {
            let _ = send(&mut out, Reply::Error { error: format!("{e:#}") });
            std::process::exit(1);
        }
    }

    fn run(out: &mut File) -> Result<()> {
        let args = args()?;
        if !args.verbose {
            whisper_rs::install_logging_hooks();
        }
        let ctx = WhisperContext::new_with_params(&args.model, WhisperContextParameters::default())
            .map_err(|e| anyhow::anyhow!("load the model {}: {e}", args.model))?;
        let mut state = ctx.create_state().map_err(|e| anyhow::anyhow!("whisper state: {e}"))?;
        send(out, Reply::Ready { ready: true })?;

        let mut input = BufReader::new(std::io::stdin().lock());
        let mut line = String::new();
        loop {
            line.clear();
            if input.read_line(&mut line)? == 0 {
                return Ok(()); // tenx web is done with us
            }
            let req: Request = serde_json::from_str(line.trim()).context("a request line")?;
            let mut bytes = vec![0u8; req.samples * 4];
            input.read_exact(&mut bytes).context("the request's samples")?;
            let audio = whisper::bytes_samples(&bytes);
            let reply = match transcribe(&mut state, &args, &req, &audio) {
                Ok(reply) => reply,
                Err(e) => Reply::Error { error: format!("{e:#}") },
            };
            send(out, reply)?;
        }
    }

    fn transcribe(state: &mut WhisperState, args: &Args, req: &Request, audio: &[f32]) -> Result<Reply> {
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 2 });
        p.set_language(Some(req.language.as_deref().unwrap_or(&args.language)));
        if req.fallback == Some(false) {
            p.set_temperature_inc(0.0);
        }
        p.set_n_threads(args.threads);
        // Timestamps are what splits the text into segments with end times.
        p.set_no_timestamps(!req.segments);
        p.set_print_special(false);
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_timestamps(false);
        // The glossary, then what was said before this audio (live dictation).
        let prompt = whisper::join_segments([args.prompt.as_str(), req.prompt.as_deref().unwrap_or("")]);
        if !prompt.is_empty() {
            p.set_initial_prompt(&prompt);
        }
        state.full(p, audio).map_err(|e| anyhow::anyhow!("transcribe: {e}"))?;
        let segments: Vec<whisper::Segment> = state
            .as_iter()
            .map(|s| whisper::Segment { text: s.to_string().trim().to_string(), end: s.end_timestamp() as f32 / 100.0 })
            .collect();
        let text = whisper::join_segments(segments.iter().map(|s| s.text.as_str()));
        Ok(Reply::Text { text, segments: if req.segments { segments } else { vec![] } })
    }
}
