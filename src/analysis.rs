use grammers_client::{types::Chat, Client, Config, InitParams};
use grammers_session::Session;
use log::{error, info, warn};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::time::sleep;

use crate::backend_config::{BackendConfig, BackendRateLimiter, BackendType};
use crate::cache::{AnalysisResult, CacheManager};
use crate::llm::{calculate_delay, MAX_RETRIES};
use crate::rate_limiters::telegram::TelegramRateLimiter;
use crate::session_manager::SessionManager;
use crate::web_scraper::TelegramWebScraper;
use deadpool_postgres::Pool;

#[derive(Serialize, Deserialize, Debug, Hash)]
pub struct MessageDict {
    pub date: Option<String>,
    pub message: Option<String>,
    pub images: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct AnalysisData {
    pub messages: Vec<MessageDict>,
    pub cache_key: String,
}

pub struct AnalysisEngine {
    client: Option<Client>,
    api_id: i32,
    api_hash: String,
    pub cache: CacheManager,
    resolved_channels: HashMap<String, Arc<Chat>>,
    rate_limiter: TelegramRateLimiter,
    session_files: Vec<String>,
    web_scraper: TelegramWebScraper,
    backend_config: BackendConfig,
    backend_rate_limiter: BackendRateLimiter,
}

impl AnalysisEngine {
    pub fn new(pool: Arc<Pool>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let api_id = env::var("TG_API_ID")
            .map_err(|_| "TG_API_ID environment variable is required")?
            .parse::<i32>()
            .map_err(|_| "TG_API_ID must be a valid integer")?;

        let api_hash =
            env::var("TG_API_HASH").map_err(|_| "TG_API_HASH environment variable is required")?;

        let cache = CacheManager::new(pool);

        let session_files = SessionManager::discover_sessions()?;
        if session_files.is_empty() {
            return Err("No session files found in sessions/ directory".into());
        }
        info!("Found {} session files", session_files.len());

        let web_scraper = TelegramWebScraper::new()
            .map_err(|e| format!("Failed to initialize web scraper: {}", e))?;

        Ok(Self {
            client: None,
            api_id,
            api_hash,
            cache,
            resolved_channels: HashMap::new(),
            rate_limiter: TelegramRateLimiter::new(),
            session_files,
            web_scraper,
            backend_config: BackendConfig::default(),
            backend_rate_limiter: BackendRateLimiter::new(),
        })
    }

    fn get_random_session(&self) -> &String {
        let mut rng = rand::thread_rng();
        let index = rng.gen_range(0..self.session_files.len());
        &self.session_files[index]
    }

    async fn ensure_client(&mut self) -> Result<&Client, Box<dyn std::error::Error + Send + Sync>> {
        if self.client.is_none() {
            info!("Initializing Telegram client...");

            for attempt in 0..=MAX_RETRIES {
                let session_file = self.get_random_session();
                let session = match Session::load_file(session_file) {
                    Ok(session) => {
                        info!("Loaded existing session: {}", session_file);
                        session
                    }
                    Err(_) => {
                        info!("Failed to load session {}, creating new one", session_file);
                        Session::new()
                    }
                };

                let config = Config {
                    session,
                    api_id: self.api_id,
                    api_hash: self.api_hash.clone(),
                    params: InitParams {
                        ..Default::default()
                    },
                };

                let client = match Client::connect(config).await {
                    Ok(client) => client,
                    Err(e) => {
                        if attempt == MAX_RETRIES {
                            error!(
                                "Failed to connect Telegram client after {} attempts: {}",
                                MAX_RETRIES + 1,
                                e
                            );
                            return Err(e.into());
                        }

                        let delay = calculate_delay(attempt);
                        warn!(
                            "Failed to connect Telegram client (attempt {}/{}): {}. Retrying in {}ms",
                            attempt + 1,
                            MAX_RETRIES + 1,
                            e,
                            delay.as_millis()
                        );
                        sleep(delay).await;
                        continue;
                    }
                };

                match client.is_authorized().await {
                    Ok(true) => {
                        info!(
                            "Client connected and authorized successfully (attempt {})",
                            attempt + 1
                        );
                        self.client = Some(client);
                        break;
                    }
                    Ok(false) => {
                        return Err("Client is not authorized. Please run the standalone analyzer first to authorize.".into());
                    }
                    Err(e) => {
                        if attempt == MAX_RETRIES {
                            error!(
                                "Failed to check client authorization after {} attempts: {}",
                                MAX_RETRIES + 1,
                                e
                            );
                            return Err(e.into());
                        }

                        let delay = calculate_delay(attempt);
                        warn!(
                            "Failed to check client authorization (attempt {}/{}): {}. Retrying in {}ms",
                            attempt + 1,
                            MAX_RETRIES + 1,
                            e,
                            delay.as_millis()
                        );
                        sleep(delay).await;
                    }
                }
            }
        }

        Ok(self.client.as_ref().unwrap())
    }

    pub async fn validate_channel(
        &mut self,
        channel_username: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let clean_username = if channel_username.starts_with('@') {
            &channel_username[1..]
        } else {
            channel_username
        };

        info!("Validating channel: {}", clean_username);

        for attempt in 0..=MAX_RETRIES {
            // rate limit username resolution on every attempt
            self.rate_limiter.wait_for_username_resolution().await;

            let client = match self.ensure_client().await {
                Ok(client) => client,
                Err(e) => {
                    if attempt == MAX_RETRIES {
                        error!(
                            "Failed to get client for channel validation after {} attempts: {}",
                            MAX_RETRIES + 1,
                            e
                        );
                        return Err(e);
                    }

                    let delay = calculate_delay(attempt);
                    warn!(
                        "Failed to get client for channel validation (attempt {}/{}): {}. Retrying in {}ms",
                        attempt + 1,
                        MAX_RETRIES + 1,
                        e,
                        delay.as_millis()
                    );
                    sleep(delay).await;
                    continue;
                }
            };

            match client.resolve_username(clean_username).await {
                Ok(Some(chat)) => {
                    info!(
                        "Channel {} is valid and accessible (attempt {})",
                        clean_username,
                        attempt + 1
                    );
                    // cache the resolved channel
                    self.resolved_channels
                        .insert(clean_username.to_string(), Arc::new(chat));
                    return Ok(true);
                }
                Ok(None) => {
                    info!("Channel {} not found", clean_username);
                    return Ok(false);
                }
                Err(e) => {
                    if attempt == MAX_RETRIES {
                        error!(
                            "Error validating channel {} after {} attempts: {}",
                            clean_username,
                            MAX_RETRIES + 1,
                            e
                        );
                        return Err(e.into());
                    }

                    let delay = calculate_delay(attempt);
                    warn!(
                        "Channel validation failed for {} (attempt {}/{}): {}. Retrying in {}ms",
                        clean_username,
                        attempt + 1,
                        MAX_RETRIES + 1,
                        e,
                        delay.as_millis()
                    );
                    sleep(delay).await;
                    // reset client and clear channel cache on connection errors
                    self.client = None;
                    self.resolved_channels.remove(clean_username);
                }
            }
        }

        unreachable!()
    }


    pub async fn prepare_analysis_data(
        &mut self,
        channel_username: &str,
    ) -> Result<AnalysisData, Box<dyn std::error::Error + Send + Sync>> {
        info!("Starting analysis for channel: {}", channel_username);

        let messages = match self.cache.load_channel_messages(channel_username).await {
            Some(cached_messages) => {
                info!(
                    "Using cached messages for channel: {} ({} messages)",
                    channel_username,
                    cached_messages.len()
                );
                cached_messages
            }
            None => {
                info!("Fetching fresh messages from channel: {}", channel_username);
                self.ensure_client().await.map_err(|e| {
                    error!(
                        "Failed to ensure client for channel {}: {}",
                        channel_username, e
                    );
                    e
                })?;
                let (messages, _hit_rate_limits) = self
                    .get_all_messages_with_rate_limit_info(channel_username)
                    .await
                    .map_err(|e| {
                        error!(
                            "Failed to fetch messages from channel {}: {}",
                            channel_username, e
                        );
                        e
                    })?;
                info!(
                    "Fetched {} messages from channel: {}",
                    messages.len(),
                    channel_username
                );
                if let Err(e) = self
                    .cache
                    .save_channel_messages(channel_username, &messages)
                    .await
                {
                    error!(
                        "Failed to cache messages for channel {}: {}",
                        channel_username, e
                    );
                    // Continue execution - caching failure shouldn't stop the analysis
                }
                messages
            }
        };

        let cache_key = self.cache.get_llm_cache_key(&messages, "analysis");
        Ok(AnalysisData {
            messages,
            cache_key,
        })
    }

    pub async fn finish_analysis(
        &mut self,
        cache_key: &str,
        result: AnalysisResult,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // cache the full analysis result
        if let Err(e) = self.cache.save_llm_result(cache_key, &result).await {
            info!("Failed to cache LLM result: {}", e);
        }
        Ok(())
    }

    async fn get_all_messages_with_rate_limit_info(
        &mut self,
        channel_username: &str,
    ) -> Result<(Vec<MessageDict>, bool), Box<dyn std::error::Error + Send + Sync>> {
        info!("Getting messages from {}", channel_username);

        // select backend based on rate limits (web scraping preferred)
        let backend = self
            .backend_rate_limiter
            .select_available_backend(&self.backend_config.enabled_backends)
            .unwrap_or(BackendType::WebScraping);

        // check if both backends are rate limited
        let web_time = self
            .backend_rate_limiter
            .time_until_available(BackendType::WebScraping);
        let api_time = self
            .backend_rate_limiter
            .time_until_available(BackendType::Api);
        let hit_rate_limits = web_time.is_some() && api_time.is_some();

        // if chosen backend is not available, wait for the closest one
        if !self.backend_rate_limiter.is_available(backend) {
            let closest_backend = match (web_time, api_time) {
                (None, _) => BackendType::WebScraping,
                (_, None) => BackendType::Api,
                (Some(web), Some(api)) => {
                    if web <= api {
                        BackendType::WebScraping
                    } else {
                        BackendType::Api
                    }
                }
            };

            if let Some(wait_time) = self
                .backend_rate_limiter
                .time_until_available(closest_backend)
            {
                info!(
                    "Waiting {}s for {} backend",
                    wait_time.as_secs(),
                    closest_backend.name()
                );
                self.backend_rate_limiter
                    .wait_for_backend(closest_backend)
                    .await;
            }
        }

        let messages = match backend {
            BackendType::WebScraping => {
                info!("Using web scraping backend for {}", channel_username);
                let channel_url =
                    format!("https://t.me/{}", channel_username.trim_start_matches('@'));
                let messages = self
                    .web_scraper
                    .scrape_channel_messages(&channel_url, 10)
                    .await
                    .map_err(|e| {
                        error!(
                            "Web scraping failed for channel {}: {}",
                            channel_username, e
                        );
                        Box::new(e) as Box<dyn std::error::Error + Send + Sync>
                    })?;
                self.backend_rate_limiter
                    .record_backend_call(BackendType::WebScraping);
                messages
            }
            BackendType::Api => {
                info!("Using API backend for {}", channel_username);

                // validate channel when using API backend
                match self.validate_channel(channel_username).await {
                    Ok(true) => {}
                    Ok(false) => {
                        error!(
                            "Channel validation failed for {}: channel not found or not accessible",
                            channel_username
                        );
                        return Err("Channel not found or not accessible".into());
                    }
                    Err(e) => {
                        error!("Channel validation error for {}: {}", channel_username, e);
                        return Err(e);
                    }
                }

                self.ensure_client().await.map_err(|e| {
                    error!("Failed to ensure client for API backend: {}", e);
                    e
                })?;
                let messages = self
                    .get_all_messages_api(channel_username)
                    .await
                    .map_err(|e| {
                        error!(
                            "Failed to get messages via API for channel {}: {}",
                            channel_username, e
                        );
                        e
                    })?;
                self.backend_rate_limiter
                    .record_backend_call(BackendType::Api);
                messages
            }
        };

        Ok((messages, hit_rate_limits))
    }

    async fn get_all_messages_api(
        &mut self,
        channel_username: &str,
    ) -> Result<Vec<MessageDict>, Box<dyn std::error::Error + Send + Sync>> {
        let clean_username = if channel_username.starts_with('@') {
            &channel_username[1..]
        } else {
            channel_username
        };

        // check for cached channel first, fallback to resolution if needed
        let channel = if let Some(cached_channel) = self.resolved_channels.get(clean_username) {
            info!("Using cached channel for {}", clean_username);
            Some(cached_channel.clone())
        } else {
            info!("No cached channel found, resolving {}", clean_username);
            // get client reference
            let client = self.client.as_ref().ok_or("Client not initialized")?;
            // retry channel resolution
            let mut attempt = 0;
            loop {
                self.rate_limiter.wait_for_username_resolution().await;
                match client.resolve_username(clean_username).await {
                    Ok(channel) => {
                        if let Some(ref ch) = channel {
                            // cache the newly resolved channel
                            self.resolved_channels
                                .insert(clean_username.to_string(), Arc::new(ch.clone()));
                        }
                        break channel.map(Arc::new);
                    }
                    Err(e) => {
                        if attempt == MAX_RETRIES {
                            error!(
                                "Failed to resolve channel {} after {} attempts: {}",
                                clean_username,
                                MAX_RETRIES + 1,
                                e
                            );
                            return Err(e.into());
                        }

                        let delay = calculate_delay(attempt);
                        warn!(
                            "Failed to resolve channel {} for message fetching (attempt {}/{}): {}. Retrying in {}ms",
                            clean_username,
                            attempt + 1,
                            MAX_RETRIES + 1,
                            e,
                            delay.as_millis()
                        );
                        sleep(delay).await;
                        attempt += 1;
                    }
                }
            }
        };

        let mut messages = Vec::new();
        let mut skipped = 0;

        if let Some(chat) = channel {
            let client = self.client.as_ref().ok_or("Client not initialized")?;
            for attempt in 0..=MAX_RETRIES {
                self.rate_limiter.wait_for_message_iteration().await;
                let mut message_iter = client.iter_messages(chat.as_ref());
                let mut current_messages = Vec::new();
                let mut current_skipped = 0;

                match async {
                    while let Some(message) = message_iter.next().await? {
                        if message.forward_header().is_some() {
                            current_skipped += 1;
                            continue;
                        }
                        if message.text().len() < 32 {
                            current_skipped += 1;
                            continue;
                        }

                        current_messages.push(MessageDict {
                            date: Some(message.date().to_rfc2822()),
                            message: Some(message.text().to_string()),
                            images: None, // Telegram API messages don't include images in this context
                        });

                        if current_messages.len() >= 200 {
                            break;
                        }
                    }
                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                }
                .await
                {
                    Ok(_) => {
                        messages = current_messages;
                        skipped = current_skipped;
                        info!(
                            "Retrieved {} messages, skipped {} (attempt {})",
                            messages.len(),
                            skipped,
                            attempt + 1
                        );
                        break;
                    }
                    Err(e) => {
                        if attempt == MAX_RETRIES {
                            error!(
                                "Failed to fetch messages from {} after {} attempts: {}",
                                clean_username,
                                MAX_RETRIES + 1,
                                e
                            );
                            return Err(e);
                        }

                        let delay = calculate_delay(attempt);
                        warn!(
                            "Failed to fetch messages from {} (attempt {}/{}): {}. Retrying in {}ms",
                            clean_username,
                            attempt + 1,
                            MAX_RETRIES + 1,
                            e,
                            delay.as_millis()
                        );
                        sleep(delay).await;
                        // clear channel cache on message fetching errors
                        self.resolved_channels.remove(clean_username);
                    }
                }
            }
        }

        info!("Retrieved {} messages, skipped {}", messages.len(), skipped);
        Ok(messages)
    }

}

