use grammers_client::{types::Chat, Client, Config, InitParams};
use grammers_session::Session;
use log::{error, info, warn};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::cache::{AnalysisResult, CacheManager};
use crate::session_manager::SessionManager;
use deadpool_postgres::Pool;

/// rate limiter for telegram api operations
struct TelegramRateLimiter {
    username_resolution_last_call: Arc<Mutex<Option<Instant>>>,
    message_iteration_last_call: Arc<Mutex<Option<Instant>>>,
}

impl TelegramRateLimiter {
    fn new() -> Self {
        Self {
            username_resolution_last_call: Arc::new(Mutex::new(None)),
            message_iteration_last_call: Arc::new(Mutex::new(None)),
        }
    }

    /// wait for username resolution rate limit (1 request per 10 minutes)
    async fn wait_for_username_resolution(&self) {
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
    async fn wait_for_message_iteration(&self) {
        let mut last_call = self.message_iteration_last_call.lock().await;
        *last_call = Some(Instant::now());
    }
}

#[derive(Serialize, Deserialize, Debug, Hash)]
pub struct MessageDict {
    pub date: Option<String>,
    pub message: Option<String>,
}

pub struct AnalysisEngine {
    client: Option<Client>,
    api_id: i32,
    api_hash: String,
    cache: CacheManager,
    resolved_channels: HashMap<String, Chat>,
    rate_limiter: TelegramRateLimiter,
    session_files: Vec<String>,
}

impl AnalysisEngine {
    pub fn new(pool: Pool) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
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

        Ok(Self {
            client: None,
            api_id,
            api_hash,
            cache,
            resolved_channels: HashMap::new(),
            rate_limiter: TelegramRateLimiter::new(),
            session_files,
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
                        .insert(clean_username.to_string(), chat);
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

    pub async fn analyze_channel_with_type(
        &mut self,
        channel_username: &str,
        _analysis_type: &str,
    ) -> Result<AnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
        info!("Starting analysis for channel: {}", channel_username);

        let messages = match self.cache.load_channel_messages(channel_username).await {
            Some(cached_messages) => cached_messages,
            None => {
                info!("Fetching fresh messages from channel");
                self.ensure_client().await?;
                let messages = self.get_all_messages(channel_username).await?;
                self.cache
                    .save_channel_messages(channel_username, &messages)
                    .await?;
                messages
            }
        };

        let cache_key = self.cache.get_llm_cache_key(&messages, "analysis");
        if let Some(cached_result) = self.cache.load_llm_result(&cache_key).await {
            return Ok(cached_result);
        }

        let prompt = Self::generate_analysis_prompt(&messages)?;

        info!("Querying LLM for analysis...");
        let mut result = Self::query_and_parse_analysis(&prompt).await?;
        result.messages_count = messages.len();

        // cache the full analysis result
        if let Err(e) = self.cache.save_llm_result(&cache_key, &result).await {
            info!("Failed to cache LLM result: {}", e);
        }

        Ok(result)
    }

    async fn get_all_messages(
        &mut self,
        channel_username: &str,
    ) -> Result<Vec<MessageDict>, Box<dyn std::error::Error + Send + Sync>> {
        info!("Getting messages from {}", channel_username);

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
                                .insert(clean_username.to_string(), ch.clone());
                        }
                        break channel;
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
                let mut message_iter = client.iter_messages(&chat);
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

    fn generate_analysis_prompt(
        messages: &[MessageDict],
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let messages_json = serde_json::to_string_pretty(messages)?;

        Ok(format!(
            "Given the following messages from a Telegram channel, analyze the author profile and write three blocks of text in the following format:

                    <professional>This part should contain a professional analysis of the author, including their expertise, background, and any relevant professional details, as an advice to the hiring manager</professional>
                    <personal>This part should contain a personal analysis of the author, as written by a professional psychologist for non-professional reader</personal>
                    <roast>This part should contain a roast of the author, as written by a friend of theirs, that is somewhat harsh</roast>
                    Each part should be around 2048 characters long, in the language of the messages.
Messages:
{}",
            messages_json
        ))
    }

    async fn query_and_parse_analysis(
        prompt: &str,
    ) -> Result<AnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
        let llm_response = query_llm(prompt, "gemini-2.5-pro").await?;

        let professional = extract_tag(&llm_response.content, "professional");
        let personal = extract_tag(&llm_response.content, "personal");
        let roast = extract_tag(&llm_response.content, "roast");

        Ok(AnalysisResult {
            professional,
            personal,
            roast,
            messages_count: 0, // will be set by caller
        })
    }
}

#[derive(Debug)]
struct LLMResponse {
    content: String,
}

fn extract_tag(text: &str, tag: &str) -> Option<String> {
    let pattern = format!(r"(?s)<{}>(.*?)</{}>", tag, tag);
    let re = Regex::new(&pattern).ok()?;
    re.captures(text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().trim().to_string())
}

const MAX_RETRIES: u32 = 3;
const BASE_DELAY_MS: u64 = 1000;

async fn query_llm(
    prompt: &str,
    model: &str,
) -> Result<LLMResponse, Box<dyn std::error::Error + Send + Sync>> {
    info!("Querying LLM with model: {}", model);

    for attempt in 0..=MAX_RETRIES {
        let response = match gemini_rs::chat(model).send_message(prompt).await {
            Ok(resp) => resp,
            Err(e) => {
                if attempt == MAX_RETRIES {
                    error!(
                        "Failed to get response from Gemini API after {} attempts: {:?}",
                        MAX_RETRIES + 1,
                        e
                    );
                    return Err(e.into());
                }

                let delay = calculate_delay(attempt);
                warn!(
                    "Gemini API call failed (attempt {}/{}): {:?}. Retrying in {}ms",
                    attempt + 1,
                    MAX_RETRIES + 1,
                    e,
                    delay.as_millis()
                );
                sleep(delay).await;
                continue;
            }
        };

        let content = response.to_string();

        if content.is_empty() {
            if attempt == MAX_RETRIES {
                error!(
                    "Received empty response from Gemini API after {} attempts",
                    MAX_RETRIES + 1
                );
                return Err("Empty response from Gemini API".into());
            }

            let delay = calculate_delay(attempt);
            warn!(
                "Received empty response from Gemini API (attempt {}/{}). Retrying in {}ms",
                attempt + 1,
                MAX_RETRIES + 1,
                delay.as_millis()
            );
            sleep(delay).await;
            continue;
        }

        info!(
            "Received response of length: {} (attempt {})",
            content.len(),
            attempt + 1
        );
        return Ok(LLMResponse { content });
    }

    unreachable!()
}

fn calculate_delay(attempt: u32) -> Duration {
    let base_delay = BASE_DELAY_MS * (1 << attempt); // exponential backoff: 1s, 2s, 4s
    let jitter = fastrand::u64(0..=base_delay / 4); // add up to 25% jitter
    Duration::from_millis(base_delay + jitter)
}
