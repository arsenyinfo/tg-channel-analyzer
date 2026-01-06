use log::info;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendType {
    Api,
    WebScraping,
}

impl BackendType {
    pub fn name(&self) -> &'static str {
        match self {
            BackendType::Api => "API",
            BackendType::WebScraping => "WebScraping",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    pub enabled_backends: Vec<BackendType>,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            enabled_backends: vec![BackendType::WebScraping, BackendType::Api],
        }
    }
}

#[derive(Debug)]
pub struct BackendRateLimiter {
    api_last_call: Option<Instant>,
    web_scraping_last_call: Option<Instant>,
    api_rate_limit: Duration,
    web_scraping_rate_limit: Duration,
}

impl BackendRateLimiter {
    pub fn new() -> Self {
        Self {
            api_last_call: None,
            web_scraping_last_call: None,
            api_rate_limit: Duration::from_secs(600), // 10 minutes for API operations
            web_scraping_rate_limit: Duration::from_secs(20), // 20 sec for web scraping
        }
    }

    pub fn time_until_available(&self, backend: BackendType) -> Option<Duration> {
        let (last_call, rate_limit) = match backend {
            BackendType::Api => (self.api_last_call, self.api_rate_limit),
            BackendType::WebScraping => (self.web_scraping_last_call, self.web_scraping_rate_limit),
        };

        if let Some(last_time) = last_call {
            let elapsed = last_time.elapsed();
            if elapsed < rate_limit {
                Some(rate_limit - elapsed)
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn is_available(&self, backend: BackendType) -> bool {
        self.time_until_available(backend).is_none()
    }

    pub fn select_available_backend(&self, backends: &[BackendType]) -> Option<BackendType> {
        for &backend in backends {
            if self.is_available(backend) {
                return Some(backend);
            }
        }
        // if none available, return first backend (caller will handle rate limiting)
        backends.first().copied()
    }

    pub async fn wait_for_backend(&mut self, backend: BackendType) {
        if let Some(wait_time) = self.time_until_available(backend) {
            // add jitter to avoid thundering herd
            let jitter =
                Duration::from_millis(fastrand::u64(0..=wait_time.as_millis() as u64 / 10));
            let total_wait = wait_time + jitter;

            info!(
                "Rate limiting {}: waiting {}ms (with {}ms jitter)",
                backend.name(),
                total_wait.as_millis(),
                jitter.as_millis()
            );
            tokio::time::sleep(total_wait).await;
        }
    }

    pub fn record_backend_call(&mut self, backend: BackendType) {
        // record the call time after the actual request
        match backend {
            BackendType::Api => self.api_last_call = Some(Instant::now()),
            BackendType::WebScraping => self.web_scraping_last_call = Some(Instant::now()),
        }
    }
}
