//! `tenx-tts --model <kokoro dir> [--voice <name|id>] [--threads <n>]`: text
//! to speech for `tenx web`'s speaker, one piece of text at a time, over
//! stdin/stdout (the conversation: `tenx_core::tts`). `tenx web` starts it
//! on the first reading and ends it — by closing its stdin — when unused for
//! a while; nobody runs it by hand.
//!
//! The model is Kokoro as sherpa-onnx packs it (`kokoro-multi-lang-v1_0`:
//! `model.onnx`, `voices.bin`, `tokens.txt`, `espeak-ng-data/`, lexicons),
//! run by sherpa-onnx's prebuilt static libraries — which exist for macOS
//! and glibc Linux (Cargo.toml); elsewhere this is a stub that says so.

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn main() {
    engine::main()
}

#[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
fn main() {
    use tenx_core::tts::{Reply, reply_line};
    let error = "reading aloud needs a tenx-tts built for macOS or glibc Linux".to_string();
    print!("{}", reply_line(&Reply::Error { error }));
    std::process::exit(1);
}

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
mod engine {
    use std::fs::File;
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::FromRawFd;
    use std::path::Path;

    use anyhow::{Context, Result, bail};
    use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig, OfflineTtsModelConfig};
    use tenx_core::tts::{self, Reply, Request};

    struct Args {
        model: String,
        voice: i32,
        threads: i32,
    }

    fn args() -> Result<Args> {
        let mut a = Args { model: String::new(), voice: tts::voice_id(tts::DEFAULT_VOICE).unwrap_or(0), threads: 0 };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
            match arg.as_str() {
                "--model" => a.model = value()?,
                "--voice" => {
                    let v = value()?;
                    a.voice = tts::voice_id(&v).with_context(|| format!("no voice {v:?} (e.g. af_heart, bm_george)"))?;
                }
                "--threads" => a.threads = value()?.parse().context("--threads")?,
                "--version" => {
                    println!("tenx-tts {}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                _ => bail!("unknown argument {arg} (see `tenx web` — it runs this)"),
            }
        }
        if a.model.is_empty() {
            bail!("--model <kokoro directory> is required");
        }
        if a.threads <= 0 {
            a.threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8) as i32);
        }
        Ok(a)
    }

    /// The real stdout, for replies only; fd 1 itself goes to stderr, so nothing
    /// sherpa-onnx or onnxruntime prints can land in the middle of a reply.
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

    fn send(out: &mut File, reply: &Reply, samples: &[f32]) -> Result<()> {
        let mut msg = tts::reply_line(reply).into_bytes();
        msg.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        out.write_all(&msg)?;
        out.flush()?;
        Ok(())
    }

    pub fn main() {
        let mut out = match private_stdout() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("tenx-tts: {e:#}");
                std::process::exit(1);
            }
        };
        if let Err(e) = run(&mut out) {
            let _ = send(&mut out, &Reply::Error { error: format!("{e:#}") }, &[]);
            std::process::exit(1);
        }
    }

    /// The model directory's file `name`, as sherpa-onnx wants it.
    fn file(dir: &Path, name: &str) -> Result<String> {
        let p = dir.join(name);
        if !p.exists() {
            bail!("{} has no {name} — is it a Kokoro model from sherpa-onnx (kokoro-multi-lang-v1_0)?", dir.display());
        }
        Ok(p.to_string_lossy().into_owned())
    }

    fn load(args: &Args) -> Result<OfflineTts> {
        let dir = Path::new(&args.model);
        // The full model; a directory that only has the int8 one works too
        // (slower on Apple silicon, smaller to download).
        let model = ["model.onnx", "model.int8.onnx"].into_iter().find(|m| dir.join(m).is_file()).unwrap_or("model.onnx");
        let (lang, lexicon) = tts::voice_language(args.voice);
        let lexicon = match lexicon {
            Some(l) if dir.join(l).is_file() => Some(file(dir, l)?),
            _ => None,
        };
        let dict = dir.join("dict");
        let config = OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                kokoro: OfflineTtsKokoroModelConfig {
                    model: Some(file(dir, model)?),
                    voices: Some(file(dir, "voices.bin")?),
                    tokens: Some(file(dir, "tokens.txt")?),
                    data_dir: Some(file(dir, "espeak-ng-data")?),
                    dict_dir: dict.is_dir().then(|| dict.to_string_lossy().into_owned()),
                    lexicon,
                    lang: Some(lang.to_string()),
                    ..Default::default()
                },
                num_threads: args.threads,
                ..Default::default()
            },
            ..Default::default()
        };
        let tts = OfflineTts::create(&config).with_context(|| format!("load the Kokoro model in {}", dir.display()))?;
        if args.voice >= tts.num_speakers() {
            bail!("the model in {} has {} voices; this is voice {}", dir.display(), tts.num_speakers(), args.voice);
        }
        Ok(tts)
    }

    fn run(out: &mut File) -> Result<()> {
        let args = args()?;
        let engine = load(&args)?;
        let sample_rate = u32::try_from(engine.sample_rate()).context("the model's sample rate")?;
        send(out, &Reply::Ready { ready: true, sample_rate }, &[])?;

        let mut input = BufReader::new(std::io::stdin().lock());
        let mut line = String::new();
        loop {
            line.clear();
            if input.read_line(&mut line)? == 0 {
                return Ok(()); // tenx web is done with us
            }
            let req: Request = serde_json::from_str(line.trim()).context("a request line")?;
            let config = GenerationConfig { sid: args.voice, speed: req.speed.clamp(0.5, 2.0), ..Default::default() };
            match engine.generate_with_config::<fn(&[f32], f32) -> bool>(&req.text, &config, None) {
                Some(audio) => send(out, &Reply::Audio { samples: audio.samples().len() }, audio.samples())?,
                None => send(out, &Reply::Error { error: "Kokoro made no audio".into() }, &[])?,
            }
        }
    }
}
