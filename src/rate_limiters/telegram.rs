use log::info;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;

/// rate limiter for telegram api operations
pub struct TelegramRateLimiter {
    username_resolution_last_call: Arc<Mutex<Option<Instant>>>,
    message_iteration_last_call: Arc<Mutex<Option<Instant>>>,
}

impl TelegramRateLimiter {
    pub fn new() -> Self {
        Self {
            username_resolution_last_call: Arc::new(Mutex::new(None)),
            message_iteration_last_call: Arc::new(Mutex::new(None)),
        }
    }

    /// wait for username resolution rate limit (1 request per 10 minutes)
    pub async fn wait_for_username_resolution(&self) {
        let mut last_call = self.username_resolution_last_call.lock().await;

        if let Some(last_time) = *last_call {
            let elapsed = last_time.elapsed();
            let min_interval = Duration::from_secs(600);

            if elapsed < min_interval {
                let wait_time = min_interval - elapsed;
                info!(
                    "Rate limiting username resolution: waiting {}ms",
                    wait_time.as_millis()
                );
                sleep(wait_time).await;
            }
        }

        *last_call = Some(Instant::now());
    }

    /// wait for message iteration rate limit (no artificial limit, just tracking)
    pub async fn wait_for_message_iteration(&self) {
        let mut last_call = self.message_iteration_last_call.lock().await;
        *last_call = Some(Instant::now());
    }
}