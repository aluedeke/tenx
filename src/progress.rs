use std::io::Write;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

pub struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    label: String,
    finished: bool,
}

impl Spinner {
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let msg = label.clone();

        let handle = std::thread::spawn(move || {
            const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0usize;
            loop {
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                print!("\r  {} {}…", FRAMES[i % FRAMES.len()], msg);
                std::io::stdout().flush().ok();
                std::thread::sleep(Duration::from_millis(80));
                i += 1;
            }
        });

        Spinner { stop, handle: Some(handle), label, finished: false }
    }

    pub fn done(mut self) {
        let label = self.label.clone();
        self.finish_with(&format!("✓ {label}"));
    }

    pub fn fail(mut self, err: &str) {
        let label = self.label.clone();
        self.finish_with(&format!("✗ {label}  {err}"));
    }

    fn finish_with(&mut self, line: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
        // \r\x1b[2K — carriage return + erase line, then print final state.
        println!("\r\x1b[2K  {line}");
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        if !self.finished {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                h.join().ok();
            }
            println!();
        }
    }
}
