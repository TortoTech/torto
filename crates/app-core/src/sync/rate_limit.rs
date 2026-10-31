use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, RETRY_AFTER};

pub(super) const COOLDOWN_KEY: &str = "rate-limit-until-ms";
pub(super) const PACE_KEY: &str = "rate-limit-pace-v1";

// Recover cautiously, one step per six quiet hours, including across app restarts.
pub(super) fn pace_interval(bytes: &[u8], now: u64) -> u64 {
    let Ok(bytes) = <[u8; 16]>::try_from(bytes) else {
        return 1000;
    };
    let interval = u64::from_le_bytes(bytes[..8].try_into().unwrap()).clamp(1000, 8000);
    let at = u64::from_le_bytes(bytes[8..].try_into().unwrap());
    (interval >> (now.saturating_sub(at) / (6 * 60 * 60 * 1000)).min(3)).max(1000)
}
pub(super) const DEFAULT_COOLDOWN: Duration = Duration::from_secs(5 * 60);

/// Preserve the server's retry deadline across the worker/UI boundary.
#[derive(Debug)]
pub struct RateLimited {
    pub retry_after: Duration,
}

impl fmt::Display for RateLimited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "WebDAV HTTP 429: 请求过于频繁，等待 {} 秒后自动重试",
            self.retry_after.as_secs()
        )
    }
}

impl std::error::Error for RateLimited {}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

pub(super) fn retry_after(headers: &HeaderMap, now: SystemTime) -> Duration {
    let delay = headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let value = value.trim();
            if let Ok(seconds) = value.parse::<u64>() {
                return Some(Duration::from_secs(seconds));
            }
            let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
            let deadline: SystemTime = date.into();
            Some(deadline.duration_since(now).unwrap_or_default())
        })
        .unwrap_or(DEFAULT_COOLDOWN);
    // Bound corrupt server values while retaining normal multi-hour/day quotas.
    delay.clamp(
        Duration::from_secs(1),
        Duration::from_secs(30 * 24 * 60 * 60),
    )
}

pub(super) fn remaining(bytes: &[u8], now: u64) -> Option<Duration> {
    let deadline = u64::from_le_bytes(bytes.try_into().ok()?);
    (deadline > now).then(|| Duration::from_millis(deadline - now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_preserves_seconds_and_http_dates_and_handles_missing_headers() {
        let now = UNIX_EPOCH + Duration::from_secs(784111777);
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after(&headers, now), DEFAULT_COOLDOWN);
        for (value, seconds) in [
            ("7200", 7200),
            ("Sun, 06 Nov 1994 08:51:37 GMT", 120),
            ("Sun, 06 Nov 1994 08:49:37 GMT", 1),
            ("invalid", 300),
            ("18446744073709551615", 30 * 24 * 60 * 60),
        ] {
            headers.insert(RETRY_AFTER, value.parse().unwrap());
            assert_eq!(
                retry_after(&headers, now),
                Duration::from_secs(seconds),
                "{value}"
            );
        }
    }

    #[test]
    fn saved_cooldown_expires_and_rejects_invalid_cache_data() {
        let bytes = 1500_u64.to_le_bytes();
        assert_eq!(remaining(&bytes, 1000), Some(Duration::from_millis(500)));
        assert_eq!(remaining(&bytes, 1500), None);
        assert_eq!(remaining(&bytes, 1600), None);
        assert_eq!(remaining(b"invalid", 1000), None);
    }

    #[test]
    fn pacing_survives_restarts_and_recovers_gradually() {
        let at = 1_000_u64;
        let mut bytes = 8_000_u64.to_le_bytes().to_vec();
        bytes.extend_from_slice(&at.to_le_bytes());
        assert_eq!(pace_interval(&bytes, at), 8000);
        assert_eq!(pace_interval(&bytes, at + 6 * 60 * 60 * 1000), 4000);
        assert_eq!(pace_interval(&bytes, at + 12 * 60 * 60 * 1000), 2000);
        assert_eq!(pace_interval(&bytes, at + 24 * 60 * 60 * 1000), 1000);
        assert_eq!(pace_interval(b"invalid", at), 1000);
    }
}
