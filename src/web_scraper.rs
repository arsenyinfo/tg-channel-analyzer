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
            WebScrapingError::StatusCodeError(code) => {
                write!(f, "HTTP status code error: {}", code)
            }
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

    async fn http_request_with_retry(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, WebScrapingError> {
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

        self.scrape_normalized_url(&normalized_url, max_pages).await
    }

    async fn scrape_normalized_url(
        &mut self,
        normalized_url: &str,
        max_pages: usize,
    ) -> Result<Vec<MessageDict>, WebScrapingError> {
        // initialize cookies first
        self.initialize_cookies(normalized_url).await?;

        let mut all_messages = Vec::new();
        let mut before_id: Option<i64>;

        // get initial page
        info!("Fetching initial page: {}", normalized_url);
        let response = self
            .http_request_with_retry(self.client.get(normalized_url))
            .await?;

        let html_content = response.text().await?;
        debug!("Initial page content length: {}", html_content.len());

        let (mut messages, last_id) = self.extract_messages_from_html(&html_content)?;
        all_messages.append(&mut messages);
        before_id = last_id;

        info!(
            "Initial page: {} messages, last ID: {:?}",
            all_messages.len(),
            before_id
        );

        // fetch additional pages with pagination
        for page in 1..max_pages {
            let Some(previous_id) = before_id else {
                break;
            };

            // add delay between requests to be polite
            tokio::time::sleep(Duration::from_millis(500)).await;

            info!("Fetching page {} with before_id: {:?}", page, before_id);

            let pagination_url = format!("{}?before={}", normalized_url, previous_id);
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                "Accept",
                "application/json, text/javascript, */*; q=0.01"
                    .parse()
                    .unwrap(),
            );
            headers.insert("X-Requested-With", "XMLHttpRequest".parse().unwrap());
            headers.insert("Referer", normalized_url.parse().unwrap());
            headers.insert("Origin", "https://t.me".parse().unwrap());
            headers.insert("Sec-Fetch-Dest", "empty".parse().unwrap());
            headers.insert("Sec-Fetch-Mode", "cors".parse().unwrap());
            headers.insert("Sec-Fetch-Site", "same-origin".parse().unwrap());
            headers.insert("Content-Length", "0".parse().unwrap());

            let response = self
                .http_request_with_retry(
                    self.client.post(&pagination_url).headers(headers).body(""), // empty body for POST request
                )
                .await?;
            let response_text = response.text().await?;
            debug!("Pagination response length: {}", response_text.len());

            // pagination responses are JSON-encoded HTML
            let html_content = if response_text.starts_with('"') {
                // response is a JSON-encoded string, parse it
                match serde_json::from_str::<String>(&response_text) {
                    Ok(html) => {
                        debug!(
                            "Successfully decoded JSON-encoded HTML, length: {}",
                            html.len()
                        );
                        html
                    }
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

            // Forwarded posts still advance the raw cursor even when none of this page's
            // messages are retained. Stop only when there is no progress toward older posts,
            // before appending a repeated page's messages.
            if !last_id.is_some_and(|id| id < previous_id) {
                info!("No older messages found at page {}", page);
                break;
            }

            let page_count = page_messages.len();
            all_messages.append(&mut page_messages);
            before_id = last_id;

            info!(
                "Page {}: {} messages, last ID: {:?}",
                page, page_count, before_id
            );
        }

        info!(
            "Total extracted: {} non-forwarded messages",
            all_messages.len()
        );
        Ok(all_messages)
    }

    fn normalize_channel_url(&self, channel_url: &str) -> Result<String, WebScrapingError> {
        let clean_url = if let Some(channel_name) = channel_url.strip_prefix('@') {
            format!("https://t.me/s/{}/", channel_name)
        } else if channel_url.starts_with("https://t.me/") && !channel_url.contains("/s/") {
            // convert t.me/channel to t.me/s/channel/
            let channel_name = channel_url
                .trim_start_matches("https://t.me/")
                .trim_end_matches('/');
            format!("https://t.me/s/{}/", channel_name)
        } else if channel_url.starts_with("https://t.me/s/") {
            // already in correct format
            if channel_url.ends_with('/') {
                channel_url.to_string()
            } else {
                format!("{}/", channel_url)
            }
        } else {
            return Err(WebScrapingError::InvalidUrl(format!(
                "Invalid channel URL: {}",
                channel_url
            )));
        };

        Ok(clean_url)
    }

    async fn initialize_cookies(&mut self, url: &str) -> Result<(), WebScrapingError> {
        if self.cookies_initialized {
            return Ok(());
        }

        info!("Initializing cookies for: {}", url);

        let base_url = if url.contains("/s/") {
            url.split("/s/")
                .next()
                .unwrap_or("https://t.me")
                .to_string()
                + "/"
        } else {
            "https://t.me/".to_string()
        };

        debug!("Initializing cookies from base URL: {}", base_url);

        let _response = self
            .http_request_with_retry(self.client.get(&base_url))
            .await?;

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

        let image_selector = Selector::parse("a.tgme_widget_message_photo_wrap")
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
                    if let Some(post_id_str) = data_post.split('/').next_back() {
                        // remove any non-numeric suffixes like 'g'
                        let numeric_part: String =
                            post_id_str.chars().filter(|c| c.is_ascii_digit()).collect();
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

            // extract images
            let mut image_urls = Vec::new();
            for image_elem in wrap.select(&image_selector) {
                if let Some(style_attr) = image_elem.value().attr("style") {
                    // extract URL from background-image: url('...')
                    if style_attr.contains("background-image") {
                        if let Some(start) = style_attr.find("url('") {
                            let url_start = start + 5;
                            if let Some(end) = style_attr[url_start..].find("')") {
                                let image_url = &style_attr[url_start..url_start + end];
                                // skip emoji images and duplicates
                                if !image_url.contains("/emoji/")
                                    && !image_urls.contains(&image_url.to_string())
                                {
                                    image_urls.push(image_url.to_string());
                                    debug!("Found image URL: {}", image_url);
                                }
                            }
                        }
                    }
                }
            }

            // find the message text container
            if let Some(text_elem) = wrap.select(&text_selector).next() {
                let text = text_elem
                    .text()
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string();
                if (!text.is_empty() || !image_urls.is_empty()) && current_message_id.is_some() {
                    messages.push(MessageDict {
                        date: None, // date extraction can be added later if needed
                        message: Some(text),
                        images: if image_urls.is_empty() {
                            None
                        } else {
                            Some(image_urls)
                        },
                    });
                }
            } else if !image_urls.is_empty() && current_message_id.is_some() {
                // message with only images, no text
                messages.push(MessageDict {
                    date: None,
                    message: None,
                    images: Some(image_urls),
                });
            }
        }

        // for pagination, we need the minimum (oldest) message ID from this page
        let last_message_id = if !all_message_ids.is_empty() {
            let min_id = *all_message_ids.iter().min().unwrap();
            debug!(
                "Message IDs on page: {:?}, using {} for next pagination",
                all_message_ids, min_id
            );
            Some(min_id)
        } else {
            None
        };

        Ok((messages, last_message_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn post(id: i64, text: &str, forwarded: bool) -> String {
        let forwarded = if forwarded {
            "<div class=\"tgme_widget_message_forwarded_from\">Source</div>"
        } else {
            ""
        };
        format!(
            "<div class=\"tgme_widget_message_wrap\"><div data-post=\"channel/{id}\">{forwarded}<div class=\"tgme_widget_message_text\">{text}</div></div></div>"
        )
    }

    async fn scrape_pages(pages: Vec<(&str, String)>, max_pages: usize) -> Vec<MessageDict> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/s/channel/", listener.local_addr().unwrap());
        let pages: Vec<_> = pages
            .into_iter()
            .map(|(request, body)| (request.to_string(), body))
            .collect();
        let server = tokio::spawn(async move {
            for (expected_request, body) in pages {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 1024];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0, "request ended before its headers");
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        break;
                    }
                }
                assert_eq!(
                    String::from_utf8_lossy(&request).lines().next().unwrap(),
                    expected_request
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let mut scraper = TelegramWebScraper {
            client: Client::builder().no_proxy().build().unwrap(),
            cookies_initialized: true,
        };
        let (messages, served) = timeout(Duration::from_secs(10), async {
            tokio::join!(scraper.scrape_normalized_url(&url, max_pages), server)
        })
        .await
        .expect("scraping fixture timed out");
        served.unwrap();
        messages.unwrap()
    }

    #[tokio::test]
    async fn pagination_continues_past_forwarded_only_pages() {
        let messages = scrape_pages(
            vec![
                ("GET /s/channel/ HTTP/1.1", post(30, "Newest", false)),
                (
                    "POST /s/channel/?before=30 HTTP/1.1",
                    serde_json::to_string(&post(20, "Forwarded", true)).unwrap(),
                ),
                (
                    "POST /s/channel/?before=20 HTTP/1.1",
                    serde_json::to_string(&post(10, "Oldest", false)).unwrap(),
                ),
            ],
            3,
        )
        .await;
        assert_eq!(
            messages
                .iter()
                .map(|message| message.message.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("Newest"), Some("Oldest")]
        );
    }

    #[tokio::test]
    async fn pagination_stops_without_an_older_cursor() {
        for page in [
            String::new(),
            post(30, "Repeated", false),
            post(31, "Newer", false),
        ] {
            let messages = scrape_pages(
                vec![
                    ("GET /s/channel/ HTTP/1.1", post(30, "Newest", false)),
                    ("POST /s/channel/?before=30 HTTP/1.1", page),
                ],
                3,
            )
            .await;
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].message.as_deref(), Some("Newest"));
        }
    }
}
