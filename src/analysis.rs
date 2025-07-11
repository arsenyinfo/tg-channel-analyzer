use grammers_client::{Client, Config, InitParams};
use grammers_session::Session;
use log::{error, info, warn};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::env;
use std::time::Duration;
use tokio::time::sleep;

use crate::cache::{AnalysisResult, CacheManager};
use deadpool_postgres::Pool;

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

        Ok(Self {
            client: None,
            api_id,
            api_hash,
            cache,
        })
    }

    async fn ensure_client(&mut self) -> Result<&Client, Box<dyn std::error::Error + Send + Sync>> {
        if self.client.is_none() {
            info!("Initializing Telegram client...");

            let session = match Session::load_file("session_name.session") {
                Ok(session) => {
                    info!("Loaded existing session");
                    session
                }
                Err(_) => {
                    info!("No existing session found, creating new one");
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

            let client = Client::connect(config).await?;

            if !client.is_authorized().await? {
                return Err("Client is not authorized. Please run the standalone analyzer first to authorize.".into());
            }

            self.client = Some(client);
        }

        Ok(self.client.as_ref().unwrap())
    }

    pub async fn validate_channel(
        &mut self,
        channel_username: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let client = self.ensure_client().await?;

        let clean_username = if channel_username.starts_with('@') {
            &channel_username[1..]
        } else {
            channel_username
        };

        info!("Validating channel: {}", clean_username);

        match client.resolve_username(clean_username).await {
            Ok(Some(_)) => {
                info!("Channel {} is valid and accessible", clean_username);
                Ok(true)
            }
            Ok(None) => {
                info!("Channel {} not found", clean_username);
                Ok(false)
            }
            Err(e) => {
                error!("Error validating channel {}: {}", clean_username, e);
                Err(e.into())
            }
        }
    }

    pub async fn analyze_channel(
        &mut self,
        channel_username: &str,
    ) -> Result<AnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
        info!("Starting analysis for channel: {}", channel_username);

        let messages = match self.cache.load_channel_messages(channel_username).await {
            Some(cached_messages) => cached_messages,
            None => {
                info!("Fetching fresh messages from channel");
                self.ensure_client().await?;
                let messages = {
                    let client = self.client.as_ref().unwrap();
                    self.get_all_messages(client, channel_username).await?
                };
                self.cache
                    .save_channel_messages(channel_username, &messages)
                    .await?;
                messages
            }
        };

        let messages_json = serde_json::to_string_pretty(&messages)?;

        // check LLM cache first
        let cache_key = self.cache.get_llm_cache_key(&messages, "analysis");
        if let Some(cached_result) = self.cache.load_llm_result(&cache_key).await {
            return Ok(cached_result);
        }

        let prompt = format!(
            "Given the following messages from a Telegram channel, analyze the author profile and write three blocks of text in the following format:

            <professional>This part should contain a professional analysis of the author, including their expertise, background, and any relevant professional details, as an advice to the hiring manager</professional>
            <personal>This part should contain a personal analysis of the author, as written by a professional psychologist for non-professional reader</personal>
            <roast>This part should contain a roast of the author, as written by a friend of theirs, that is somewhat harsh</roast>
            Each part should be around 2048 characters long, in the language of the messages.

Messages:
{}",
            messages_json
        );

        info!("Querying LLM for analysis...");
        let llm_response = query_llm(&prompt, "gemini-2.5-flash").await?;

        let professional = extract_tag(&llm_response.content, "professional");
        let personal = extract_tag(&llm_response.content, "personal");
        let roast = extract_tag(&llm_response.content, "roast");

        let result = AnalysisResult {
            professional: professional,
            personal: personal,
            roast: roast,
            messages_count: messages.len(),
        };

        // cache the LLM result
        if let Err(e) = self.cache.save_llm_result(&cache_key, &result).await {
            info!("Failed to cache LLM result: {}", e);
        }

        Ok(result)
    }

    async fn get_all_messages(
        &self,
        client: &Client,
        channel_username: &str,
    ) -> Result<Vec<MessageDict>, Box<dyn std::error::Error + Send + Sync>> {
        info!("Getting messages from {}", channel_username);

        let mut messages = Vec::new();

        let clean_username = if channel_username.starts_with('@') {
            &channel_username[1..]
        } else {
            channel_username
        };

        let channel = client.resolve_username(clean_username).await?;

        let mut skipped = 0;
        if let Some(chat) = channel {
            let mut message_iter = client.iter_messages(&chat);

            while let Some(message) = message_iter.next().await? {
                if message.forward_header().is_some() {
                    skipped += 1;
                    continue;
                }
                if message.text().len() < 32 {
                    skipped += 1;
                    continue;
                }

                messages.push(MessageDict {
                    date: Some(message.date().to_rfc2822()),
                    message: Some(message.text().to_string()),
                });

                if messages.len() >= 200 {
                    break;
                }
            }
        }

        info!("Retrieved {} messages, skipped {}", messages.len(), skipped);
        Ok(messages)
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
