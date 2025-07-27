use log::{error, info, warn};
use regex::Regex;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{
    CallbackQuery, ChatId, InlineKeyboardButton, InlineKeyboardMarkup,
    ParseMode, PreCheckoutQuery, SuccessfulPayment,
};
use teloxide::utils::command::BotCommands;
use tokio::sync::Mutex;

use crate::analysis::AnalysisEngine;
use crate::cache::AnalysisResult;
use crate::handlers::{PaymentHandler, CallbackHandler, CommandHandler, GroupHandler, payment_handler::{SINGLE_PACKAGE_PRICE, BULK_PACKAGE_PRICE, BULK_PACKAGE_AMOUNT}};
use crate::user_manager::UserManager;
use crate::user_session::{SessionManager, SessionState};
use crate::utils::MessageFormatter;
use deadpool_postgres::Pool;

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
    bot: Arc<Bot>,
    analysis_engine: Arc<Mutex<AnalysisEngine>>,
    user_manager: Arc<UserManager>,
    pool: Arc<Pool>,
    payment_handler: PaymentHandler,
    group_handler: GroupHandler,
    session_manager: Arc<SessionManager>,
}

#[derive(Clone)]
pub struct BotContext {
    pub bot: Arc<Bot>,
    pub analysis_engine: Arc<Mutex<AnalysisEngine>>,
    pub user_manager: Arc<UserManager>,
    pub payment_handler: PaymentHandler,
    pub group_handler: GroupHandler,
    pub session_manager: Arc<SessionManager>,
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



    async fn run_message_queue_processor(bot: Arc<Bot>, pool: Arc<Pool>) {
        info!("Starting message queue processor");
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(2));
        
        loop {
            interval.tick().await;
            
            let client = match pool.get().await {
                Ok(client) => client,
                Err(e) => {
                    error!("Failed to get database connection for queue processor: {}", e);
                    continue;
                }
            };
            
            // get next pending message
            let row = match client.query_opt(
                "SELECT id, telegram_user_id, message, parse_mode 
                 FROM message_queue 
                 WHERE status = 'pending' 
                 ORDER BY created_at 
                 LIMIT 1 
                 FOR UPDATE SKIP LOCKED",
                &[],
            ).await {
                Ok(row) => row,
                Err(e) => {
                    error!("Failed to query message queue: {}", e);
                    continue;
                }
            };
            
            if let Some(row) = row {
                let id: i32 = row.get(0);
                let user_id: i64 = row.get(1);
                let message: String = row.get(2);
                let parse_mode: String = row.get(3);
                
                // send message
                let send_result = if parse_mode.to_uppercase() == "HTML" {
                    bot.send_message(ChatId(user_id), &message)
                        .parse_mode(ParseMode::Html)
                        .await
                } else {
                    bot.send_message(ChatId(user_id), &message)
                        .parse_mode(ParseMode::MarkdownV2)
                        .await
                };
                
                match send_result {
                    Ok(_) => {
                        if let Err(e) = client.execute(
                            "UPDATE message_queue SET status = 'sent', sent_at = NOW() WHERE id = $1",
                            &[&id],
                        ).await {
                            error!("Failed to update message status to sent: {}", e);
                        }
                    }
                    Err(e) => {
                        let error_msg = e.to_string();
                        if let Err(e) = client.execute(
                            "UPDATE message_queue SET status = 'failed', error_message = $2 WHERE id = $1",
                            &[&id, &error_msg],
                        ).await {
                            error!("Failed to update message status to failed: {}", e);
                        }
                    }
                }
            }
        }
    }


    pub async fn new(
        bot_token: &str,
        user_manager: Arc<UserManager>,
        pool: Arc<Pool>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let bot = Arc::new(Bot::new(bot_token));
        let analysis_engine = Arc::new(Mutex::new(AnalysisEngine::new(pool.clone())?));
        let payment_handler = PaymentHandler::new(user_manager.clone());
        let group_handler = GroupHandler::new(pool.clone());
        let session_manager = Arc::new(SessionManager::new());

        Ok(Self {
            bot,
            analysis_engine,
            user_manager,
            pool,
            payment_handler,
            group_handler,
            session_manager,
        })
    }




    pub async fn run(&self) {
        info!("Starting Telegram bot...");

        // get and log bot info at startup
        info!("Fetching bot info...");
        match self.bot.get_me().await {
            Ok(me) => {
                info!("Bot started successfully!");
                info!("Bot ID: {}", me.id);
                info!("Bot username: @{}", me.username.as_deref().unwrap_or("NO_USERNAME"));
                info!("Bot name: {}", me.first_name);
                if me.can_join_groups {
                    info!("Bot CAN join groups");
                } else {
                    warn!("Bot CANNOT join groups - this will prevent group analysis!");
                }
                if me.can_read_all_group_messages {
                    info!("Bot CAN read all group messages");
                } else {
                    warn!("Bot CANNOT read all group messages - group analysis may not work properly!");
                }
            }
            Err(e) => {
                error!("Failed to get bot info at startup: {}", e);
            }
        }

        // spawn message queue processor
        let bot_clone = self.bot.clone();
        let pool_clone = self.pool.clone();
        tokio::spawn(async move {
            Self::run_message_queue_processor(bot_clone, pool_clone).await;
        });

        // create context for all handlers
        let ctx = BotContext {
            bot: self.bot.clone(),
            analysis_engine: self.analysis_engine.clone(),
            user_manager: self.user_manager.clone(),
            payment_handler: self.payment_handler.clone(),
            group_handler: self.group_handler.clone(),
            session_manager: self.session_manager.clone(),
        };
        
        // create group handler with clone for the handler tree
        let group_handler = self.group_handler.clone();

        let handler = dptree::entry()
            .branch(Update::filter_pre_checkout_query().endpoint({
                let ctx = ctx.clone();
                move |query: PreCheckoutQuery| {
                    let ctx = ctx.clone();
                    async move { PaymentHandler::handle_pre_checkout_query(ctx.bot, query).await }
                }
            }))
            .branch(Update::filter_callback_query().endpoint({
                let ctx = ctx.clone();
                move |query: CallbackQuery| {
                    let ctx = ctx.clone();
                    async move { CallbackHandler::handle_callback_query(ctx, query).await }
                }
            }))
            .branch(
                Update::filter_message()
                    .branch(
                        dptree::entry()
                            .filter_command::<Command>()
                            .endpoint({
                                let ctx = ctx.clone();
                                move |msg: Message, cmd: Command| {
                                    let ctx = ctx.clone();
                                    async move { CommandHandler::handle_command(ctx, msg, cmd).await }
                                }
                            }),
                    )
                    .branch(
                        dptree::entry()
                            .filter_map(|msg: Message| {
                                msg.successful_payment()
                                    .cloned()
                                    .map(|payment| (msg, payment))
                            })
                            .endpoint({
                                let ctx = ctx.clone();
                                move |(msg, payment): (Message, SuccessfulPayment)| {
                                    let ctx = ctx.clone();
                                    async move { ctx.payment_handler.handle_successful_payment(ctx.bot, msg, payment).await }
                                }
                            }),
                    )
                    .branch(dptree::endpoint({
                        let ctx = ctx.clone();
                        let group_handler = group_handler.clone();
                        move |msg: Message| {
                            let ctx = ctx.clone();
                            let group_handler = group_handler.clone();
                            async move { Self::handle_message(ctx, msg, group_handler).await }
                        }
                    })),
            );

        Dispatcher::builder(self.bot.clone(), handler)
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



    async fn handle_message(
        ctx: BotContext,
        msg: Message,
        group_handler: GroupHandler,
    ) -> ResponseResult<()> {
        
        // check if message is from a group chat
        if msg.chat.is_group() || msg.chat.is_supergroup() {
            // handle group messages
            return group_handler.handle_group_message(ctx, msg).await;
        }
        
        if let Some(text) = msg.text() {
            let text = text.trim();

            // get user ID for session management
            let user_id = msg.from.as_ref().map(|user| user.id.0 as i64).unwrap_or(0);
            
            // check user session state
            let session_state = ctx.session_manager.get_session(user_id).await;
            
            match session_state {
                SessionState::ChannelAnalysisAwaitingInput => {
                    // user is in channel analysis mode, validate input
                    if let Some(channel_name) = Self::validate_and_normalize_channel(text) {
                        // set session to selecting analysis type
                        ctx.session_manager.set_session(
                            user_id, 
                            SessionState::ChannelAnalysisSelectingType { channel_name: channel_name.clone() }
                        ).await;
                        
                        // send analysis type selection
                        let selection_msg = format!(
                            "🎯 <b>Channel:</b> <code>{}</code>\n\n\
                            Please choose the type of analysis you'd like to perform:",
                            MessageFormatter::escape_html(&channel_name)
                        );

                        ctx.bot.send_message(msg.chat.id, selection_msg)
                            .parse_mode(ParseMode::Html)
                            .reply_markup(Self::create_channel_analysis_selection_keyboard(&channel_name))
                            .await?;
                    } else {
                        // invalid channel input
                        ctx.bot.send_message(
                            msg.chat.id,
                            "❌ Please send a valid channel username starting with '@' (e.g., @channelname) or a t.me link.\n\nUse /start to return to the main menu.",
                        ).await?;
                    }
                    return Ok(());
                }
                SessionState::Idle => {
                    // fallback for backward compatibility - handle as normal channel input
                    if let Some(channel_name) = Self::validate_and_normalize_channel(text) {
                        Self::handle_legacy_channel_input(ctx, msg, channel_name).await?;
                        return Ok(());
                    } else {
                        // send help message for invalid input
                        ctx.bot.send_message(
                            msg.chat.id,
                            "❓ Please use the menu buttons or send a valid channel username starting with '@' (e.g., @channelname).\n\nUse /start to see the main menu.",
                        ).await?;
                        return Ok(());
                    }
                }
                _ => {
                    // user is in some other state, ignore text input or reset to idle
                    ctx.session_manager.clear_session(user_id).await;
                    ctx.bot.send_message(
                        msg.chat.id,
                        "Session reset. Please use /start to return to the main menu.",
                    ).await?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn create_channel_analysis_selection_keyboard(channel_name: &str) -> InlineKeyboardMarkup {
        let professional_button = InlineKeyboardButton::callback(
            "💼 Professional Analysis",
            format!("channel_analysis_professional_{}", channel_name),
        );
        let personal_button = InlineKeyboardButton::callback(
            "🧠 Personal Analysis", 
            format!("channel_analysis_personal_{}", channel_name),
        );
        let roast_button = InlineKeyboardButton::callback(
            "🔥 Roast Analysis",
            format!("channel_analysis_roast_{}", channel_name),
        );

        InlineKeyboardMarkup::new(vec![
            vec![professional_button],
            vec![personal_button],
            vec![roast_button],
        ])
    }

    // legacy handler for backward compatibility
    async fn handle_legacy_channel_input(
        ctx: BotContext,
        msg: Message,
        channel_name: String,
    ) -> ResponseResult<()> {
        info!("Received channel analysis request: {}", channel_name);

        // get user info from telegram message
        let telegram_user_id = msg.from.as_ref().map(|user| user.id.0 as i64).unwrap_or(0);
        let username = msg.from.as_ref().and_then(|user| user.username.as_deref());
        let first_name = msg.from.as_ref().map(|user| user.first_name.as_str());
        let last_name = msg.from.as_ref().and_then(|user| user.last_name.as_deref());
        let language_code = msg.from.as_ref().and_then(|user| user.language_code.as_deref());

        // get or create user and check credits
        let user = match ctx.user_manager
            .get_or_create_user(telegram_user_id, username, first_name, last_name, None, language_code)
            .await
        {
            Ok((user, _)) => user,
            Err(e) => {
                error!("Failed to get/create user: {}", e);
                ctx.bot.send_message(
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

            ctx.bot.send_message(msg.chat.id, no_credits_msg)
                .parse_mode(ParseMode::Html)
                .reply_markup(CallbackHandler::create_payment_keyboard())
                .await?;
            return Ok(());
        }

        // send immediate response with credit info
        let credits_msg = format!(
            "🔍 Starting analysis...\n\n\
            💳 Credits remaining after analysis: <code>{}</code>",
            user.analysis_credits - 1
        );
        ctx.bot.send_message(msg.chat.id, credits_msg)
            .parse_mode(ParseMode::Html)
            .await?;

        // show analysis type selection directly (validation will happen during analysis)
        let selection_msg = format!(
            "🎯 <b>Channel:</b> <code>{}</code>\n\n\
            Please choose the type of analysis you'd like to perform:",
            MessageFormatter::escape_html(&channel_name)
        );

        ctx.bot.send_message(msg.chat.id, selection_msg)
            .parse_mode(ParseMode::Html)
            .reply_markup(CallbackHandler::create_analysis_selection_keyboard(&channel_name))
            .await?;
        
        Ok(())
    }

    pub async fn perform_single_analysis(
        bot: Arc<Bot>,
        user_chat_id: ChatId,
        channel_name: String,
        analysis_type: String,
        analysis_engine: Arc<Mutex<AnalysisEngine>>,
        user_manager: Arc<UserManager>,
        user_id: i32,
        analysis_id: i32,
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
            let prompt = match crate::prompts::analysis::generate_analysis_prompt(&analysis_data.messages) {
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
            let mut result = match crate::llm::analysis_query::query_and_parse_analysis(&prompt).await {
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
        // ATOMIC OPERATION: consume credit + mark completed + send result (protected from shutdown)
        let remaining_credits = match user_manager
            .atomic_complete_analysis(analysis_id, user_id)
            .await
        {
            Ok(credits) => credits,
            Err(e) => {
                error!("Failed to atomically complete analysis {}: {}", analysis_id, e);
                // mark as failed if atomic completion failed
                if let Err(mark_err) = user_manager.mark_analysis_failed(analysis_id).await {
                    error!("Failed to mark analysis {} as failed: {}", analysis_id, mark_err);
                }
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
            user_id,
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
            user_id,
        )
        .await?;

        Ok(())
    }



    async fn send_single_analysis_to_user(
        bot: Arc<Bot>,
        user_chat_id: ChatId,
        channel_name: &str,
        analysis_type: &str,
        result: AnalysisResult,
        user_id: i32,
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
                let html_content = MessageFormatter::markdown_to_html_safe(content);
                
                // prepare header template that will be added to each part
                let header = format!(
                    "📊 <b>Channel Analysis Results</b> by <a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">@ScratchAuthorEgoBot</a>\n\n\
                    🎯 <b>Channel:</b> <code>{}</code>\n\n",
                    user_id,
                    MessageFormatter::escape_html(channel_name)
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

                // calculate available space for content after headers (using UTF-16 code units as Telegram does)
                const MAX_MESSAGE_LENGTH: usize = 3584;
                let headers_length = MessageFormatter::count_utf16_code_units(&header) + MessageFormatter::count_utf16_code_units(&analysis_header);
                let available_content_length = MAX_MESSAGE_LENGTH.saturating_sub(headers_length + 100); // buffer for part indicators

                // split content if needed
                let content_chunks = MessageFormatter::split_message_into_chunks(&html_content, available_content_length);
                
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

    #[allow(dead_code)]
    fn parse_group_analysis_request(text: &str) -> Option<i64> {
        // try parsing as "Group -123456789" format first
        if text.starts_with("Group ") {
            if let Some(chat_id_str) = text.strip_prefix("Group ") {
                if let Ok(chat_id) = chat_id_str.parse::<i64>() {
                    return Some(chat_id);
                }
            }
        }
        
        // try parsing as direct chat ID (negative numbers for groups)
        if let Ok(chat_id) = text.parse::<i64>() {
            if chat_id < 0 {  // negative chat IDs are typically groups/supergroups
                return Some(chat_id);
            }
        }
        
        None
    }

    #[allow(dead_code)]
    async fn handle_group_analysis_request(
        ctx: BotContext,
        msg: Message,
        chat_id: i64,
    ) -> ResponseResult<()> {
        // get user info from telegram message
        let telegram_user_id = msg.from.as_ref().map(|user| user.id.0 as i64).unwrap_or(0);
        let username = msg.from.as_ref().and_then(|user| user.username.as_deref());
        let first_name = msg.from.as_ref().map(|user| user.first_name.as_str());
        let last_name = msg.from.as_ref().and_then(|user| user.last_name.as_deref());

        // verify user is member of the group
        let user_groups = match ctx.group_handler.get_user_groups(telegram_user_id).await {
            Ok(groups) => groups,
            Err(e) => {
                error!("Failed to get user groups for {}: {}", telegram_user_id, e);
                ctx.bot.send_message(msg.chat.id, "❌ Error accessing group information.")
                    .await?;
                return Ok(());
            }
        };

        if !user_groups.contains(&chat_id) {
            ctx.bot.send_message(
                msg.chat.id, 
                "❌ You don't have access to this group analysis. You need to be a member of the group when the analysis was performed."
            )
            .await?;
            return Ok(());
        }

        // check if analysis exists for this group
        let analysis = match ctx.group_handler.get_available_analyses(chat_id).await {
            Ok(Some(analysis)) => analysis,
            Ok(None) => {
                ctx.bot.send_message(
                    msg.chat.id,
                    "❌ No analysis available for this group. The group needs to trigger an analysis first by mentioning the bot."
                )
                .await?;
                return Ok(());
            }
            Err(e) => {
                error!("Failed to get group analysis for {}: {}", chat_id, e);
                ctx.bot.send_message(msg.chat.id, "❌ Error accessing group analysis.")
                    .await?;
                return Ok(());
            }
        };

        // get or create user and check credits
        let user = match ctx.user_manager
            .get_or_create_user(telegram_user_id, username, first_name, last_name, None, None)
            .await
        {
            Ok((user, _)) => user,
            Err(e) => {
                error!("Failed to get/create user: {}", e);
                ctx.bot.send_message(msg.chat.id, "❌ Error processing user request.")
                    .await?;
                return Ok(());
            }
        };

        // check if user has credits
        if user.analysis_credits <= 0 {
            let no_credits_msg = format!(
                "❌ <b>No Analysis Credits Available</b>\n\n\
                You need 1 credit to access group analysis results.\n\n\
                💰 <b>Purchase Credits:</b>\n\
                • 1 analysis for {} ⭐ stars\n\
                • 10 analyses for {} ⭐ stars (save {} stars!)\n\n\
                📊 <b>Your Stats:</b>\n\
                • Credits remaining: <code>{}</code>\n\
                • Total analyses performed: <code>{}</code>",
                SINGLE_PACKAGE_PRICE,
                BULK_PACKAGE_PRICE,
                (SINGLE_PACKAGE_PRICE * BULK_PACKAGE_AMOUNT as u32) - BULK_PACKAGE_PRICE,
                user.analysis_credits,
                user.total_analyses_performed
            );

            ctx.bot.send_message(msg.chat.id, no_credits_msg)
                .parse_mode(ParseMode::Html)
                .reply_markup(CallbackHandler::create_payment_keyboard())
                .await?;
            return Ok(());
        }

        // consume credit and store access record
        match ctx.user_manager.consume_credit_for_group_analysis(user.id).await {
            Ok(_) => {},
            Err(e) => {
                error!("Failed to consume credit for user {}: {}", user.id, e);
                ctx.bot.send_message(msg.chat.id, "❌ Error processing credit consumption.")
                    .await?;
                return Ok(());
            }
        };

        // send analysis results
        Self::send_group_analysis_results(&ctx, &msg, &analysis).await?;

        Ok(())
    }

    #[allow(dead_code)]
    async fn send_group_analysis_results(
        ctx: &BotContext,
        msg: &Message,
        analysis: &crate::handlers::group_handler::GroupAnalysisData,
    ) -> ResponseResult<()> {
        let analyzed_users: Vec<String> = analysis.analyzed_users.iter()
            .map(|user| {
                if let Some(username) = &user.username {
                    format!("@{}", username)
                } else if let Some(first_name) = &user.first_name {
                    first_name.clone()
                } else {
                    format!("User {}", user.telegram_user_id)
                }
            })
            .collect();

        let header_msg = format!(
            "🎭 <b>Group Analysis Results</b>\n\n\
            📊 <b>Analysis Summary:</b>\n\
            • Messages analyzed: <code>{}</code>\n\
            • Users analyzed: <code>{}</code>\n\
            • Analysis date: <code>{}</code>\n\n\
            <b>Analyzed members:</b> {}\n\n",
            analysis.message_count,
            analysis.analyzed_users.len(),
            analysis.analysis_timestamp.format("%Y-%m-%d %H:%M UTC"),
            analyzed_users.join(", ")
        );

        // send header first
        ctx.bot.send_message(msg.chat.id, header_msg)
            .parse_mode(ParseMode::Html)
            .await?;

        // send each analysis type
        if let Some(professional) = &analysis.professional {
            let professional_msg = format!(
                "💼 <b>Professional Analysis</b>\n\n{}",
                MessageFormatter::escape_html(professional)
            );
            ctx.bot.send_message(msg.chat.id, professional_msg)
                .parse_mode(ParseMode::Html)
                .await?;
        }

        if let Some(personal) = &analysis.personal {
            let personal_msg = format!(
                "🧠 <b>Personal Analysis</b>\n\n{}",
                MessageFormatter::escape_html(personal)
            );
            ctx.bot.send_message(msg.chat.id, personal_msg)
                .parse_mode(ParseMode::Html)
                .await?;
        }

        if let Some(roast) = &analysis.roast {
            let roast_msg = format!(
                "🔥 <b>Roast Analysis</b>\n\n{}",
                MessageFormatter::escape_html(roast)
            );
            ctx.bot.send_message(msg.chat.id, roast_msg)
                .parse_mode(ParseMode::Html)
                .await?;
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub async fn handle_group_analysis_request_direct(
        ctx: BotContext,
        user_chat_id: ChatId,
        chat_id: i64,
        user: crate::user_manager::User,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // verify user is member of the group
        let user_groups = ctx.group_handler.get_user_groups(user.telegram_user_id).await?;
        if !user_groups.contains(&chat_id) {
            ctx.bot.send_message(
                user_chat_id, 
                "❌ You don't have access to this group analysis. You need to be a member of the group when the analysis was performed."
            ).await?;
            return Ok(());
        }

        // check if analysis exists for this group
        let analysis = match ctx.group_handler.get_available_analyses(chat_id).await? {
            Some(analysis) => analysis,
            None => {
                ctx.bot.send_message(
                    user_chat_id,
                    "❌ No analysis available for this group. The group needs to trigger an analysis first by mentioning the bot."
                ).await?;
                return Ok(());
            }
        };

        // consume credit and store access record
        ctx.user_manager.consume_credit_for_group_analysis(user.id).await?;

        // send analysis results
        Self::send_group_analysis_results_direct(&ctx, user_chat_id, &analysis).await?;
        
        Ok(())
    }

    #[allow(dead_code)]
    async fn send_group_analysis_results_direct(
        ctx: &BotContext,
        chat_id: ChatId,
        analysis: &crate::handlers::group_handler::GroupAnalysisData,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let analyzed_users: Vec<String> = analysis.analyzed_users.iter()
            .map(|user| {
                if let Some(username) = &user.username {
                    format!("@{}", username)
                } else if let Some(first_name) = &user.first_name {
                    first_name.clone()
                } else {
                    format!("User {}", user.telegram_user_id)
                }
            })
            .collect();

        let header_msg = format!(
            "🎭 <b>Group Analysis Results</b>\n\n\
            📊 <b>Analysis Summary:</b>\n\
            • Messages analyzed: <code>{}</code>\n\
            • Users analyzed: <code>{}</code>\n\
            • Analysis date: <code>{}</code>\n\n\
            <b>Analyzed members:</b> {}\n\n",
            analysis.message_count,
            analysis.analyzed_users.len(),
            analysis.analysis_timestamp.format("%Y-%m-%d %H:%M UTC"),
            analyzed_users.join(", ")
        );

        // send header first
        ctx.bot.send_message(chat_id, header_msg)
            .parse_mode(ParseMode::Html)
            .await?;

        // send each analysis type
        if let Some(professional) = &analysis.professional {
            let professional_msg = format!(
                "💼 <b>Professional Analysis</b>\n\n{}",
                MessageFormatter::escape_html(professional)
            );
            ctx.bot.send_message(chat_id, professional_msg)
                .parse_mode(ParseMode::Html)
                .await?;
        }

        if let Some(personal) = &analysis.personal {
            let personal_msg = format!(
                "🧠 <b>Personal Analysis</b>\n\n{}",
                MessageFormatter::escape_html(personal)
            );
            ctx.bot.send_message(chat_id, personal_msg)
                .parse_mode(ParseMode::Html)
                .await?;
        }

        if let Some(roast) = &analysis.roast {
            let roast_msg = format!(
                "🔥 <b>Roast Analysis</b>\n\n{}",
                MessageFormatter::escape_html(roast)
            );
            ctx.bot.send_message(chat_id, roast_msg)
                .parse_mode(ParseMode::Html)
                .await?;
        }

        Ok(())
    }
}
