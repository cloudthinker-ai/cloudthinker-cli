//! Device-code login pacing and deadline handling.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

use crate::StoredToken;
use crate::client::{CtClient, DeviceAuthorization, DeviceTokenPoll};
use crate::error::{CtError, CtResult};

const TRANSPORT_BACKOFF: Duration = Duration::from_secs(5);
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Wait for a browser decision without ever polling faster than the server asks.
pub async fn wait_for_device_token(
    client: &CtClient,
    authorization: &DeviceAuthorization,
) -> CtResult<StoredToken> {
    wait_for_device_token_with(authorization, || {
        client.poll_device_token(&authorization.device_code)
    })
    .await
}

async fn wait_for_device_token_with<F, Fut>(
    authorization: &DeviceAuthorization,
    mut poll: F,
) -> CtResult<StoredToken>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = CtResult<DeviceTokenPoll>>,
{
    let deadline = Instant::now() + authorization.expires_in;
    let mut interval = authorization.interval;

    loop {
        if tokio::time::timeout_at(deadline, tokio::time::sleep(interval))
            .await
            .is_err()
        {
            return Err(expired());
        }

        let poll_result = tokio::time::timeout_at(deadline, poll())
            .await
            .map_err(|_| expired())?;

        match poll_result {
            Ok(DeviceTokenPoll::Token(token)) => return Ok(token),
            Ok(DeviceTokenPoll::Pending(server_interval))
            | Ok(DeviceTokenPoll::SlowDown(server_interval)) => {
                if !server_interval.is_zero() {
                    interval = server_interval;
                }
            }
            Err(error) if error.is_transport() => {
                interval = interval
                    .saturating_add(TRANSPORT_BACKOFF)
                    .min(MAX_POLL_INTERVAL);
            }
            Err(error) => return Err(error),
        }

        if Instant::now() >= deadline {
            return Err(expired());
        }
    }
}

fn expired() -> CtError {
    CtError::Timeout("device code expired; run `cloudthinker login --device-auth` again".into())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    fn authorization(expires_in: u64, interval: u64) -> DeviceAuthorization {
        DeviceAuthorization {
            device_code: "device-code".into(),
            user_code: "BCDF-GHJK".into(),
            verification_uri: "https://app.cloudthinker.io/auth/cli".into(),
            expires_in: Duration::from_secs(expires_in),
            interval: Duration::from_secs(interval),
        }
    }

    fn token() -> StoredToken {
        StoredToken {
            access_token: "access-token".into(),
            refresh_token: Some("refresh-token".into()),
            expires_at: None,
            workspace_id: None,
            workspace_name: None,
        }
    }

    fn record_call(calls: &Mutex<Vec<Instant>>) {
        calls.lock().expect("calls lock").push(Instant::now());
    }

    #[tokio::test(start_paused = true)]
    async fn waits_for_server_interval_and_adopts_slow_down() {
        let authorization = authorization(60, 5);
        let started = Instant::now();
        let attempts = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(Mutex::new(Vec::new()));

        let result = wait_for_device_token_with(&authorization, || {
            let attempts = Arc::clone(&attempts);
            let calls = Arc::clone(&calls);
            async move {
                record_call(&calls);
                match attempts.fetch_add(1, Ordering::SeqCst) {
                    0 => Ok(DeviceTokenPoll::Pending(Duration::from_secs(5))),
                    1 => Ok(DeviceTokenPoll::SlowDown(Duration::from_secs(10))),
                    _ => Ok(DeviceTokenPoll::Token(token())),
                }
            }
        })
        .await
        .expect("poll succeeds");

        assert_eq!(result.access_token, "access-token");
        let calls = calls.lock().expect("calls lock");
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0] - started, Duration::from_secs(5));
        assert_eq!(calls[1] - calls[0], Duration::from_secs(5));
        assert_eq!(calls[2] - calls[1], Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn transport_backoff_is_bounded() {
        let authorization = authorization(200, 5);
        let started = Instant::now();
        let attempts = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(Mutex::new(Vec::new()));

        wait_for_device_token_with(&authorization, || {
            let attempts = Arc::clone(&attempts);
            let calls = Arc::clone(&calls);
            async move {
                record_call(&calls);
                if attempts.fetch_add(1, Ordering::SeqCst) < 6 {
                    Err(CtError::Transport("offline".into()))
                } else {
                    Ok(DeviceTokenPoll::Token(token()))
                }
            }
        })
        .await
        .expect("poll recovers");

        let calls = calls.lock().expect("calls lock");
        let actual = std::iter::once(calls[0] - started)
            .chain(calls.windows(2).map(|pair| pair[1] - pair[0]))
            .collect::<Vec<_>>();
        assert_eq!(actual, [5, 10, 15, 20, 25, 30, 30].map(Duration::from_secs));
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_error_stops_polling() {
        let authorization = authorization(60, 5);
        let attempts = Arc::new(AtomicUsize::new(0));

        let error = wait_for_device_token_with(&authorization, || {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(CtError::LoginDenied)
            }
        })
        .await
        .expect_err("denial is terminal");

        assert!(matches!(error, CtError::LoginDenied));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_stops_polling_before_the_next_request() {
        let authorization = authorization(12, 5);
        let started = Instant::now();
        let attempts = Arc::new(AtomicUsize::new(0));

        let error = wait_for_device_token_with(&authorization, || {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(DeviceTokenPoll::Pending(Duration::from_secs(10)))
            }
        })
        .await
        .expect_err("authorization expires");

        assert!(matches!(error, CtError::Timeout(_)));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(Instant::now() - started, Duration::from_secs(12));
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_cancels_an_in_flight_poll() {
        let authorization = authorization(12, 5);
        let attempts = Arc::new(AtomicUsize::new(0));
        let task_attempts = Arc::clone(&attempts);

        let task = tokio::spawn(async move {
            wait_for_device_token_with(&authorization, || {
                let attempts = Arc::clone(&task_attempts);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    std::future::pending().await
                }
            })
            .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_secs(7)).await;
        tokio::task::yield_now().await;
        if !task.is_finished() {
            task.abort();
            panic!("in-flight poll continued past authorization deadline");
        }

        let error = task
            .await
            .expect("poll task completes")
            .expect_err("authorization expires");

        assert!(matches!(error, CtError::Timeout(_)));
    }
}
