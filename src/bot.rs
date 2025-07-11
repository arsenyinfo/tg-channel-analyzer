use log::{error, info};
use regex::Regex;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{ChatId, ParseMode};
use teloxide::utils::command::BotCommands;
use tokio::sync::Mutex;

use crate::analysis::AnalysisEngine;
use crate::cache::AnalysisResult;
use crate::user_manager::UserManager;
use deadpool_postgres::Pool;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Supported commands:")]
pub enum Command {
    #[command(description = "start the bot")]
    Start,
    #[command(description = "add credits to your account")]
    AddCredits { amount: i32 },
}

pub struct TelegramBot {
    bot: Bot,
    analysis_engine: Arc<Mutex<AnalysisEngine>>,
    user_manager: Arc<UserManager>,
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
        result = bold_re
            .replace_all(&result, |caps: &regex::Captures| {
                let content = &caps[1];
                let escaped_content = Self::escape_content_only(content);
                let placeholder = format!("SAFEPLACEHOLDERBOLD{}", counter);
                replacements.push((placeholder.clone(), format!("*{}*", escaped_content)));
                counter += 1;
                placeholder
            })
            .to_string();

        // process underline (__text__ -> __text__ for MarkdownV2)
        result = underline_re
            .replace_all(&result, |caps: &regex::Captures| {
                let content = &caps[1];
                let escaped_content = Self::escape_content_only(content);
                let placeholder = format!("SAFEPLACEHOLDERUNDERLINE{}", counter);
                replacements.push((placeholder.clone(), format!("__{}__", escaped_content)));
                counter += 1;
                placeholder
            })
            .to_string();

        // process italic (*text* -> _text_ for MarkdownV2)
        result = italic_re
            .replace_all(&result, |caps: &regex::Captures| {
                let content = &caps[1];
                let escaped_content = Self::escape_content_only(content);
                let placeholder = format!("SAFEPLACEHOLDERITALIC{}", counter);
                replacements.push((placeholder.clone(), format!("_{}_", escaped_content)));
                counter += 1;
                placeholder
            })
            .to_string();

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

    pub async fn new(
        bot_token: &str,
        user_manager: Arc<UserManager>,
        pool: Pool,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let bot = Bot::new(bot_token);
        let analysis_engine = Arc::new(Mutex::new(AnalysisEngine::new(pool)?));

        Ok(Self {
            bot,
            analysis_engine,
            user_manager,
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
                self.analysis_engine.clone(),
                self.user_manager.clone()
            ])
            .enable_ctrlc_handler()
            .build()
            .dispatch()
            .await;
    }

    async fn handle_command(
        bot: Bot,
        msg: Message,
        cmd: Command,
        user_manager: Arc<UserManager>,
    ) -> ResponseResult<()> {
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
            Command::AddCredits { amount } => {
                if amount <= 0 || amount > 100 {
                    bot.send_message(msg.chat.id, "❌ Invalid amount. Please specify between 1 and 100 credits.")
                        .await?;
                    return Ok(());
                }
                
                let telegram_user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
                if telegram_user_id == 0 {
                    bot.send_message(msg.chat.id, "❌ Could not identify user.")
                        .await?;
                    return Ok(());
                }
                
                match user_manager.add_credits(telegram_user_id, amount).await {
                    Ok(new_balance) => {
                        bot.send_message(
                            msg.chat.id,
                            format!("✅ Successfully added {} credits! Your new balance is: {} credits", amount, new_balance)
                        )
                        .await?;
                    }
                    Err(e) => {
                        error!("Failed to add credits: {}", e);
                        bot.send_message(msg.chat.id, "❌ Failed to add credits. Please try again later.")
                            .await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_message(
        bot: Bot,
        msg: Message,
        analysis_engine: Arc<Mutex<AnalysisEngine>>,
        user_manager: Arc<UserManager>,
    ) -> ResponseResult<()> {
        if let Some(text) = msg.text() {
            let text = text.trim();

            // check if message looks like a channel username
            if text.starts_with('@') && text.len() > 1 {
                info!("Received channel analysis request: {}", text);

                // get user info from telegram message
                let telegram_user_id = msg.from.as_ref().map(|user| user.id.0 as i64).unwrap_or(0);
                let username = msg.from.as_ref().and_then(|user| user.username.as_deref());
                let first_name = msg.from.as_ref().map(|user| user.first_name.as_str());
                let last_name = msg.from.as_ref().and_then(|user| user.last_name.as_deref());

                // get or create user and check credits
                let user = match user_manager
                    .get_or_create_user(telegram_user_id, username, first_name, last_name)
                    .await
                {
                    Ok(user) => user,
                    Err(e) => {
                        error!("Failed to get/create user: {}", e);
                        bot.send_message(
                            msg.chat.id,
                            "❌ Error processing user request. Please try again later.",
                        )
                        .await?;
                        return Ok(());
                    }
                };

                // check if user has credits
                if user.analysis_credits <= 0 {
                    let no_credits_msg = format!(
                        "❌ *No Analysis Credits Available*\n\n\
                        You have used all your free analysis credits\\.\n\n\
                        💰 *Add More Credits:*\n\
                        Use `/addcredits <amount>` to add more credits\\.\n\
                        Example: `/addcredits 5`\n\n\
                        📊 *Your Stats:*\n\
                        • Credits remaining: `{}`\n\
                        • Total analyses performed: `{}`",
                        user.analysis_credits, user.total_analyses_performed
                    );

                    bot.send_message(msg.chat.id, no_credits_msg)
                        .parse_mode(ParseMode::MarkdownV2)
                        .await?;
                    return Ok(());
                }

                // send immediate response with credit info
                let credits_msg = format!(
                    "🔍 Validating channel and starting analysis\\.\\.\\.\n\n\
                    💳 Credits remaining after this analysis: `{}`",
                    user.analysis_credits - 1
                );
                bot.send_message(msg.chat.id, credits_msg)
                    .parse_mode(ParseMode::MarkdownV2)
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
                        let user_manager_clone = user_manager.clone();

                        tokio::spawn(async move {
                            if let Err(e) = Self::perform_analysis(
                                bot_clone.clone(),
                                user_chat_id,
                                channel_name,
                                analysis_engine_clone,
                                user_manager_clone,
                                telegram_user_id,
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
        user_manager: Arc<UserManager>,
        telegram_user_id: i64,
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

        // consume credit after successful analysis
        let remaining_credits = match user_manager
            .consume_credit(telegram_user_id, &channel_name)
            .await
        {
            Ok(credits) => credits,
            Err(e) => {
                error!(
                    "Failed to consume credit for user {}: {}",
                    telegram_user_id, e
                );
                bot.send_message(
                    user_chat_id,
                    "⚠️ Analysis completed but failed to update credits. Please contact support.",
                )
                .await?;
                return Err(e);
            }
        };

        // notify user that analysis is complete and send results with credit info
        let completion_msg = format!(
            "✅ *Analysis Complete\\!*\n\n\
            📊 Your results are ready\\.\n\
            💳 Credits remaining: `{}`",
            remaining_credits
        );
        bot.send_message(user_chat_id, completion_msg)
            .parse_mode(ParseMode::MarkdownV2)
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
