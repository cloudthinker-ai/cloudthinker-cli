use std::io::Write as _;
use std::time::Instant;

pub struct PhaseTimer {
    start: Option<Instant>,
    previous_ms: f64,
}

impl PhaseTimer {
    pub fn from_env() -> Self {
        Self {
            start: (std::env::var("CLOUDTHINKER_TIMING").as_deref() == Ok("1")).then(Instant::now),
            previous_ms: 0.0,
        }
    }

    pub fn mark(&mut self, phase: &str) {
        if let Some(start) = self.start {
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            let _ = writeln!(
                std::io::stderr(),
                "[cloudthinker timing] {{\"phase\":\"{phase}\",\"elapsed_ms\":{elapsed_ms:.3},\"phase_ms\":{:.3}}}",
                elapsed_ms - self.previous_ms
            );
            self.previous_ms = elapsed_ms;
        }
    }
}
