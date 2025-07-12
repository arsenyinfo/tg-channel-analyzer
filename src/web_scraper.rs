use log::{debug, error, info};
use reqwest::Client;
use scraper::{Html, Selector};
use serde_json::Value;
use std::time::Duration;
use tokio::time::timeout;

use crate::analysis::MessageDict;

#[derive(Debug)]
pub enum WebScrapingError {
    HttpError(reqwest::Error),
    ParseError(String),
    TimeoutError,
    InvalidUrl(String),
    StatusCodeError(u16),
}

impl std::fmt::Display for WebScrapingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebScrapingError::HttpError(e) => write!(f, "HTTP error: {}", e),
            WebScrapingError::ParseError(e) => write!(f, "Parse error: {}", e),
            WebScrapingError::TimeoutError => write!(f, "Operation timed out"),
            WebScrapingError::InvalidUrl(e) => write!(f, "Invalid URL: {}", e),
            WebScrapingError::StatusCodeError(code) => write!(f, "HTTP status code error: {}", code),
        }
    }
}

impl std::error::Error for WebScrapingError {}

impl From<reqwest::Error> for WebScrapingError {
    fn from(err: reqwest::Error) -> Self {
        WebScrapingError::HttpError(err)
    }
}

pub struct TelegramWebScraper {
    client: Client,
    cookies_initialized: bool,
}

impl TelegramWebScraper {
    pub fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        
        let client = Client::builder()
            .cookie_store(true) // enable automatic cookie handling
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36")
            .default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8".parse()?);
                headers.insert("Accept-Language", "en-US,en;q=0.9".parse()?);
                headers.insert("Accept-Encoding", "gzip, deflate".parse()?);
                headers.insert("Sec-Ch-Ua", "\"Google Chrome\";v=\"137\", \"Chromium\";v=\"137\", \"Not/A)Brand\";v=\"24\"".parse()?);
                headers.insert("Sec-Ch-Ua-Mobile", "?0".parse()?);
                headers.insert("Sec-Ch-Ua-Platform", "\"macOS\"".parse()?);
                headers.insert("Sec-Fetch-Dest", "document".parse()?);
                headers.insert("Sec-Fetch-Mode", "navigate".parse()?);
                headers.insert("Sec-Fetch-Site", "none".parse()?);
                headers.insert("Sec-Fetch-User", "?1".parse()?);
                headers.insert("Upgrade-Insecure-Requests", "1".parse()?);
                headers
            })
            .build()?;

        Ok(Self {
            client,
            cookies_initialized: false,
        })
    }

    async fn http_request_with_retry(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response, WebScrapingError> {
        let mut last_error = None;
        
        for attempt in 1..=3 {
            let request_clone = request.try_clone().ok_or_else(|| {
                WebScrapingError::ParseError("Failed to clone request".to_string())
            })?;
            
            match request_clone.send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    } else {
                        let error = WebScrapingError::StatusCodeError(status.as_u16());
                        error!("Attempt {}/3 failed with status code: {}", attempt, status);
                        last_error = Some(error);
                        
                        if attempt < 3 {
                            tokio::time::sleep(Duration::from_millis(1000 * attempt as u64)).await;
                        }
                    }
                }
                Err(e) => {
                    let error = WebScrapingError::HttpError(e);
                    error!("Attempt {}/3 failed with error: {}", attempt, error);
                    last_error = Some(error);
                    
                    if attempt < 3 {
                        tokio::time::sleep(Duration::from_millis(1000 * attempt as u64)).await;
                    }
                }
            }
        }
        
        Err(last_error.unwrap())
    }

    /// Scrape messages from a Telegram channel with 30-second timeout
    pub async fn scrape_channel_messages(
        &mut self,
        channel_url: &str,
        max_pages: usize,
    ) -> Result<Vec<MessageDict>, WebScrapingError> {
        let operation = self.scrape_channel_messages_impl(channel_url, max_pages);
        
        match timeout(Duration::from_secs(30), operation).await {
            Ok(result) => result,
            Err(_) => {
                error!("Web scraping operation timed out after 30 seconds");
                Err(WebScrapingError::TimeoutError)
            }
        }
    }

    async fn scrape_channel_messages_impl(
        &mut self,
        channel_url: &str,
        max_pages: usize,
    ) -> Result<Vec<MessageDict>, WebScrapingError> {
        info!("Starting web scraping for channel: {}", channel_url);

        let normalized_url = self.normalize_channel_url(channel_url)?;
        
        // initialize cookies first
        self.initialize_cookies(&normalized_url).await?;

        let mut all_messages = Vec::new();
        let mut before_id: Option<i64>;

        // get initial page
        info!("Fetching initial page: {}", normalized_url);
        let response = self.http_request_with_retry(self.client.get(&normalized_url)).await?;
        
        let html_content = response.text().await?;
        debug!("Initial page content length: {}", html_content.len());

        let (mut messages, last_id) = self.extract_messages_from_html(&html_content)?;
        all_messages.append(&mut messages);
        before_id = last_id;

        info!("Initial page: {} messages, last ID: {:?}", all_messages.len(), before_id);

        // fetch additional pages with pagination
        for page in 1..max_pages {
            if before_id.is_none() {
                break;
            }

            // add delay between requests to be polite
            tokio::time::sleep(Duration::from_millis(500)).await;

            info!("Fetching page {} with before_id: {:?}", page, before_id);
            
            let pagination_url = format!("{}?before={}", normalized_url, before_id.unwrap());
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("Accept", "application/json, text/javascript, */*; q=0.01".parse().unwrap());
            headers.insert("X-Requested-With", "XMLHttpRequest".parse().unwrap());
            headers.insert("Referer", normalized_url.parse().unwrap());
            headers.insert("Origin", "https://t.me".parse().unwrap());
            headers.insert("Sec-Fetch-Dest", "empty".parse().unwrap());
            headers.insert("Sec-Fetch-Mode", "cors".parse().unwrap());
            headers.insert("Sec-Fetch-Site", "same-origin".parse().unwrap());
            headers.insert("Content-Length", "0".parse().unwrap());

            let response = self.http_request_with_retry(
                self.client
                    .post(&pagination_url)
                    .headers(headers)
                    .body("")  // empty body for POST request
            ).await?;
            let response_text = response.text().await?;
            debug!("Pagination response length: {}", response_text.len());

            // pagination responses are JSON-encoded HTML
            let html_content = if response_text.starts_with('"') {
                // response is a JSON-encoded string, parse it
                match serde_json::from_str::<String>(&response_text) {
                    Ok(html) => {
                        debug!("Successfully decoded JSON-encoded HTML, length: {}", html.len());
                        html
                    },
                    Err(e) => {
                        debug!("Failed to decode JSON-encoded HTML: {}", e);
                        response_text
                    }
                }
            } else if response_text.starts_with('{') || response_text.starts_with('[') {
                // response is a JSON object, try to extract HTML string
                match serde_json::from_str::<Value>(&response_text) {
                    Ok(json) => json.as_str().unwrap_or(&response_text).to_string(),
                    Err(_) => response_text,
                }
            } else {
                response_text
            };

            let (mut page_messages, last_id) = self.extract_messages_from_html(&html_content)?;
            
            if page_messages.is_empty() {
                info!("No more messages found at page {}", page);
                break;
            }

            let page_count = page_messages.len();
            all_messages.append(&mut page_messages);
            before_id = last_id;

            info!("Page {}: {} messages, last ID: {:?}", page, page_count, before_id);
        }

        info!("Total extracted: {} non-forwarded messages", all_messages.len());
        Ok(all_messages)
    }

    fn normalize_channel_url(&self, channel_url: &str) -> Result<String, WebScrapingError> {
        let clean_url = if channel_url.starts_with('@') {
            format!("https://t.me/s/{}/", &channel_url[1..])
        } else if channel_url.starts_with("https://t.me/") && !channel_url.contains("/s/") {
            // convert t.me/channel to t.me/s/channel/
            let channel_name = channel_url.trim_start_matches("https://t.me/").trim_end_matches('/');
            format!("https://t.me/s/{}/", channel_name)
        } else if channel_url.starts_with("https://t.me/s/") {
            // already in correct format
            if channel_url.ends_with('/') {
                channel_url.to_string()
            } else {
                format!("{}/", channel_url)
            }
        } else {
            return Err(WebScrapingError::InvalidUrl(format!("Invalid channel URL: {}", channel_url)));
        };

        Ok(clean_url)
    }

    async fn initialize_cookies(&mut self, url: &str) -> Result<(), WebScrapingError> {
        if self.cookies_initialized {
            return Ok(());
        }

        info!("Initializing cookies for: {}", url);

        let base_url = if url.contains("/s/") {
            url.split("/s/").next().unwrap_or("https://t.me").to_string() + "/"
        } else {
            "https://t.me/".to_string()
        };

        debug!("Initializing cookies from base URL: {}", base_url);

        let _response = self.http_request_with_retry(self.client.get(&base_url)).await?;

        // note: automatic cookie handling is built into reqwest::Client
        debug!("Cookie initialization completed");
        self.cookies_initialized = true;

        Ok(())
    }

    fn extract_messages_from_html(
        &self,
        html_content: &str,
    ) -> Result<(Vec<MessageDict>, Option<i64>), WebScrapingError> {
        let document = Html::parse_document(html_content);
        
        // css selectors equivalent to Python's BeautifulSoup
        let message_wrap_selector = Selector::parse("div.tgme_widget_message_wrap")
            .map_err(|e| WebScrapingError::ParseError(format!("Invalid selector: {}", e)))?;
        
        let data_post_selector = Selector::parse("div[data-post]")
            .map_err(|e| WebScrapingError::ParseError(format!("Invalid selector: {}", e)))?;
        
        let forwarded_selector = Selector::parse("div.tgme_widget_message_forwarded_from")
            .map_err(|e| WebScrapingError::ParseError(format!("Invalid selector: {}", e)))?;
        
        let text_selector = Selector::parse("div.tgme_widget_message_text")
            .map_err(|e| WebScrapingError::ParseError(format!("Invalid selector: {}", e)))?;

        let mut messages = Vec::new();
        let mut all_message_ids = Vec::new();

        let message_wraps: Vec<_> = document.select(&message_wrap_selector).collect();
        debug!("Found {} message wraps", message_wraps.len());

        for wrap in message_wraps {
            let mut current_message_id: Option<i64> = None;

            // extract message ID from data-post attribute
            if let Some(message_elem) = wrap.select(&data_post_selector).next() {
                if let Some(data_post) = message_elem.value().attr("data-post") {
                    // data-post format is "channel_name/message_id" or "channel_name/message_idg"
                    if let Some(post_id_str) = data_post.split('/').last() {
                        // remove any non-numeric suffixes like 'g'
                        let numeric_part: String = post_id_str.chars().filter(|c| c.is_ascii_digit()).collect();
                        if !numeric_part.is_empty() {
                            if let Ok(id) = numeric_part.parse::<i64>() {
                                current_message_id = Some(id);
                                all_message_ids.push(id);
                            }
                        }
                    }
                }
            }

            // check if this is a forwarded message
            if wrap.select(&forwarded_selector).next().is_some() {
                continue; // skip forwarded messages
            }

            // find the message text container
            if let Some(text_elem) = wrap.select(&text_selector).next() {
                let text = text_elem.text().collect::<Vec<_>>().join("\n").trim().to_string();
                if !text.is_empty() && current_message_id.is_some() {
                    messages.push(MessageDict {
                        date: None, // date extraction can be added later if needed
                        message: Some(text),
                    });
                }
            }
        }

        // for pagination, we need the minimum (oldest) message ID from this page
        let last_message_id = if !all_message_ids.is_empty() {
            let min_id = *all_message_ids.iter().min().unwrap();
            debug!("Message IDs on page: {:?}, using {} for next pagination", all_message_ids, min_id);
            Some(min_id)
        } else {
            None
        };

        Ok((messages, last_message_id))
    }
}