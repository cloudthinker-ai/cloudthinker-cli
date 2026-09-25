//! Generic poll loop with jittered backoff and bounded transport tolerance.
//!
//! `watch` is transport-agnostic: it takes an async `fetch` closure returning
//! `Poll::Pending` / `Poll::Terminal` and drives it until terminal, timeout, or
//! too many consecutive transport failures.

use std::future::Future;
use std::time::{Duration, Instant};

use cloudthinker_client::{CtError, CtResult};
use rand::Rng;

/// One poll outcome.
pub enum Poll<T> {
    Pending,
    Terminal(T),
}

/// Backoff + tolerance knobs. `for_run` builds the production profile; tests
/// override the delays to run fast.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub factor: f64,
    pub jitter: f64,
    pub max_transport_errors: u32,
    pub overall_timeout: Duration,
}

impl WatchConfig {
    /// Production profile: 2s → ×1.5 → cap 10s, ±20% jitter, tolerate 5
    /// consecutive transport errors.
    pub fn for_run(overall_timeout: Duration) -> Self {
        Self {
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(10),
            factor: 1.5,
            jitter: 0.2,
            max_transport_errors: 5,
            overall_timeout,
        }
    }
}

/// Poll `fetch` until it returns `Terminal`, the deadline passes, or transport
/// errors exceed the budget.
///
/// - A `Pending` resets the consecutive-error counter.
/// - Transport errors are tolerated up to `max_transport_errors`; the next one
///   aborts with that error.
/// - Any non-transport error aborts immediately.
/// - The deadline yields `CtError::Timeout`; the caller prints a resume hint.
pub async fn watch<T, F, Fut>(fetch: F, cfg: &WatchConfig) -> CtResult<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = CtResult<Poll<T>>>,
{
    let start = Instant::now();
    let mut delay = cfg.base_delay;
    let mut consecutive_errors: u32 = 0;

    loop {
        if start.elapsed() >= cfg.overall_timeout {
            return Err(CtError::Timeout(
                "run did not finish before the deadline".into(),
            ));
        }

        match fetch().await {
            Ok(Poll::Terminal(value)) => return Ok(value),
            Ok(Poll::Pending) => consecutive_errors = 0,
            Err(err) if err.is_transport() => {
                consecutive_errors += 1;
                if consecutive_errors > cfg.max_transport_errors {
                    return Err(err);
                }
            }
            Err(err) => return Err(err),
        }

        let remaining = cfg.overall_timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(CtError::Timeout(
                "run did not finish before the deadline".into(),
            ));
        }
        let nap = jittered(delay, cfg.jitter, cfg.max_delay).min(remaining);
        tokio::time::sleep(nap).await;
        delay = next_delay(delay, cfg.factor, cfg.max_delay);
    }
}

fn next_delay(current: Duration, factor: f64, max: Duration) -> Duration {
    let scaled = current.mul_f64(factor);
    scaled.min(max)
}

fn jittered(delay: Duration, jitter: f64, max: Duration) -> Duration {
    if jitter <= 0.0 {
        return delay.min(max);
    }
    let factor = 1.0 + rand::thread_rng().gen_range(-jitter..=jitter);
    delay.mul_f64(factor.max(0.0)).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fast_config(overall: Duration, max_errors: u32) -> WatchConfig {
        WatchConfig {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
            factor: 1.5,
            jitter: 0.0,
            max_transport_errors: max_errors,
            overall_timeout: overall,
        }
    }

    #[tokio::test]
    async fn terminal_returns_immediately() {
        let cfg = fast_config(Duration::from_secs(5), 5);
        let result: CtResult<u8> = watch(async || Ok(Poll::Terminal(7u8)), &cfg).await;
        assert_eq!(result.unwrap(), 7);
    }

    // CA-CLI-13: the overall deadline elapses while the run stays pending.
    #[tokio::test]
    async fn ca_cli_13_deadline_elapsed_times_out() {
        let cfg = fast_config(Duration::from_millis(30), 5);
        let result: CtResult<u8> =
            watch(async || Ok::<Poll<u8>, CtError>(Poll::Pending), &cfg).await;
        assert!(matches!(result, Err(CtError::Timeout(_))), "got {result:?}");
    }

    // CA-CLI-15: up to 5 consecutive transport errors are tolerated; the 6th
    // aborts with that transport error.
    #[tokio::test]
    async fn ca_cli_15_sixth_consecutive_transport_error_aborts() {
        let calls = AtomicUsize::new(0);
        let cfg = fast_config(Duration::from_secs(30), 5);
        let result: CtResult<u8> = watch(
            async || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(CtError::Transport("reset".into()))
            },
            &cfg,
        )
        .await;
        assert!(
            matches!(result, Err(CtError::Transport(_))),
            "got {result:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 6, "5 tolerated, 6th aborts");
    }

    #[tokio::test]
    async fn tolerates_five_transport_errors_then_succeeds() {
        let calls = AtomicUsize::new(0);
        let cfg = fast_config(Duration::from_secs(30), 5);
        let result: CtResult<u8> = watch(
            async || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 5 {
                    Err(CtError::Transport("blip".into()))
                } else {
                    Ok(Poll::Terminal(42u8))
                }
            },
            &cfg,
        )
        .await;
        assert_eq!(result.unwrap(), 42);
    }

    // A non-transport error aborts immediately, not counted against the budget.
    #[tokio::test]
    async fn non_transport_error_aborts_immediately() {
        let calls = AtomicUsize::new(0);
        let cfg = fast_config(Duration::from_secs(30), 5);
        let result: CtResult<u8> = watch(
            async || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(CtError::Auth("nope".into()))
            },
            &cfg,
        )
        .await;
        assert!(matches!(result, Err(CtError::Auth(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn jittered_delay_respects_the_configured_maximum() {
        let delay = Duration::from_secs(10);
        let max = Duration::from_secs(10);
        for _ in 0..128 {
            assert!(jittered(delay, 0.2, max) <= max);
        }
    }
}
