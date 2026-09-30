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
    let mut policy = WatchPolicy::new(cfg);
    loop {
        policy.before_fetch()?;
        match fetch().await {
            Ok(Poll::Terminal(value)) => return Ok(value),
            Ok(Poll::Pending) => policy.pending(),
            Err(err) => policy.failed(err)?,
        }
        tokio::time::sleep(policy.next_nap()?).await;
    }
}

struct WatchPolicy<'a> {
    cfg: &'a WatchConfig,
    start: Instant,
    delay: Duration,
    consecutive_errors: u32,
}

impl<'a> WatchPolicy<'a> {
    fn new(cfg: &'a WatchConfig) -> Self {
        Self {
            cfg,
            start: Instant::now(),
            delay: cfg.base_delay,
            consecutive_errors: 0,
        }
    }

    fn before_fetch(&self) -> CtResult<()> {
        if self.start.elapsed() >= self.cfg.overall_timeout {
            return Err(deadline_elapsed());
        }
        Ok(())
    }

    fn pending(&mut self) {
        self.consecutive_errors = 0;
    }

    fn failed(&mut self, err: CtError) -> CtResult<()> {
        if !err.is_transport() {
            return Err(err);
        }
        self.consecutive_errors += 1;
        if self.consecutive_errors > self.cfg.max_transport_errors {
            return Err(err);
        }
        Ok(())
    }

    fn next_nap(&mut self) -> CtResult<Duration> {
        let remaining = self
            .cfg
            .overall_timeout
            .saturating_sub(self.start.elapsed());
        if remaining.is_zero() {
            return Err(deadline_elapsed());
        }
        let nap = jittered(self.delay, self.cfg.jitter, self.cfg.max_delay).min(remaining);
        self.delay = next_delay(self.delay, self.cfg.factor, self.cfg.max_delay);
        Ok(nap)
    }
}

fn deadline_elapsed() -> CtError {
    CtError::Timeout("run did not finish before the deadline".into())
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
    fn policy_refuses_to_fetch_once_the_deadline_is_zero() {
        let cfg = fast_config(Duration::ZERO, 5);
        let mut policy = WatchPolicy::new(&cfg);
        assert!(matches!(policy.before_fetch(), Err(CtError::Timeout(_))));
        assert!(matches!(policy.next_nap(), Err(CtError::Timeout(_))));
    }

    #[test]
    fn policy_with_no_error_budget_aborts_on_the_first_transport_error() {
        let cfg = fast_config(Duration::from_secs(30), 0);
        let mut policy = WatchPolicy::new(&cfg);
        assert!(matches!(
            policy.failed(CtError::Transport("reset".into())),
            Err(CtError::Transport(_))
        ));
    }

    #[test]
    fn policy_pending_resets_the_consecutive_transport_error_count() {
        let cfg = fast_config(Duration::from_secs(30), 2);
        let mut policy = WatchPolicy::new(&cfg);
        for _ in 0..3 {
            assert!(policy.failed(CtError::Transport("blip".into())).is_ok());
            assert!(policy.failed(CtError::Transport("blip".into())).is_ok());
            policy.pending();
        }
        assert!(policy.failed(CtError::Transport("blip".into())).is_ok());
        assert!(policy.failed(CtError::Transport("blip".into())).is_ok());
        assert!(policy.failed(CtError::Transport("blip".into())).is_err());
    }

    #[test]
    fn policy_backoff_grows_by_the_factor_and_stops_at_the_cap() {
        let cfg = WatchConfig {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(200),
            factor: 1.5,
            jitter: 0.0,
            max_transport_errors: 5,
            overall_timeout: Duration::from_secs(3600),
        };
        let mut policy = WatchPolicy::new(&cfg);
        let naps: Vec<_> = (0..4).map(|_| policy.next_nap().unwrap()).collect();
        assert_eq!(
            naps,
            [100, 150, 200, 200].map(Duration::from_millis).to_vec()
        );
    }

    #[test]
    fn policy_nap_never_outlives_the_remaining_deadline() {
        let cfg = WatchConfig {
            base_delay: Duration::from_secs(60),
            max_delay: Duration::from_secs(60),
            factor: 1.0,
            jitter: 0.0,
            max_transport_errors: 5,
            overall_timeout: Duration::from_secs(1),
        };
        let mut policy = WatchPolicy::new(&cfg);
        assert!(policy.next_nap().unwrap() <= Duration::from_secs(1));
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
