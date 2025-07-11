use comrak::{markdown_to_html, ComrakOptions};
use html_escape;
use log::{error, info};
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{
    CallbackQuery, ChatId, InlineKeyboardButton, InlineKeyboardMarkup, LabeledPrice, ParseMode,
    PreCheckoutQuery, SuccessfulPayment,
};
use teloxide::utils::command::BotCommands;
use tokio::sync::Mutex;

use crate::analysis::AnalysisEngine;
use crate::cache::AnalysisResult;
use crate::user_manager::UserManager;
use deadpool_postgres::Pool;

// payment configuration constants
const CREDITS_1_PRICE: u32 = 10;
const CREDITS_10_PRICE: u32 = 50;
const CREDITS_1_AMOUNT: i32 = 1;
const CREDITS_10_AMOUNT: i32 = 10;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Supported commands:")]
pub enum Command {
    #[command(description = "start the bot")]
    Start,
    #[command(description = "buy 1 analysis for 10 star")]
    Buy1,
    #[command(description = "buy 10 analyses for 50 stars")]
    Buy10,
}

pub struct TelegramBot {
    bot: Bot,
    analysis_engine: Arc<Mutex<AnalysisEngine>>,
    user_manager: Arc<UserManager>,
}

impl TelegramBot {
    fn create_payment_keyboard() -> InlineKeyboardMarkup {
        let buy1_button = InlineKeyboardButton::callback(
            format!(
                "💎 Buy {} Credit ({} ⭐)",
                CREDITS_1_AMOUNT, CREDITS_1_PRICE
            ),
            "buy_1",
        );
        let buy10_button = InlineKeyboardButton::callback(
            format!(
                "💎 Buy {} Credits ({} ⭐)",
                CREDITS_10_AMOUNT, CREDITS_10_PRICE
            ),
            "buy_10",
        );

        InlineKeyboardMarkup::new(vec![vec![buy1_button], vec![buy10_button]])
    }

    fn create_analysis_selection_keyboard(channel_name: &str) -> InlineKeyboardMarkup {
        let professional_button = InlineKeyboardButton::callback(
            "💼 Professional Analysis",
            format!("analysis_professional_{}", channel_name),
        );
        let personal_button = InlineKeyboardButton::callback(
            "🧠 Personal Analysis",
            format!("analysis_personal_{}", channel_name),
        );
        let roast_button = InlineKeyboardButton::callback(
            "🔥 Roast Analysis",
            format!("analysis_roast_{}", channel_name),
        );

        InlineKeyboardMarkup::new(vec![
            vec![professional_button],
            vec![personal_button],
            vec![roast_button],
        ])
    }

    fn escape_html(text: &str) -> String {
        // use proper HTML escaping library
        html_escape::encode_text(text).to_string()
    }

    fn markdown_to_html_safe(text: &str) -> String {
        // convert markdown to HTML with Telegram-compatible options
        let mut options = ComrakOptions::default();
        options.extension.strikethrough = true;
        options.extension.autolink = true;
        options.render.hardbreaks = true;
        options.render.unsafe_ = false;

        let html = markdown_to_html(text, &options);

        // telegram HTML mode only supports: b, i, u, s, code, pre, a
        // replace unsupported tags with supported ones or remove them
        let html = html
            .replace("<p>", "")
            .replace("</p>", "\n\n")
            .replace("<h1>", "<b>")
            .replace("</h1>", "</b>\n\n")
            .replace("<h2>", "<b>")
            .replace("</h2>", "</b>\n\n")
            .replace("<h3>", "<b>")
            .replace("</h3>", "</b>\n")
            .replace("<h4>", "<b>")
            .replace("</h4>", "</b>\n")
            .replace("<h5>", "<b>")
            .replace("</h5>", "</b>\n")
            .replace("<h6>", "<b>")
            .replace("</h6>", "</b>\n")
            .replace("<strong>", "<b>")
            .replace("</strong>", "</b>")
            .replace("<em>", "<i>")
            .replace("</em>", "</i>")
            .replace("<del>", "<s>")
            .replace("</del>", "</s>")
            // remove list tags and convert to plain text with bullets
            .replace("<ul>", "")
            .replace("</ul>", "\n")
            .replace("<ol>", "")
            .replace("</ol>", "\n")
            .replace("<li>", "• ")
            .replace("</li>", "\n")
            // remove other unsupported tags
            .replace("<div>", "")
            .replace("</div>", "\n")
            .replace("<span>", "")
            .replace("</span>", "")
            .replace("<br>", "\n")
            .replace("<br/>", "\n")
            .replace("<br />", "\n")
            .replace("<hr>", "\n───────────\n")
            .replace("<hr/>", "\n───────────\n")
            .replace("<hr />", "\n───────────\n");

        // clean up excessive whitespace
        let lines: Vec<&str> = html.lines().collect();
        let mut result = Vec::new();
        let mut empty_line_count = 0;

        for line in lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                empty_line_count += 1;
                // allow max 1 consecutive empty line (single blank line between paragraphs)
                if empty_line_count <= 1 {
                    result.push("");
                }
            } else {
                empty_line_count = 0;
                result.push(trimmed);
            }
        }

        result.join("\n").trim().to_string()
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

    async fn send_payment_invoice(
        bot: Bot,
        chat_id: ChatId,
        credits: i32,
        stars: u32,
        title: &str,
        description: &str,
    ) -> ResponseResult<()> {
        let prices = vec![LabeledPrice {
            label: format!("{} credits", credits),
            amount: stars,
        }];

        bot.send_invoice(
            chat_id,
            title,
            description,
            format!("credits_{}", credits),
            "XTR",
            prices,
        )
        .provider_token("")
        .await?;

        Ok(())
    }

    async fn handle_pre_checkout_query(bot: Bot, query: PreCheckoutQuery) -> ResponseResult<()> {
        // approve all pre-checkout queries for digital goods
        // in a real implementation, you might want to add additional validation
        bot.answer_pre_checkout_query(query.id, true).await?;
        info!(
            "Approved pre-checkout query for {} stars",
            query.total_amount
        );
        Ok(())
    }

    async fn handle_successful_payment(
        bot: Bot,
        msg: Message,
        payment: SuccessfulPayment,
        user_manager: Arc<UserManager>,
    ) -> ResponseResult<()> {
        let telegram_user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);

        // parse credits from payload
        let credits = if payment.invoice_payload == "credits_1" {
            1
        } else if payment.invoice_payload == "credits_10" {
            10
        } else {
            error!("Unknown payment payload: {}", payment.invoice_payload);
            return Ok(());
        };

        // add credits to user account
        match user_manager.add_credits(telegram_user_id, credits).await {
            Ok(new_balance) => {
                let success_msg = format!(
                    "🎉 <b>Payment Successful!</b> - @ScratchAuthorEgoBot\n\n\
                    ✅ Added {} credits to your account\n\
                    💳 New balance: {} credits\n\n\
                    You can now analyze channels by sending me a channel username like <code>@channelname</code>",
                    credits, new_balance
                );

                bot.send_message(msg.chat.id, success_msg)
                    .parse_mode(ParseMode::Html)
                    .await?;

                info!(
                    "Successfully processed payment: {} credits for user {}",
                    credits, telegram_user_id
                );
            }
            Err(e) => {
                error!(
                    "Failed to add credits after payment for user {}: {}",
                    telegram_user_id, e
                );
                bot.send_message(
                    msg.chat.id,
                    "⚠️ Payment received but failed to add credits. Please contact support with your payment ID."
                )
                .await?;
            }
        }

        Ok(())
    }

    pub async fn run(&self) {
        info!("Starting Telegram bot...");

        let handler = dptree::entry()
            .branch(Update::filter_pre_checkout_query().endpoint(Self::handle_pre_checkout_query))
            .branch(Update::filter_callback_query().endpoint(Self::handle_callback_query))
            .branch(
                Update::filter_message()
                    .branch(
                        dptree::entry()
                            .filter_command::<Command>()
                            .endpoint(Self::handle_command),
                    )
                    .branch(
                        dptree::entry()
                            .filter_map(|msg: Message| {
                                msg.successful_payment()
                                    .cloned()
                                    .map(|payment| (msg, payment))
                            })
                            .endpoint(
                                |(msg, payment): (Message, SuccessfulPayment),
                                 bot: Bot,
                                 user_manager: Arc<UserManager>| {
                                    Self::handle_successful_payment(bot, msg, payment, user_manager)
                                },
                            ),
                    )
                    .branch(dptree::endpoint(Self::handle_message)),
            );

        Dispatcher::builder(self.bot.clone(), handler)
            .dependencies(dptree::deps![
                self.analysis_engine.clone(),
                self.user_manager.clone()
            ])
            .error_handler(
                teloxide::error_handlers::LoggingErrorHandler::with_custom_text(
                    "An error from the update listener",
                ),
            )
            .enable_ctrlc_handler()
            .build()
            .dispatch()
            .await;
    }

    async fn handle_callback_query(
        bot: Bot,
        query: CallbackQuery,
        analysis_engine: Arc<Mutex<AnalysisEngine>>,
        user_manager: Arc<UserManager>,
    ) -> ResponseResult<()> {
        if let Some(data) = &query.data {
            if let Some(message) = &query.message {
                match data.as_str() {
                    "buy_1" => {
                        Self::send_payment_invoice(
                            bot.clone(),
                            message.chat().id,
                            CREDITS_1_AMOUNT,
                            CREDITS_1_PRICE,
                            "1 Channel Analysis",
                            "Get 1 analysis credit to analyze any Telegram channel",
                        )
                        .await?;

                        bot.answer_callback_query(&query.id).await?;
                    }
                    "buy_10" => {
                        Self::send_payment_invoice(
                            bot.clone(),
                            message.chat().id,
                            CREDITS_10_AMOUNT,
                            CREDITS_10_PRICE,
                            "10 Channel Analyses",
                            &format!("Get 10 analysis credits to analyze any Telegram channels ({} stars discount!)",
                                (CREDITS_1_PRICE * CREDITS_10_AMOUNT as u32) - CREDITS_10_PRICE),
                        )
                        .await?;

                        bot.answer_callback_query(&query.id).await?;
                    }
                    callback_data if callback_data.starts_with("analysis_") => {
                        // parse analysis type and channel from callback data
                        let parts: Vec<&str> = callback_data.splitn(3, '_').collect();
                        if parts.len() >= 3 {
                            let analysis_type = parts[1]; // professional, personal, or roast
                            let channel_name = parts[2];

                            let telegram_user_id = query.from.id.0 as i64;

                            // start analysis in background
                            let bot_clone = bot.clone();
                            let user_chat_id = message.chat().id;
                            let channel_name_clone = channel_name.to_string();
                            let analysis_type_clone = analysis_type.to_string();
                            let analysis_engine_clone = analysis_engine.clone();
                            let user_manager_clone = user_manager.clone();

                            tokio::spawn(async move {
                                if let Err(e) = Self::perform_single_analysis(
                                    bot_clone.clone(),
                                    user_chat_id,
                                    channel_name_clone,
                                    analysis_type_clone,
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

                        bot.answer_callback_query(&query.id).await?;
                    }
                    _ => {
                        bot.answer_callback_query(&query.id).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_command(
        bot: Bot,
        msg: Message,
        cmd: Command,
        user_manager: Arc<UserManager>,
    ) -> ResponseResult<()> {
        match cmd {
            Command::Start => {
                // get user info from telegram message
                let telegram_user_id = msg.from.as_ref().map(|user| user.id.0 as i64).unwrap_or(0);
                let username = msg.from.as_ref().and_then(|user| user.username.as_deref());
                let first_name = msg.from.as_ref().map(|user| user.first_name.as_str());
                let last_name = msg.from.as_ref().and_then(|user| user.last_name.as_deref());

                // get or create user to check credit balance
                let user = match user_manager
                    .get_or_create_user(telegram_user_id, username, first_name, last_name)
                    .await
                {
                    Ok(user) => user,
                    Err(e) => {
                        log::error!("Failed to get/create user: {}", e);
                        bot.send_message(msg.chat.id, "❌ Sorry, there was an error accessing your account. Please try again later.")
                            .await?;
                        return Ok(());
                    }
                };

                if user.analysis_credits <= 0 {
                    // user has no credits - show pricing and payment options
                    let intro_text = format!(
                        "🤖 <b>@ScratchAuthorEgoBot - Channel Analyzer</b>\n\n\
                        Welcome! I can analyze Telegram channels and provide insights.\n\n\
                        📋 <b>How to use:</b>\n\
                        • Send me a channel username (e.g., <code>@channelname</code>)\n\
                        • I'll validate the channel and show analysis options\n\
                        • Choose your preferred analysis type\n\
                        • Get detailed results in seconds!\n\n\
                        ⚡ <b>Analysis Types:</b>\n\
                        • 💼 Professional: Expert assessment for hiring\n\
                        • 🧠 Personal: Psychological profile insights\n\
                        • 🔥 Roast: Fun, brutally honest critique\n\n\
                        💰 <b>Pricing:</b>\n\
                        • 1 analysis: {} ⭐ stars\n\
                        • 10 analyses: {} ⭐ stars (save {} stars!)\n\n\
                        Choose a package below or just send me a channel name to get started!",
                        CREDITS_1_PRICE,
                        CREDITS_10_PRICE,
                        (CREDITS_1_PRICE * CREDITS_10_AMOUNT as u32) - CREDITS_10_PRICE
                    );

                    bot.send_message(msg.chat.id, intro_text)
                        .parse_mode(ParseMode::Html)
                        .reply_markup(Self::create_payment_keyboard())
                        .await?;
                } else {
                    // user has credits - show welcome without pricing
                    let intro_text = format!(
                        "🤖 <b>@ScratchAuthorEgoBot - Channel Analyzer</b>\n\n\
                        Welcome back! I can analyze Telegram channels and provide insights.\n\n\
                        📋 <b>How to use:</b>\n\
                        • Send me a channel username (e.g., <code>@channelname</code>)\n\
                        • I'll validate the channel and show analysis options\n\
                        • Choose your preferred analysis type\n\
                        • Get detailed results in seconds!\n\n\
                        ⚡ <b>Analysis Types:</b>\n\
                        • 💼 Professional: Expert assessment for hiring\n\
                        • 🧠 Personal: Psychological profile insights\n\
                        • 🔥 Roast: Fun, brutally honest critique\n\n\
                        💳 <b>Your Status:</b>\n\
                        • Credits remaining: <b>{}</b>\n\
                        • Total analyses performed: <b>{}</b>\n\n\
                        Just send me a channel name to get started!",
                        user.analysis_credits, user.total_analyses_performed
                    );

                    bot.send_message(msg.chat.id, intro_text)
                        .parse_mode(ParseMode::Html)
                        .await?;
                }
            }
            Command::Buy1 => {
                Self::send_payment_invoice(
                    bot,
                    msg.chat.id,
                    CREDITS_1_AMOUNT,
                    CREDITS_1_PRICE,
                    "1 Channel Analysis",
                    "Get 1 analysis credit to analyze any Telegram channel",
                )
                .await?;
            }
            Command::Buy10 => {
                Self::send_payment_invoice(
                    bot,
                    msg.chat.id,
                    CREDITS_10_AMOUNT,
                    CREDITS_10_PRICE,
                    "10 Channel Analyses",
                    &format!("Get 10 analysis credits to analyze any Telegram channels ({} stars discount!)",
                        (CREDITS_1_PRICE * CREDITS_10_AMOUNT as u32) - CREDITS_10_PRICE),
                )
                .await?;
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
                        "❌ <b>No Analysis Credits Available</b>\n\n\
                        You have used all your free analysis credits.\n\n\
                        💰 <b>Purchase More Credits:</b>\n\
                        • 1 analysis for {} ⭐ stars\n\
                        • 10 analyses for {} ⭐ stars (save {} stars!)\n\n\
                        📊 <b>Your Stats:</b>\n\
                        • Credits remaining: <code>{}</code>\n\
                        • Total analyses performed: <code>{}</code>\n\n\
                        Choose a package below to continue analyzing channels!",
                        CREDITS_1_PRICE,
                        CREDITS_10_PRICE,
                        (CREDITS_1_PRICE * CREDITS_10_AMOUNT as u32) - CREDITS_10_PRICE,
                        user.analysis_credits,
                        user.total_analyses_performed
                    );

                    bot.send_message(msg.chat.id, no_credits_msg)
                        .parse_mode(ParseMode::Html)
                        .reply_markup(Self::create_payment_keyboard())
                        .await?;
                    return Ok(());
                }

                // send immediate response with credit info
                let credits_msg = format!(
                    "🔍 Validating channel...\n\n\
                    💳 Credits remaining after analysis: <code>{}</code>",
                    user.analysis_credits - 1
                );
                bot.send_message(msg.chat.id, credits_msg)
                    .parse_mode(ParseMode::Html)
                    .await?;

                // validate channel first
                let mut engine = analysis_engine.lock().await;
                match engine.validate_channel(text).await {
                    Ok(true) => {
                        drop(engine); // release lock

                        // show analysis type selection
                        let selection_msg = format!(
                            "✅ <b>Channel Validated!</b> by @ScratchAuthorEgoBot\n\n\
                            🎯 <b>Channel:</b> <code>{}</code>\n\n\
                            Please choose the type of analysis you'd like to perform:",
                            Self::escape_html(text)
                        );

                        bot.send_message(msg.chat.id, selection_msg)
                            .parse_mode(ParseMode::Html)
                            .reply_markup(Self::create_analysis_selection_keyboard(text))
                            .await?;
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

    async fn perform_single_analysis(
        bot: Bot,
        user_chat_id: ChatId,
        channel_name: String,
        analysis_type: String,
        analysis_engine: Arc<Mutex<AnalysisEngine>>,
        user_manager: Arc<UserManager>,
        telegram_user_id: i64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!(
            "Starting {} analysis for channel: {}",
            analysis_type, channel_name
        );

        // notify user that analysis is starting
        let analysis_emoji = match analysis_type.as_str() {
            "professional" => "💼",
            "personal" => "🧠",
            "roast" => "🔥",
            _ => "🔍",
        };

        bot.send_message(
            user_chat_id,
            format!(
                "Starting {} {} analysis... This may take a few minutes.",
                analysis_emoji, analysis_type
            ),
        )
        .await?;

        // perform analysis
        let mut engine = analysis_engine.lock().await;
        let result = engine
            .analyze_channel_with_type(&channel_name, &analysis_type)
            .await?;
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
            "✅ <b>{} Analysis Complete!</b> by @ScratchAuthorEgoBot\n\n\
            📊 Your results are ready.\n\
            💳 Credits remaining: <code>{}</code>",
            analysis_type
                .chars()
                .next()
                .unwrap()
                .to_uppercase()
                .collect::<String>()
                + &analysis_type[1..],
            remaining_credits
        );
        bot.send_message(user_chat_id, completion_msg)
            .parse_mode(ParseMode::Html)
            .await?;

        // send single analysis result to user
        Self::send_single_analysis_to_user(
            bot,
            user_chat_id,
            &channel_name,
            &analysis_type,
            result,
        )
        .await?;

        Ok(())
    }

    async fn send_single_analysis_to_user(
        bot: Bot,
        user_chat_id: ChatId,
        channel_name: &str,
        analysis_type: &str,
        result: AnalysisResult,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let header = format!(
            "📊 <b>Channel Analysis Results</b> by @ScratchAuthorEgoBot\n\n\
            🎯 <b>Channel:</b> <code>{}</code>\n\n",
            Self::escape_html(channel_name)
        );

        let (analysis_emoji, analysis_content) = match analysis_type {
            "professional" => ("💼", &result.professional),
            "personal" => ("🧠", &result.personal),
            "roast" => ("🔥", &result.roast),
            _ => ("🔍", &None),
        };

        match analysis_content {
            Some(content) if !content.is_empty() => {
                let analysis_header = format!(
                    "{} <b>{} Analysis:</b>\n\n",
                    analysis_emoji,
                    analysis_type
                        .chars()
                        .next()
                        .unwrap()
                        .to_uppercase()
                        .collect::<String>()
                        + &analysis_type[1..]
                );
                // convert LLM markdown content to HTML
                let html_content = Self::markdown_to_html_safe(content);
                let full_message = format!("{}{}{}", header, analysis_header, html_content);

                bot.send_message(user_chat_id, full_message)
                    .parse_mode(ParseMode::Html)
                    .await?;

                info!(
                    "Sent {} analysis results to user for channel: {}",
                    analysis_type, channel_name
                );
            }
            _ => {
                bot.send_message(
                    user_chat_id,
                    format!(
                        "❌ No {} analysis content was generated. Please try again.",
                        analysis_type
                    ),
                )
                .await?;
            }
        }

        Ok(())
    }
}
