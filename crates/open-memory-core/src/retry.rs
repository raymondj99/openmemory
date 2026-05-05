use std::time::Duration;

pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl RetryConfig {
    pub fn io() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
        }
    }

    pub fn network() -> Self {
        Self {
            max_retries: 5,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(10),
        }
    }
}

pub fn with_retry<T, E, F, R>(
    config: &RetryConfig,
    is_retryable: R,
    mut operation: F,
) -> Result<T, E>
where
    F: FnMut() -> Result<T, E>,
    R: Fn(&E) -> bool,
    E: std::fmt::Display,
{
    let mut last_err;

    match operation() {
        Ok(val) => return Ok(val),
        Err(e) => {
            if !is_retryable(&e) || config.max_retries == 0 {
                return Err(e);
            }
            last_err = e;
        }
    }

    for attempt in 0..config.max_retries {
        let delay = calculate_delay(config, attempt);
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(delay);

        match operation() {
            Ok(val) => return Ok(val),
            Err(e) => {
                if !is_retryable(&e) {
                    return Err(e);
                }
                last_err = e;
            }
        }
    }

    Err(last_err)
}

fn calculate_delay(config: &RetryConfig, attempt: u32) -> Duration {
    let base_ms = config.base_delay.as_millis() as u64;
    let max_ms = config.max_delay.as_millis() as u64;

    let exp_ms = base_ms.saturating_mul(1u64 << attempt.min(31)).min(max_ms);

    let jitter_min = exp_ms / 2;
    let jitter_range = exp_ms - jitter_min;
    let jitter = if jitter_range > 0 {
        jitter_min + rand::random::<u64>() % (jitter_range + 1)
    } else {
        jitter_min
    };

    Duration::from_millis(jitter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn succeeds_immediately() {
        let config = RetryConfig::io();
        let result: Result<i32, String> = with_retry(&config, |_| true, || Ok(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn retries_then_succeeds() {
        let config = RetryConfig {
            max_retries: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        };
        let attempts = Cell::new(0);
        let result: Result<i32, String> = with_retry(
            &config,
            |_| true,
            || {
                let n = attempts.get();
                attempts.set(n + 1);
                if n < 2 {
                    Err("transient".to_string())
                } else {
                    Ok(42)
                }
            },
        );
        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.get(), 3);
    }

    #[test]
    fn stops_on_non_retryable() {
        let config = RetryConfig {
            max_retries: 5,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        };
        let attempts = Cell::new(0);
        let result: Result<i32, String> = with_retry(
            &config,
            |e: &String| e != "permanent",
            || {
                let n = attempts.get();
                attempts.set(n + 1);
                if n == 0 {
                    Err("transient".to_string())
                } else {
                    Err("permanent".to_string())
                }
            },
        );
        assert_eq!(result.unwrap_err(), "permanent");
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn exhausts_retries() {
        let config = RetryConfig {
            max_retries: 2,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        };
        let attempts = Cell::new(0);
        let result: Result<i32, String> = with_retry(
            &config,
            |_| true,
            || {
                attempts.set(attempts.get() + 1);
                Err("fail".to_string())
            },
        );
        assert_eq!(result.unwrap_err(), "fail");
        assert_eq!(attempts.get(), 3);
    }

    #[test]
    fn zero_retries_returns_immediately() {
        let config = RetryConfig {
            max_retries: 0,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
        };
        let attempts = Cell::new(0);
        let result: Result<i32, String> = with_retry(
            &config,
            |_| true,
            || {
                attempts.set(attempts.get() + 1);
                Err("fail".to_string())
            },
        );
        assert_eq!(result.unwrap_err(), "fail");
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn delay_respects_max() {
        let config = RetryConfig {
            max_retries: 10,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
        };
        for attempt in 0..10 {
            let delay = calculate_delay(&config, attempt);
            assert!(delay.as_millis() <= 500);
        }
    }
}
