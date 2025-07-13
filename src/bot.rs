use comrak::{markdown_to_html, ComrakOptions};
use html_escape;
use log::{error, info};
use regex::Regex;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{
    CallbackQuery, ChatId, InlineKeyboardButton, InlineKeyboardMarkup, LabeledPrice,
    LinkPreviewOptions, ParseMode, PreCheckoutQuery, SuccessfulPayment,
};
use teloxide::utils::command::BotCommands;
use tokio::sync::Mutex;

use crate::analysis::AnalysisEngine;
use crate::cache::AnalysisResult;
use crate::user_manager::{UserManager, UserManagerError};
use deadpool_postgres::Pool;

// payment configuration constants
const SINGLE_PACKAGE_PRICE: u32 = 40;
const BULK_PACKAGE_PRICE: u32 = 200;
const SINGLE_PACKAGE_AMOUNT: i32 = 1;
const BULK_PACKAGE_AMOUNT: i32 = 10;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Supported commands:")]
pub enum Command {
    #[command(description = "start the bot")]
    Start,
    #[command(description = "buy 1 analysis for 40 stars")]
    Buy1,
    #[command(description = "buy 10 analyses for 200 stars")]
    Buy10,
}

pub struct TelegramBot {
    bot: Bot,
    analysis_engine: Arc<Mutex<AnalysisEngine>>,
    user_manager: Arc<UserManager>,
}

impl TelegramBot {
    fn validate_and_normalize_channel(text: &str) -> Option<String> {
        // regex for valid telegram channel username (5-32 chars, alphanumeric and underscore)
        let channel_regex = Regex::new(r"^@([a-zA-Z0-9_]{5,32})$").unwrap();
        
        // regex for t.me links
        let tme_regex = Regex::new(r"^(?:https?://)?t\.me/([a-zA-Z0-9_]{5,32})$").unwrap();
        
        // check if it's already in @channel format
        if channel_regex.is_match(text) {
            return Some(text.to_string());
        }
        
        // check if it's a t.me link and extract channel name
        if let Some(captures) = tme_regex.captures(text) {
            return Some(format!("@{}", &captures[1]));
        }
        
        None
    }

    fn create_payment_keyboard() -> InlineKeyboardMarkup {
        let single_button = InlineKeyboardButton::callback(
            format!(
                "💎 Buy {} Credit ({} ⭐)",
                SINGLE_PACKAGE_AMOUNT, SINGLE_PACKAGE_PRICE
            ),
            "buy_single",
        );
        let bulk_button = InlineKeyboardButton::callback(
            format!(
                "💎 Buy {} Credits ({} ⭐)",
                BULK_PACKAGE_AMOUNT, BULK_PACKAGE_PRICE
            ),
            "buy_bulk",
        );

        InlineKeyboardMarkup::new(vec![vec![single_button], vec![bulk_button]])
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

        // get user info for referral link
        let user_db_id = match user_manager
            .get_or_create_user(telegram_user_id, None, None, None, None)
            .await
        {
            Ok((user_info, _)) => user_info.id,
            Err(e) => {
                error!("Failed to get user info during payment: {}", e);
                // continue with payment processing even if we can't get the referral link
                0
            }
        };

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
                    "🎉 <b>Payment Successful!</b> - <a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">@ScratchAuthorEgoBot</a>\n\n\
                    ✅ Added {} credits to your account\n\
                    💳 New balance: {} credits\n\n\
                    You can now analyze channels by sending me a channel username like <code>@channelname</code>",
                    user_db_id,
                    credits, new_balance
                );

                bot.send_message(msg.chat.id, success_msg)
                    .parse_mode(ParseMode::Html)
                    .await?;

                info!(
                    "Successfully processed payment: {} credits for user {}",
                    credits, telegram_user_id
                );

                // process referral rewards if user was referred
                match user_manager.record_paid_referral(telegram_user_id).await {
                    Ok(Some(reward_info)) => {
                        if let Some(referrer_telegram_id) = reward_info.referrer_telegram_id {
                            // send notification to referrer
                            let reward_msg = if reward_info.paid_rewards > 0 && reward_info.milestone_rewards > 0 {
                                format!(
                                    "🎉 <b>Referral Rewards!</b>\n\n\
                                    You've earned <b>{}</b> credits (Total referrals: <b>{}</b>):\n\
                                    • {} credit(s) for paid referral\n\
                                    • {} credit(s) for milestone bonus\n\n\
                                    Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">your referral link</a>",
                                    reward_info.total_credits_awarded,
                                    reward_info.referral_count,
                                    reward_info.paid_rewards,
                                    reward_info.milestone_rewards,
                                    reward_info.referrer_user_id.unwrap_or(0)
                                )
                            } else if reward_info.paid_rewards > 0 {
                                format!(
                                    "🎉 <b>Referral Reward!</b>\n\n\
                                    You've earned <b>{}</b> credit(s) for a paid referral! (Total referrals: <b>{}</b>)\n\n\
                                    Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">your referral link</a>",
                                    reward_info.paid_rewards,
                                    reward_info.referral_count,
                                    reward_info.referrer_user_id.unwrap_or(0)
                                )
                            } else if reward_info.milestone_rewards > 0 {
                                format!(
                                    "🎉 <b>Milestone Reward!</b>\n\n\
                                    You've earned <b>{}</b> credit(s) for reaching <b>{}</b> referrals!\n\n\
                                    Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">your referral link</a>",
                                    reward_info.milestone_rewards,
                                    reward_info.referral_count,
                                    reward_info.referrer_user_id.unwrap_or(0)
                                )
                            } else {
                                String::new()
                            };

                            if !reward_msg.is_empty() {
                                let _ = bot.send_message(
                                    ChatId(referrer_telegram_id), 
                                    reward_msg
                                )
                                .parse_mode(ParseMode::Html)
                                .await;
                            }
                        }
                    }
                    Ok(None) => {
                        // no referral rewards
                    }
                    Err(e) => {
                        error!("Failed to process paid referral for user {}: {}", telegram_user_id, e);
                    }
                }
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
                    "buy_single" => {
                        Self::send_payment_invoice(
                            bot.clone(),
                            message.chat().id,
                            SINGLE_PACKAGE_AMOUNT,
                            SINGLE_PACKAGE_PRICE,
                            "1 Channel Analysis",
                            "Get 1 analysis credit to analyze any Telegram channel",
                        )
                        .await?;

                        bot.answer_callback_query(&query.id).await?;
                    }
                    "buy_bulk" => {
                        Self::send_payment_invoice(
                            bot.clone(),
                            message.chat().id,
                            BULK_PACKAGE_AMOUNT,
                            BULK_PACKAGE_PRICE,
                            "10 Channel Analyses",
                            &format!("Get 10 analysis credits to analyze any Telegram channels ({} stars discount!)",
                                (SINGLE_PACKAGE_PRICE * BULK_PACKAGE_AMOUNT as u32) - BULK_PACKAGE_PRICE),
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

                            // check if user has credits before starting analysis
                            let user = match user_manager
                                .get_or_create_user(
                                    telegram_user_id,
                                    query.from.username.as_deref(),
                                    Some(query.from.first_name.as_str()),
                                    query.from.last_name.as_deref(),
                                    None, // no referral in callback queries
                                )
                                .await
                            {
                                Ok((user, _)) => user,
                                Err(e) => {
                                    error!("Failed to get user: {}", e);
                                    bot.send_message(
                                        message.chat().id,
                                        "❌ Failed to check credits. Please try again.",
                                    )
                                    .await?;
                                    return Ok(());
                                }
                            };

                            if user.analysis_credits <= 0 {
                                // no credits available, send payment options
                                let message_text = "❌ No analysis credits available.\n\n\
                                    You need credits to analyze channels. Choose a package below:";

                                bot.send_message(message.chat().id, message_text)
                                    .reply_markup(Self::create_payment_keyboard())
                                    .await?;

                                bot.answer_callback_query(&query.id).await?;
                                return Ok(());
                            }

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
                                    channel_name_clone.clone(),
                                    analysis_type_clone.clone(),
                                    analysis_engine_clone,
                                    user_manager_clone,
                                    telegram_user_id,
                                )
                                .await
                                {
                                    if let Some(user_error) =
                                        e.downcast_ref::<crate::user_manager::UserManagerError>()
                                    {
                                        match user_error {
                                            crate::user_manager::UserManagerError::InsufficientCredits(user_id) => {
                                                info!("Analysis failed: User {} has insufficient credits", user_id);
                                                let _ = bot_clone
                                                    .send_message(
                                                        user_chat_id,
                                                        "❌ Insufficient credits. Please purchase more credits to continue.",
                                                    )
                                                    .await;
                                            }
                                            _ => {
                                                error!("Analysis failed for channel {} (type: {}): {}", channel_name_clone, analysis_type_clone, e);
                                                error!("User manager error during analysis: {}", user_error);
                                                let _ = bot_clone
                                                    .send_message(
                                                        user_chat_id,
                                                        "❌ Analysis failed due to a system error. Please try again later.",
                                                    )
                                                    .await;
                                            }
                                        }
                                    } else {
                                        // Log the full error details
                                        error!("Analysis failed for channel {} (type: {}): {}", channel_name_clone, analysis_type_clone, e);
                                        error!("Non-user error during analysis: {}", e);
                                        // Don't send generic error - it's already handled in perform_single_analysis
                                    }
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
                // parse referral code from message text
                let referrer_user_id = if let Some(text) = msg.text() {
                    info!("Processing /start command with text: {}", text);
                    if let Some(args) = text.strip_prefix("/start ") {
                        info!("Found referral code in /start command: {}", args);
                        if let Ok(user_id) = args.trim().parse::<i32>() {
                            info!("Parsed referrer user ID: {}", user_id);
                            // validate that referrer exists
                            match user_manager.validate_referrer(user_id).await {
                                Ok(true) => {
                                    info!("Referrer user ID {} validated successfully", user_id);
                                    Some(user_id)
                                }
                                Ok(false) => {
                                    info!("Referrer user ID {} does not exist", user_id);
                                    None
                                }
                                Err(e) => {
                                    error!("Failed to validate referrer user ID {}: {}", user_id, e);
                                    None
                                }
                            }
                        } else {
                            info!("Failed to parse referrer ID from args: {}", args);
                            None
                        }
                    } else {
                        info!("No referral code found in /start command");
                        None
                    }
                } else {
                    info!("No text found in /start message");
                    None
                };

                // get user info from telegram message
                let telegram_user_id = msg.from.as_ref().map(|user| user.id.0 as i64).unwrap_or(0);
                let username = msg.from.as_ref().and_then(|user| user.username.as_deref());
                let first_name = msg.from.as_ref().map(|user| user.first_name.as_str());
                let last_name = msg.from.as_ref().and_then(|user| user.last_name.as_deref());

                // get or create user to check credit balance
                let (user, maybe_reward_info) = match user_manager
                    .get_or_create_user(telegram_user_id, username, first_name, last_name, referrer_user_id)
                    .await
                {
                    Ok((user, reward_info)) => (user, reward_info),
                    Err(e) => {
                        log::error!("Failed to get/create user: {}", e);
                        bot.send_message(msg.chat.id, "❌ Sorry, there was an error accessing your account. Please try again later.")
                            .await?;
                        return Ok(());
                    }
                };

                // send referral milestone notification if applicable
                if let Some(reward_info) = maybe_reward_info {
                    info!("Received reward info for referral: referral_count={}, milestone_rewards={}, paid_rewards={}, is_celebration={}, referrer_telegram_id={:?}", 
                          reward_info.referral_count, reward_info.milestone_rewards, reward_info.paid_rewards, 
                          reward_info.is_celebration_milestone, reward_info.referrer_telegram_id);
                    if let Some(referrer_telegram_id) = reward_info.referrer_telegram_id {
                        let reward_msg = if reward_info.is_celebration_milestone && reward_info.total_credits_awarded > 0 {
                            format!(
                                "🎉 <b>Referral Milestone!</b>\n\n\
                                Congratulations! You've reached <b>{}</b> referrals and earned <b>{}</b> credit(s)!\n\n\
                                Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">your referral link</a>",
                                reward_info.referral_count,
                                reward_info.total_credits_awarded,
                                reward_info.referrer_user_id.unwrap_or(0)
                            )
                        } else if reward_info.is_celebration_milestone {
                            format!(
                                "🎊 <b>Referral Milestone!</b>\n\n\
                                Congratulations! You've reached <b>{}</b> referrals!\n\n\
                                Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">your referral link</a>",
                                reward_info.referral_count,
                                reward_info.referrer_user_id.unwrap_or(0)
                            )
                        } else if reward_info.total_credits_awarded > 0 {
                            format!(
                                "🎉 <b>Referral Reward!</b>\n\n\
                                You've earned <b>{}</b> credit(s) for reaching <b>{}</b> referrals!\n\n\
                                Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">your referral link</a>",
                                reward_info.total_credits_awarded,
                                reward_info.referral_count,
                                reward_info.referrer_user_id.unwrap_or(0)
                            )
                        } else {
                            String::new()
                        };

                        if !reward_msg.is_empty() {
                            info!("Sending referral notification to telegram user {}: {}", referrer_telegram_id, reward_msg.replace("\n", " "));
                            match bot.send_message(
                                ChatId(referrer_telegram_id), 
                                reward_msg
                            )
                            .parse_mode(ParseMode::Html)
                            .await {
                                Ok(_) => info!("Successfully sent referral notification to telegram user {}", referrer_telegram_id),
                                Err(e) => error!("Failed to send referral notification to telegram user {}: {}", referrer_telegram_id, e)
                            }
                        } else {
                            info!("No reward message to send (empty message generated)");
                        }
                    } else {
                        error!("Reward info received but no referrer_telegram_id found");
                    }
                } else {
                    info!("No reward info received for user creation");
                }

                if user.analysis_credits <= 0 {
                    // user has no credits - show pricing and payment options
                    let referral_info = if user.referrals_count > 0 {
                        format!("You have {} referrals! 🎉", user.referrals_count)
                    } else {
                        "Start earning free credits by referring friends!".to_string()
                    };

                    let intro_text = format!(
                        "🤖 <b><a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">@ScratchAuthorEgoBot</a> - Channel Analyzer</b>\n\n\
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
                        🎁 <b>Referral Program:</b> {}\n\
                        Share your link: <code>https://t.me/ScratchAuthorEgoBot?start={}</code>\n\
                        • Get credits at milestones: 1, 5, 10, 20, 30...\n\
                        • Get 1 credit for each paid referral\n\n\
                        Choose a package below or just send me a channel name to get started!",
                        user.id,  // for the bot name referral link
                        SINGLE_PACKAGE_PRICE,
                        BULK_PACKAGE_PRICE,
                        (SINGLE_PACKAGE_PRICE * BULK_PACKAGE_AMOUNT as u32) - BULK_PACKAGE_PRICE,
                        referral_info,
                        user.id  // for the share your link
                    );

                    bot.send_message(msg.chat.id, intro_text)
                        .parse_mode(ParseMode::Html)
                        .reply_markup(Self::create_payment_keyboard())
                        .await?;
                } else {
                    // user has credits - show welcome without pricing
                    let referral_section = if user.referrals_count > 0 {
                        let next_milestone = if user.referrals_count < 1 {
                            1
                        } else if user.referrals_count < 5 {
                            5
                        } else if user.referrals_count < 10 {
                            10
                        } else {
                            ((user.referrals_count / 10) + 1) * 10
                        };
                        let referrals_to_next = next_milestone - user.referrals_count;
                        format!(
                            "💳 <b>Your Status:</b>\n\
                            • Credits remaining: <b>{}</b>\n\
                            • Total analyses performed: <b>{}</b>\n\
                            • Referrals: <b>{}</b> (Paid: <b>{}</b>)\n\
                            • Next milestone reward in <b>{}</b> referrals\n\n\
                            🎁 <b>Referral Program:</b>\n\
                            Share your link: <code>https://t.me/ScratchAuthorEgoBot?start={}</code>\n\
                            • Get credits at milestones: 1, 5, 10, 20, 30...\n\
                            • Get 1 credit for each paid referral\n\n\
                            Great job on your {} referrals! 🎉",
                            user.analysis_credits, user.total_analyses_performed, user.referrals_count, user.paid_referrals_count, referrals_to_next, user.id, user.referrals_count
                        )
                    } else {
                        format!(
                            "💳 <b>Your Status:</b>\n\
                            • Credits remaining: <b>{}</b>\n\
                            • Total analyses performed: <b>{}</b>\n\n\
                            🎁 <b>Referral Program:</b>\n\
                            Share your link: <code>https://t.me/ScratchAuthorEgoBot?start={}</code>\n\
                            • Get credits at milestones: 1, 5, 10, 20, 30...\n\
                            • Get 1 credit for each paid referral",
                            user.analysis_credits, user.total_analyses_performed, user.id
                        )
                    };

                    let intro_text = format!(
                        "🤖 <b><a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">@ScratchAuthorEgoBot</a> - Channel Analyzer</b>\n\n\
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
                        {}\n\n\
                        Just send me a channel name to get started!",
                        user.id,
                        referral_section
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
                    SINGLE_PACKAGE_AMOUNT,
                    SINGLE_PACKAGE_PRICE,
                    "1 Channel Analysis",
                    "Get 1 analysis credit to analyze any Telegram channel",
                )
                .await?;
            }
            Command::Buy10 => {
                Self::send_payment_invoice(
                    bot,
                    msg.chat.id,
                    BULK_PACKAGE_AMOUNT,
                    BULK_PACKAGE_PRICE,
                    "10 Channel Analyses",
                    &format!("Get 10 analysis credits to analyze any Telegram channels ({} stars discount!)",
                        (SINGLE_PACKAGE_PRICE * BULK_PACKAGE_AMOUNT as u32) - BULK_PACKAGE_PRICE),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn handle_message(
        bot: Bot,
        msg: Message,
        user_manager: Arc<UserManager>,
    ) -> ResponseResult<()> {
        if let Some(text) = msg.text() {
            let text = text.trim();

            // validate and normalize channel input
            if let Some(channel_name) = Self::validate_and_normalize_channel(text) {
                info!("Received channel analysis request: {}", channel_name);

                // get user info from telegram message
                let telegram_user_id = msg.from.as_ref().map(|user| user.id.0 as i64).unwrap_or(0);
                let username = msg.from.as_ref().and_then(|user| user.username.as_deref());
                let first_name = msg.from.as_ref().map(|user| user.first_name.as_str());
                let last_name = msg.from.as_ref().and_then(|user| user.last_name.as_deref());

                // get or create user and check credits
                let user = match user_manager
                    .get_or_create_user(telegram_user_id, username, first_name, last_name, None)
                    .await
                {
                    Ok((user, _)) => user,
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
                        SINGLE_PACKAGE_PRICE,
                        BULK_PACKAGE_PRICE,
                        (SINGLE_PACKAGE_PRICE * BULK_PACKAGE_AMOUNT as u32) - BULK_PACKAGE_PRICE,
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
                    "🔍 Starting analysis...\n\n\
                    💳 Credits remaining after analysis: <code>{}</code>",
                    user.analysis_credits - 1
                );
                bot.send_message(msg.chat.id, credits_msg)
                    .parse_mode(ParseMode::Html)
                    .await?;

                // show analysis type selection directly (validation will happen during analysis)
                let selection_msg = format!(
                    "🎯 <b>Channel:</b> <code>{}</code>\n\n\
                    Please choose the type of analysis you'd like to perform:",
                    Self::escape_html(&channel_name)
                );

                bot.send_message(msg.chat.id, selection_msg)
                    .parse_mode(ParseMode::Html)
                    .reply_markup(Self::create_analysis_selection_keyboard(&channel_name))
                    .await?;
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

        // timeout notice for long-running requests
        bot.send_message(
            user_chat_id,
            "⏱️ <b>Timeout Notice</b>\n\n\
            If you don't receive a response after 60 minutes, the request may have been lost.\n\
            In that case, please try again - no credits will be consumed for failed requests.",
        )
        .parse_mode(ParseMode::Html)
        .await?;

        // check if we'll hit rate limits before starting (with lock)
        let will_hit_rate_limits = {
            let engine = analysis_engine.lock().await;
            let cached = engine
                .cache
                .load_channel_messages(&channel_name)
                .await
                .is_some();
            if !cached {
                engine.check_rate_limits().await
            } else {
                false
            }
        };

        // notify user about high load BEFORE starting analysis
        if will_hit_rate_limits {
            let high_load_msg = "⚠️ <b>High Load Notice</b>\n\n\
                This may take longer than usual.\n\n\
                🔧 <b>For better performance:</b>\n\
                Consider running your own instance with your API keys from the 🔗 <a href=\"https://github.com/arsenyinfo/tg-channel-analyzer/\">GitHub Repository</a>";
            bot.send_message(user_chat_id, high_load_msg)
                .parse_mode(ParseMode::Html)
                .link_preview_options(LinkPreviewOptions {
                    is_disabled: true,
                    url: None,
                    prefer_small_media: false,
                    prefer_large_media: false,
                    show_above_text: false,
                })
                .await?;
        }

        // prepare analysis data (with lock)
        let analysis_data = {
            let mut engine = analysis_engine.lock().await;
            match engine.prepare_analysis_data(&channel_name).await {
                Ok(data) => data,
                Err(e) => {
                    error!("Failed to prepare analysis data for channel {}: {}", channel_name, e);
                    bot.send_message(
                        user_chat_id,
                        format!("❌ <b>Analysis Error</b>\n\nFailed to prepare analysis for channel {}. This could happen if:\n• The channel is private/restricted\n• The channel doesn't exist\n• There are network connectivity issues\n\nNo credits were consumed for this request.", channel_name),
                    )
                    .parse_mode(ParseMode::Html)
                    .await?;
                    return Err(e);
                }
            }
        };

        // check if we received 0 messages and raise error
        if analysis_data.messages.is_empty() {
            bot.send_message(
                user_chat_id,
                "❌ <b>Analysis Error</b>\n\nNo messages found in the channel. This could happen if:\n• The channel is private/restricted\n• The channel has no recent messages\n• There are network connectivity issues\n\nNo credits were consumed for this request.",
            )
            .parse_mode(ParseMode::Html)
            .await?;
            return Err("No messages found in channel".into());
        }

        // check for cached result (with lock)
        let cached_result = {
            let engine = analysis_engine.lock().await;
            engine.cache.load_llm_result(&analysis_data.cache_key).await
        };

        let result = if let Some(cached_result) = cached_result {
            cached_result
        } else {
            // generate prompt without lock
            let prompt = match crate::analysis::AnalysisEngine::generate_analysis_prompt(&analysis_data.messages) {
                Ok(p) => p,
                Err(e) => {
                    error!("Failed to generate analysis prompt for channel {}: {}", channel_name, e);
                    bot.send_message(
                        user_chat_id,
                        "❌ <b>Analysis Error</b>\n\nFailed to generate analysis prompt. No credits were consumed.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await?;
                    return Err(e);
                }
            };

            info!("Querying LLM for {} analysis of channel {}...", analysis_type, channel_name);
            // perform LLM call WITHOUT holding the lock
            let mut result = match crate::analysis::AnalysisEngine::query_and_parse_analysis(&prompt).await {
                Ok(r) => r,
                Err(e) => {
                    error!("Failed to query LLM for {} analysis of channel {}: {}", analysis_type, channel_name, e);
                    bot.send_message(
                        user_chat_id,
                        "❌ <b>Analysis Error</b>\n\nFailed to complete analysis due to AI service issues. Please try again later.\n\nNo credits were consumed for this request.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await?;
                    return Err(e);
                }
            };
            result.messages_count = analysis_data.messages.len();

            // finish analysis (cache result) with lock
            {
                let mut engine = analysis_engine.lock().await;
                if let Err(e) = engine.finish_analysis(&analysis_data.cache_key, result.clone()).await {
                    error!("Failed to cache analysis result for channel {}: {}", channel_name, e);
                    // Continue execution - caching failure shouldn't stop the analysis
                }
            }

            result
        };

        // get user info for referral link
        let (user_info, _) = user_manager
            .get_or_create_user(telegram_user_id, None, None, None, None)
            .await?;
        let user_db_id = user_info.id;

        // consume credit after successful analysis
        let remaining_credits = match user_manager
            .consume_credit(telegram_user_id, &channel_name, &analysis_type)
            .await
        {
            Ok(credits) => credits,
            Err(e) => {
                match &e {
                    UserManagerError::InsufficientCredits(user_id) => {
                        info!(
                            "User {} has insufficient credits for channel {}",
                            user_id, channel_name
                        );
                    }
                    UserManagerError::UserNotFound(user_id) => {
                        error!("User {} not found during credit consumption", user_id);
                    }
                    UserManagerError::DatabaseError(db_err) => {
                        error!(
                            "Database error while consuming credit for user {}: {}",
                            telegram_user_id, db_err
                        );
                    }
                }
                bot.send_message(
                    user_chat_id,
                    "⚠️ Analysis completed but failed to update credits. Please contact support.",
                )
                .await?;
                return Err(Box::new(e));
            }
        };

        // notify user that analysis is complete and send results with credit info
        let completion_msg = format!(
            "✅ <b>{} Analysis Complete!</b> by <a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">@ScratchAuthorEgoBot</a>\n\n\
            📊 Your results are ready.\n\
            💳 Credits remaining: <code>{}</code>",
            analysis_type
                .chars()
                .next()
                .unwrap()
                .to_uppercase()
                .collect::<String>()
                + &analysis_type[1..],
            user_db_id,
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
            user_db_id,
        )
        .await?;

        Ok(())
    }

    /// splits a message into chunks that fit within Telegram's 4096 character limit
    fn split_message_into_chunks(text: &str, max_length: usize) -> Vec<String> {
        if text.len() <= max_length {
            return vec![text.to_string()];
        }

        let mut chunks = Vec::new();
        let mut current_chunk = String::new();
        
        // split by lines to avoid breaking in the middle of formatting
        for line in text.lines() {
            let line_with_newline = format!("{}\n", line);
            
            // if adding this line would exceed the limit, finalize current chunk
            if current_chunk.len() + line_with_newline.len() > max_length {
                if !current_chunk.is_empty() {
                    chunks.push(current_chunk.trim_end().to_string());
                    current_chunk.clear();
                }
                
                // if single line is too long, split it at word boundaries
                if line_with_newline.len() > max_length {
                    let words: Vec<&str> = line.split_whitespace().collect();
                    let mut word_chunk = String::new();
                    
                    for word in words {
                        let word_with_space = format!("{} ", word);
                        if word_chunk.len() + word_with_space.len() > max_length {
                            if !word_chunk.is_empty() {
                                chunks.push(word_chunk.trim_end().to_string());
                                word_chunk.clear();
                            }
                        }
                        word_chunk.push_str(&word_with_space);
                    }
                    
                    if !word_chunk.is_empty() {
                        current_chunk = word_chunk.trim_end().to_string();
                    }
                } else {
                    current_chunk.push_str(&line_with_newline);
                }
            } else {
                current_chunk.push_str(&line_with_newline);
            }
        }
        
        if !current_chunk.is_empty() {
            chunks.push(current_chunk.trim_end().to_string());
        }
        
        chunks
    }

    async fn send_single_analysis_to_user(
        bot: Bot,
        user_chat_id: ChatId,
        channel_name: &str,
        analysis_type: &str,
        result: AnalysisResult,
        user_db_id: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (analysis_emoji, analysis_content) = match analysis_type {
            "professional" => ("💼", &result.professional),
            "personal" => ("🧠", &result.personal),
            "roast" => ("🔥", &result.roast),
            _ => ("🔍", &None),
        };

        match analysis_content {
            Some(content) if !content.is_empty() => {
                // convert LLM markdown content to HTML first
                let html_content = Self::markdown_to_html_safe(content);
                
                // prepare header template that will be added to each part
                let header = format!(
                    "📊 <b>Channel Analysis Results</b> by <a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">@ScratchAuthorEgoBot</a>\n\n\
                    🎯 <b>Channel:</b> <code>{}</code>\n\n",
                    user_db_id,
                    Self::escape_html(channel_name)
                );

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

                // calculate available space for content after headers
                const MAX_MESSAGE_LENGTH: usize = 3072;
                let headers_length = header.len() + analysis_header.len();
                let available_content_length = MAX_MESSAGE_LENGTH.saturating_sub(headers_length + 50); // extra buffer for part indicators

                // split content if needed
                let content_chunks = Self::split_message_into_chunks(&html_content, available_content_length);
                
                for (i, chunk) in content_chunks.iter().enumerate() {
                    let full_message = if content_chunks.len() > 1 {
                        format!("{}{}{}\n\n<i>📄 Part {} of {}</i>", header, analysis_header, chunk, i + 1, content_chunks.len())
                    } else {
                        format!("{}{}{}", header, analysis_header, chunk)
                    };

                    bot.send_message(user_chat_id, full_message)
                        .parse_mode(ParseMode::Html)
                        .await?;
                }

                info!(
                    "Sent {} analysis results to user for channel: {} ({} parts)",
                    analysis_type, channel_name, content_chunks.len()
                );
            }
            _ => {
                error!("No {} analysis content available for channel: {} (user: {})", 
                       analysis_type, channel_name, user_chat_id);
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
