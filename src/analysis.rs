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
use tokio::time::{sleep, timeout};

use crate::backend_config::{BackendConfig, BackendRateLimiter, BackendType};
use crate::cache::{AnalysisResult, CacheManager};
use crate::session_manager::SessionManager;
use crate::web_scraper::TelegramWebScraper;
use deadpool_postgres::Pool;

/// rate limiter for telegram api operations
struct TelegramRateLimiter {
    username_resolution_last_call: Arc<Mutex<Option<Instant>>>,
    message_iteration_last_call: Arc<Mutex<Option<Instant>>>,
}

/// global rate limiter for gemini api operations
struct GeminiRateLimiter {
    last_call: Arc<Mutex<Option<Instant>>>,
}

impl GeminiRateLimiter {
    fn new() -> Self {
        Self {
            last_call: Arc::new(Mutex::new(None)),
        }
    }

    /// wait for gemini api rate limit (1 request per second)
    async fn wait_for_api_call(&self) {
        let mut last_call = self.last_call.lock().await;

        if let Some(last_time) = *last_call {
            let elapsed = last_time.elapsed();
            let min_interval = Duration::from_secs(1);

            if elapsed < min_interval {
                let wait_time = min_interval - elapsed;
                info!(
                    "Rate limiting Gemini API: waiting {}ms",
                    wait_time.as_millis()
                );
                sleep(wait_time).await;
            }
        }

        *last_call = Some(Instant::now());
    }
}

/// global gemini rate limiter instance
static GEMINI_RATE_LIMITER: std::sync::OnceLock<GeminiRateLimiter> = std::sync::OnceLock::new();

fn get_gemini_rate_limiter() -> &'static GeminiRateLimiter {
    GEMINI_RATE_LIMITER.get_or_init(|| GeminiRateLimiter::new())
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

    pub fn generate_analysis_prompt(
        messages: &[MessageDict],
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let messages_json = serde_json::to_string_pretty(messages)?;

        Ok(format!(
            "You are an expert analyst tasked with creating a comprehensive personality profile based on Telegram channel messages. Analyze the writing style, topics discussed, opinions expressed, and behavioral patterns to understand the author's character.

CRITICAL REQUIREMENTS:
1. Write in the same language as the messages (detect automatically)
2. Each section must be approximately 2048 characters long
3. Use ONLY the provided XML tags exactly as shown
4. Base analysis solely on the message content provided
5. Do not make assumptions about gender, age, or location unless clearly evident

OUTPUT FORMAT (use these exact tags):

<professional>
Write a detailed professional assessment suitable for a hiring manager. Focus on:
- Technical skills and expertise demonstrated
- Communication style and professionalism
- Leadership qualities or lack thereof
- Work ethic and reliability indicators
- Potential red flags or concerns for employers
- Industry knowledge and thought leadership
- Team collaboration potential

Tone: Formal, objective, balanced - highlight both strengths and weaknesses
Length: ~2048 characters
</professional>

<personal>
Write a psychological personality analysis for a general audience. Focus on:
- Core personality traits and characteristics
- Emotional intelligence and social skills
- Decision-making patterns and cognitive style
- Values, beliefs, and motivations
- Relationship patterns and social behavior
- Stress responses and coping mechanisms
- Growth mindset vs fixed mindset indicators

Tone: Insightful, empathetic, professional psychological assessment
Length: ~2048 characters
</personal>

<roast>
Write a sharp, witty critique as if from a close friend who knows them well. Focus on:
- Quirks, habits, and annoying tendencies
- Contradictions in their behavior or beliefs
- Pretentious or hypocritical moments
- Social media behavior and online persona
- Pet peeves others might have about them
- Blind spots and areas of self-delusion

Tone: Brutally honest, sharp humor, keeping in mind the cultural context (e.g. Eastern European directness)
Length: ~2048 characters
Note: Adjust harshness based on cultural context - Eastern Europeans typically appreciate more direct criticism
</roast>

ANALYSIS GUIDELINES:
- Look for patterns across multiple messages, not isolated incidents
- Consider context and nuance, not just surface-level content
- Identify both explicit statements and implied attitudes
- Note communication style: formal vs casual, technical vs accessible
- Observe emotional regulation and reaction patterns
- Consider the audience they're writing for and how they adapt their voice

Messages to analyze:
{}",
            messages_json
        ))
    }

    pub async fn query_and_parse_analysis(
        prompt: &str,
    ) -> Result<AnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
        // helper function to check if analysis result is complete
        fn is_analysis_complete(
            professional: &Option<String>,
            personal: &Option<String>,
            roast: &Option<String>,
        ) -> bool {
            professional.is_some() && personal.is_some() && roast.is_some()
        }

        // helper function to try a model with content retries
        async fn try_model_with_content_retries(
            prompt: &str,
            model: &str,
            api_retries: u32,
            content_retries: u32,
        ) -> Result<AnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
            // retry API calls
            for api_attempt in 0..api_retries {
                match query_llm(prompt, model).await {
                    Ok(response) => {
                        // retry content parsing
                        for content_attempt in 0..content_retries {
                            let professional = extract_tag(&response.content, "professional");
                            let personal = extract_tag(&response.content, "personal");
                            let roast = extract_tag(&response.content, "roast");

                            // log missing sections
                            let mut missing_sections = Vec::new();
                            if professional.is_none() {
                                missing_sections.push("professional");
                            }
                            if personal.is_none() {
                                missing_sections.push("personal");
                            }
                            if roast.is_none() {
                                missing_sections.push("roast");
                            }

                            if !missing_sections.is_empty() {
                                warn!(
                                    "Missing analysis sections [{}] from {} (api_attempt: {}, content_attempt: {})",
                                    missing_sections.join(", "),
                                    model,
                                    api_attempt + 1,
                                    content_attempt + 1
                                );
                            }

                            // if all sections are present, return immediately
                            if is_analysis_complete(&professional, &personal, &roast) {
                                info!("Complete analysis received from {} (api_attempt: {}, content_attempt: {})",
                                      model, api_attempt + 1, content_attempt + 1);
                                return Ok(AnalysisResult {
                                    professional,
                                    personal,
                                    roast,
                                    messages_count: 0,
                                });
                            }

                            // if incomplete and not the last content attempt, retry with same response
                            if content_attempt < content_retries - 1 {
                                warn!(
                                    "Retrying content parsing for {} (content_attempt: {})",
                                    model,
                                    content_attempt + 1
                                );
                                // in this case, we're re-parsing the same response, so we just continue the loop
                                // but in practice, extract_tag is deterministic, so this won't help
                                // this structure is here for future improvements like fuzzy parsing
                            } else {
                                // last content attempt failed, need new API call if available
                                warn!("Content parsing failed for {} after {} attempts, need new API call",
                                      model, content_retries);
                                // if this was the last api attempt, we failed completely for this model
                                if api_attempt == api_retries - 1 {
                                    error!(
                                        "Failed to get complete analysis from {} after all retries",
                                        model
                                    );
                                    return Err(format!("Failed to get complete analysis from {} after {} API attempts and {} content attempts per API call", model, api_retries, content_retries).into());
                                }
                                break; // break content loop to try new API call
                            }
                        }
                    }
                    Err(e) => {
                        error!("{} API attempt {} failed: {}", model, api_attempt + 1, e);
                        if api_attempt == api_retries - 1 {
                            return Err(e);
                        }
                    }
                }
            }
            // if we get here, all API attempts failed but didn't return Err - this shouldn't happen
            Err(format!(
                "Unexpected failure in {} after {} API attempts",
                model, api_retries
            )
            .into())
        }

        // try gemini-2.5-flash with retries
        match try_model_with_content_retries(prompt, "gemini-2.5-flash", 2, 2).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                warn!("Gemini Flash failed with error: {}, trying fallback", e);
            }
        }

        // try gemini-2.5-pro as fallback
        info!("Falling back to gemini-2.5-pro");
        match try_model_with_content_retries(prompt, "gemini-2.5-pro", 2, 2).await {
            Ok(result) => Ok(result),
            Err(e) => {
                error!("Gemini Pro fallback also failed: {}", e);
                Err(e)
            }
        }
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
const GEMINI_TIMEOUT_SECS: u64 = 300;

async fn query_llm(
    prompt: &str,
    model: &str,
) -> Result<LLMResponse, Box<dyn std::error::Error + Send + Sync>> {
    info!("Querying LLM with model: {}", model);

    // apply rate limiting before each attempt
    get_gemini_rate_limiter().wait_for_api_call().await;

    for attempt in 0..=MAX_RETRIES {
        let response = match timeout(
            Duration::from_secs(GEMINI_TIMEOUT_SECS),
            gemini_rs::chat(model).send_message(prompt),
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
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
            Err(_timeout) => {
                if attempt == MAX_RETRIES {
                    error!(
                        "Gemini API call timed out after {} attempts ({}s timeout)",
                        MAX_RETRIES + 1,
                        GEMINI_TIMEOUT_SECS
                    );
                    return Err("Gemini API call timed out".into());
                }

                let delay = calculate_delay(attempt);
                warn!(
                    "Gemini API call timed out (attempt {}/{}): {}s timeout. Retrying in {}ms",
                    attempt + 1,
                    MAX_RETRIES + 1,
                    GEMINI_TIMEOUT_SECS,
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
            "Received LLM response of length: {} (attempt {})",
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
