use std::future::Future;
use std::time::Duration;

use chrono::Utc;
use cloudthinker_api::Error as ApiError;

use crate::error::{CtError, CtResult, to_ct_error};

const BASE_DELAY: Duration = Duration::from_millis(500);
pub(crate) const MAX_RETRY_DELAY: Duration = Duration::from_secs(4);

pub(crate) fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some(
        (at.with_timezone(&Utc) - Utc::now())
            .to_std()
            .unwrap_or(Duration::ZERO),
    )
}

pub(crate) fn backoff(attempt: u32, retry_after: Option<Duration>) -> Duration {
    retry_after
        .unwrap_or_else(|| BASE_DELAY.saturating_mul(1 << attempt.min(4)))
        .min(MAX_RETRY_DELAY)
}

pub(crate) async fn classify(error: ApiError<()>) -> (CtError, Option<Duration>) {
    let retry_after = match &error {
        ApiError::UnexpectedResponse(response) => retry_after(response.headers()),
        ApiError::ErrorResponse(response) => retry_after(response.headers()),
        _ => None,
    };
    (to_ct_error(error).await, retry_after)
}

pub(crate) async fn with_retries<T, F, Fut>(attempts: u32, mut call: F) -> CtResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ApiError<()>>>,
{
    let mut attempt = 0;
    loop {
        let error = match call().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let (error, retry_after) = classify(error).await;
        attempt += 1;
        if !error.is_retryable() || attempt >= attempts {
            return Err(error);
        }
        tokio::time::sleep(backoff(attempt, retry_after)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, value.parse().unwrap());
        headers
    }

    #[test]
    fn retry_after_reads_seconds_and_http_dates() {
        assert_eq!(retry_after(&headers("3")), Some(Duration::from_secs(3)));
        assert_eq!(
            retry_after(&headers("Wed, 21 Oct 2015 07:28:00 GMT")),
            Some(Duration::ZERO)
        );
        assert_eq!(retry_after(&headers("soon")), None);
        assert_eq!(retry_after(&reqwest::header::HeaderMap::new()), None);
    }

    #[test]
    fn backoff_honours_retry_after_within_the_cap() {
        assert_eq!(backoff(1, None), Duration::from_secs(1));
        assert_eq!(backoff(2, None), Duration::from_secs(2));
        assert_eq!(backoff(9, None), MAX_RETRY_DELAY);
        assert_eq!(
            backoff(1, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        assert_eq!(backoff(1, Some(Duration::from_secs(600))), MAX_RETRY_DELAY);
    }

    #[test]
    fn only_temporary_statuses_are_retryable() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(
                CtError::Api {
                    status,
                    detail: None
                }
                .is_retryable(),
                "{status}"
            );
        }
        for status in [400, 401, 403, 404, 409, 422] {
            assert!(
                !CtError::Api {
                    status,
                    detail: None
                }
                .is_retryable(),
                "{status}"
            );
        }
        assert!(CtError::Transport("reset".into()).is_retryable());
        assert!(!CtError::Usage("bad".into()).is_retryable());
    }
}
