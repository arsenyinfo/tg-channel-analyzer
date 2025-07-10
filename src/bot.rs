use log::{error, info};
use regex::Regex;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{ChatId, ParseMode};
use teloxide::utils::command::BotCommands;
use tokio::sync::Mutex;

use crate::analysis::AnalysisEngine;
use crate::cache::AnalysisResult;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Supported commands:")]
pub enum Command {
    #[command(description = "start the bot")]
    Start,
}

pub struct TelegramBot {
    bot: Bot,
    analysis_engine: Arc<Mutex<AnalysisEngine>>,
}

impl TelegramBot {
    fn escape_markdown_v2(text: &str) -> String {
        // use safe placeholders that won't get escaped
        let mut result = text.to_string();
        
        // first escape backslashes
        result = result.replace("\\", "\\\\");
        
        let bold_re = Regex::new(r"\*\*([^*]+)\*\*").unwrap();
        let underline_re = Regex::new(r"__([^_]+)__").unwrap();
        let italic_re = Regex::new(r"\*([^*]+)\*").unwrap();
        
        // use safe placeholder format with no special chars
        let mut replacements = Vec::new();
        let mut counter = 0;
        
        // process bold first (**text** -> *text* for MarkdownV2)
        result = bold_re.replace_all(&result, |caps: &regex::Captures| {
            let content = &caps[1];
            let escaped_content = Self::escape_content_only(content);
            let placeholder = format!("SAFEPLACEHOLDERBOLD{}", counter);
            replacements.push((placeholder.clone(), format!("*{}*", escaped_content)));
            counter += 1;
            placeholder
        }).to_string();
        
        // process underline (__text__ -> __text__ for MarkdownV2)  
        result = underline_re.replace_all(&result, |caps: &regex::Captures| {
            let content = &caps[1];
            let escaped_content = Self::escape_content_only(content);
            let placeholder = format!("SAFEPLACEHOLDERUNDERLINE{}", counter);
            replacements.push((placeholder.clone(), format!("__{}__", escaped_content)));
            counter += 1;
            placeholder
        }).to_string();
        
        // process italic (*text* -> _text_ for MarkdownV2)
        result = italic_re.replace_all(&result, |caps: &regex::Captures| {
            let content = &caps[1];
            let escaped_content = Self::escape_content_only(content);
            let placeholder = format!("SAFEPLACEHOLDERITALIC{}", counter);
            replacements.push((placeholder.clone(), format!("_{}_", escaped_content)));
            counter += 1;
            placeholder
        }).to_string();
        
        // escape all remaining special characters
        result = result
            .replace("_", "\\_")
            .replace("[", "\\[")
            .replace("]", "\\]")
            .replace("(", "\\(")
            .replace(")", "\\)")
            .replace("~", "\\~")
            .replace(">", "\\>")
            .replace("#", "\\#")
            .replace("+", "\\+")
            .replace("-", "\\-")
            .replace("=", "\\=")
            .replace("|", "\\|")
            .replace("{", "\\{")
            .replace("}", "\\}")
            .replace(".", "\\.")
            .replace("!", "\\!")
            .replace("*", "\\*");
        
        // restore formatted content
        for (placeholder, replacement) in replacements {
            result = result.replace(&placeholder, &replacement);
        }
        
        result
    }
    
    fn escape_content_only(text: &str) -> String {
        text.replace("[", "\\[")
            .replace("]", "\\]")
            .replace("(", "\\(")
            .replace(")", "\\)")
            .replace("~", "\\~")
            .replace(">", "\\>")
            .replace("#", "\\#")
            .replace("+", "\\+")
            .replace("-", "\\-")
            .replace("=", "\\=")
            .replace("|", "\\|")
            .replace("{", "\\{")
            .replace("}", "\\}")
            .replace(".", "\\.")
            .replace("!", "\\!")
    }

    pub fn new(bot_token: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let bot = Bot::new(bot_token);
        let analysis_engine = Arc::new(Mutex::new(AnalysisEngine::new()?));

        Ok(Self {
            bot,
            analysis_engine,
        })
    }

    pub async fn run(&self) {
        info!("Starting Telegram bot...");

        let handler = Update::filter_message()
            .branch(
                dptree::entry()
                    .filter_command::<Command>()
                    .endpoint(Self::handle_command),
            )
            .branch(dptree::endpoint(Self::handle_message));

        Dispatcher::builder(self.bot.clone(), handler)
            .dependencies(dptree::deps![
                self.analysis_engine.clone()
            ])
            .enable_ctrlc_handler()
            .build()
            .dispatch()
            .await;
    }

    async fn handle_command(bot: Bot, msg: Message, cmd: Command) -> ResponseResult<()> {
        match cmd {
            Command::Start => {
                let intro_text = "🤖 <b>Channel Analyzer Bot</b>\n\n\
                    Welcome! I can analyze Telegram channels and provide insights.\n\n\
                    📋 <b>How to use:</b>\n\
                    • Send me a channel username (e.g., <code>@channelname</code>)\n\
                    • I'll validate the channel and start analysis\n\
                    • Results will be sent directly to you here\n\n\
                    ⚡ <b>Features:</b>\n\
                    • Deep content analysis using AI\n\
                    • Author profile insights\n\
                    • Channel activity patterns\n\n\
                    Just send me a channel name to get started!";

                bot.send_message(msg.chat.id, intro_text)
                    .parse_mode(ParseMode::Html)
                    .await?;
            }
        }
        Ok(())
    }

    async fn handle_message(
        bot: Bot,
        msg: Message,
        analysis_engine: Arc<Mutex<AnalysisEngine>>,
    ) -> ResponseResult<()> {
        if let Some(text) = msg.text() {
            let text = text.trim();

            // check if message looks like a channel username
            if text.starts_with('@') && text.len() > 1 {
                info!("Received channel analysis request: {}", text);

                // send immediate response
                bot.send_message(
                    msg.chat.id,
                    "🔍 Validating channel and starting analysis...",
                )
                .await?;

                // validate and analyze channel
                let mut engine = analysis_engine.lock().await;
                match engine.validate_channel(text).await {
                    Ok(true) => {
                        drop(engine); // release lock before long operation

                        // start analysis in background
                        let bot_clone = bot.clone();
                        let user_chat_id = msg.chat.id;
                        let channel_name = text.to_string();
                        let analysis_engine_clone = analysis_engine.clone();

                        tokio::spawn(async move {
                            if let Err(e) = Self::perform_analysis(
                                bot_clone.clone(),
                                user_chat_id,
                                channel_name,
                                analysis_engine_clone,
                            )
                            .await
                            {
                                error!("Analysis failed: {}", e);
                                let _ = bot_clone
                                    .send_message(
                                        user_chat_id,
                                        "❌ Analysis failed. Please try again later.",
                                    )
                                    .await;
                            }
                        });
                    }
                    Ok(false) => {
                        bot.send_message(
                            msg.chat.id,
                            "❌ Channel not found or not accessible. Please check the channel name and try again.",
                        ).await?;
                    }
                    Err(e) => {
                        error!("Channel validation error: {}", e);
                        bot.send_message(
                            msg.chat.id,
                            "❌ Error validating channel. Please try again later.",
                        )
                        .await?;
                    }
                }
            } else {
                // send help message for invalid input
                bot.send_message(
                    msg.chat.id,
                    "❓ Please send a valid channel username starting with '@' (e.g., @channelname)\n\nUse /start to see the full instructions.",
                ).await?;
            }
        }
        Ok(())
    }

    async fn perform_analysis(
        bot: Bot,
        user_chat_id: ChatId,
        channel_name: String,
        analysis_engine: Arc<Mutex<AnalysisEngine>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("Starting analysis for channel: {}", channel_name);

        // notify user that analysis is starting
        bot.send_message(
            user_chat_id,
            "✅ Channel validated! Starting analysis... This may take a few minutes.",
        )
        .await?;

        // perform analysis
        let mut engine = analysis_engine.lock().await;
        let result = engine.analyze_channel(&channel_name).await?;
        drop(engine);

        // notify user that analysis is complete and send results
        bot.send_message(
            user_chat_id,
            "✅ Analysis complete! Here are your results:",
        )
        .await?;

        // send results directly to user
        Self::send_results_to_user(bot, user_chat_id, &channel_name, result).await?;

        Ok(())
    }

    async fn send_results_to_user(
        bot: Bot,
        user_chat_id: ChatId,
        channel_name: &str,
        result: AnalysisResult,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let header = format!(
            "📊 *Channel Analysis Results*\n\n\
            🎯 *Channel:* `{}`\n\n",
            Self::escape_markdown_v2(channel_name)
        );

        let mut parts_sent = 0;
        for (analysis_type, analysis_content) in [
            ("Professional", &result.professional),
            ("Personal", &result.personal),
            ("Roast", &result.roast),
        ] {
            match analysis_content {
                Some(content) if !content.is_empty() => {
                    let analysis_header = format!("🔍 *{} Analysis:*\n\n", analysis_type);
                    let escaped_content = Self::escape_markdown_v2(content);
                    let full_message = format!("{}{}{}", header, analysis_header, escaped_content);

                    bot.send_message(user_chat_id, full_message)
                        .parse_mode(ParseMode::MarkdownV2)
                        .await?;
                    parts_sent += 1;
                }
                _ => {
                    info!(
                        "No {} analysis content available for channel: {}",
                        analysis_type, channel_name
                    );
                }
            }
        }

        info!(
            "Results sent to user for {} (split into {} parts)",
            channel_name, parts_sent
        );
        Ok(())
    }
}
