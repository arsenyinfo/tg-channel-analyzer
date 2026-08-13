use log::{error, info};
use regex::Regex;
use std::collections::HashMap;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, ChatId, ParseMode, PreCheckoutQuery, SuccessfulPayment};
use teloxide::utils::command::BotCommands;
use tokio::sync::Mutex;

use crate::analysis::AnalysisEngine;
use crate::analysis::MessageDict;
use crate::cache::{AnalysisResult, CacheManager};
use crate::handlers::{
    payment_handler::{BULK_PACKAGE_AMOUNT, BULK_PACKAGE_PRICE, SINGLE_PACKAGE_PRICE},
    CallbackHandler, CommandHandler, PaymentHandler,
};
use crate::localization::Lang;
use crate::user_manager::{UserManager, UserManagerError};
use crate::utils::MessageFormatter;
use deadpool_postgres::Pool;

// per-channel locks to prevent concurrent LLM calls for the same channel
pub type ChannelLocks = Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Supported commands:")]
pub enum Command {
    #[command(description = "start the bot")]
    Start,
    #[command(description = "buy 1 analysis for 50 stars")]
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
    channel_locks: ChannelLocks,
}

#[derive(Clone)]
pub struct BotContext {
    pub bot: Arc<Bot>,
    pub analysis_engine: Arc<Mutex<AnalysisEngine>>,
    pub user_manager: Arc<UserManager>,
    pub payment_handler: PaymentHandler,
    pub channel_locks: ChannelLocks,
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

            let mut client = match pool.get().await {
                Ok(client) => client,
                Err(e) => {
                    error!(
                        "Failed to get database connection for queue processor: {}",
                        e
                    );
                    continue;
                }
            };

            // hold a transaction across the claim, send, and status update so the row lock
            // spans the whole critical section (FOR UPDATE SKIP LOCKED in autocommit would
            // release the lock immediately, allowing a second processor to double-send)
            let transaction = match client.transaction().await {
                Ok(tx) => tx,
                Err(e) => {
                    error!("Failed to open queue transaction: {}", e);
                    continue;
                }
            };

            let row = match transaction
                .query_opt(
                    "SELECT id, telegram_user_id, message, parse_mode
                 FROM message_queue
                 WHERE status = 'pending'
                 ORDER BY created_at
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED",
                    &[],
                )
                .await
            {
                Ok(row) => row,
                Err(e) => {
                    error!("Failed to query message queue: {}", e);
                    continue;
                }
            };

            let Some(row) = row else {
                let _ = transaction.rollback().await;
                continue;
            };

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

            let update_result = match &send_result {
                Ok(_) => transaction
                    .execute(
                        "UPDATE message_queue SET status = 'sent', sent_at = NOW() WHERE id = $1",
                        &[&id],
                    )
                    .await,
                Err(e) => {
                    let error_msg = e.to_string();
                    transaction
                        .execute(
                            "UPDATE message_queue SET status = 'failed', error_message = $2 WHERE id = $1",
                            &[&id, &error_msg],
                        )
                        .await
                }
            };

            match update_result {
                Ok(_) => {
                    if let Err(e) = transaction.commit().await {
                        error!("Failed to commit queue status update for {}: {}", id, e);
                    }
                }
                Err(e) => {
                    error!("Failed to update message status for {}: {}", id, e);
                    let _ = transaction.rollback().await;
                }
            }
        }
    }

    pub async fn new(
        bot_token: &str,
        user_manager: Arc<UserManager>,
        pool: Arc<Pool>,
        analysis_engine: Arc<Mutex<AnalysisEngine>>,
        channel_locks: ChannelLocks,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let bot = Arc::new(Bot::new(bot_token));
        let payment_handler = PaymentHandler::new(user_manager.clone());

        Ok(Self {
            bot,
            analysis_engine,
            user_manager,
            pool,
            payment_handler,
            channel_locks,
        })
    }

    pub async fn run(&self) {
        info!("Starting Telegram bot...");

        // spawn message queue processor
        let bot_clone = self.bot.clone();
        let pool_clone = self.pool.clone();
        tokio::spawn(async move {
            Self::run_message_queue_processor(bot_clone, pool_clone).await;
        });

        // create context for all handlers (shares the engine + channel locks with recovery)
        let ctx = BotContext {
            bot: self.bot.clone(),
            analysis_engine: self.analysis_engine.clone(),
            user_manager: self.user_manager.clone(),
            payment_handler: self.payment_handler.clone(),
            channel_locks: self.channel_locks.clone(),
        };

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
                    .branch(dptree::entry().filter_command::<Command>().endpoint({
                        let ctx = ctx.clone();
                        move |msg: Message, cmd: Command| {
                            let ctx = ctx.clone();
                            async move { CommandHandler::handle_command(ctx, msg, cmd).await }
                        }
                    }))
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
                                    async move {
                                        ctx.payment_handler
                                            .handle_successful_payment(ctx.bot, msg, payment)
                                            .await
                                    }
                                }
                            }),
                    )
                    .branch(dptree::endpoint({
                        let ctx = ctx.clone();
                        move |msg: Message| {
                            let ctx = ctx.clone();
                            async move { Self::handle_message(ctx, msg).await }
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

    async fn handle_message(ctx: BotContext, msg: Message) -> ResponseResult<()> {
        let lang = Lang::from_code(
            msg.from
                .as_ref()
                .and_then(|user| user.language_code.as_deref()),
        );

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
                let language_code = msg
                    .from
                    .as_ref()
                    .and_then(|user| user.language_code.as_deref());

                // get or create user and check credits
                let user = match ctx
                    .user_manager
                    .get_or_create_user(
                        telegram_user_id,
                        username,
                        first_name,
                        last_name,
                        None,
                        language_code,
                    )
                    .await
                {
                    Ok((user, _)) => user,
                    Err(e) => {
                        error!("Failed to get/create user: {}", e);
                        ctx.bot
                            .send_message(msg.chat.id, lang.error_processing_request())
                            .await?;
                        return Ok(());
                    }
                };

                // check if user has credits
                if user.analysis_credits <= 0 {
                    let bulk_discount =
                        (SINGLE_PACKAGE_PRICE * BULK_PACKAGE_AMOUNT as u32) - BULK_PACKAGE_PRICE;
                    let no_credits_msg = lang.no_credits_available(
                        SINGLE_PACKAGE_PRICE,
                        BULK_PACKAGE_PRICE,
                        bulk_discount,
                        user.analysis_credits,
                        user.total_analyses_performed,
                    );

                    ctx.bot
                        .send_message(msg.chat.id, no_credits_msg)
                        .parse_mode(ParseMode::Html)
                        .reply_markup(CallbackHandler::create_payment_keyboard(lang))
                        .await?;
                    return Ok(());
                }

                // send immediate response with credit info
                let credits_msg = lang.analysis_starting(user.analysis_credits - 1);
                ctx.bot
                    .send_message(msg.chat.id, credits_msg)
                    .parse_mode(ParseMode::Html)
                    .await?;

                // show analysis type selection directly (validation will happen during analysis)
                let selection_msg =
                    lang.analysis_select_type(&MessageFormatter::escape_html(&channel_name));

                ctx.bot
                    .send_message(msg.chat.id, selection_msg)
                    .parse_mode(ParseMode::Html)
                    .reply_markup(CallbackHandler::create_analysis_selection_keyboard(
                        &channel_name,
                        lang,
                    ))
                    .await?;
            } else {
                // send help message for invalid input
                ctx.bot
                    .send_message(msg.chat.id, lang.error_invalid_channel())
                    .await?;
            }
        }
        Ok(())
    }

    async fn check_fast_path_cache(
        cache: &CacheManager,
        channel_name: &str,
        analysis_type: &str,
    ) -> Option<(AnalysisResult, Vec<MessageDict>)> {
        let messages = match cache.load_channel_messages(channel_name).await {
            Some(msgs) if !msgs.is_empty() => msgs,
            _ => return None,
        };

        let cache_key = cache.get_analysis_cache_key(&messages);

        let cached_result = match cache.load_llm_result(&cache_key).await {
            Some(result) => result,
            None => return None,
        };

        let analysis_content = match analysis_type {
            "professional" => &cached_result.professional,
            "personal" => &cached_result.personal,
            "roast" => &cached_result.roast,
            _ => return None,
        };

        match analysis_content {
            Some(content) if !content.is_empty() => {
                info!(
                    "Fast path hit: using cached {} analysis for channel {}",
                    analysis_type, channel_name
                );
                Some((cached_result, messages))
            }
            _ => {
                info!(
                    "Fast path miss: {} analysis not found in cache for channel {}",
                    analysis_type, channel_name
                );
                None
            }
        }
    }

    // This is the task boundary used by both callbacks and startup recovery; keeping the
    // dependencies explicit makes ownership across the spawned task clear.
    #[allow(clippy::too_many_arguments)]
    pub async fn perform_single_analysis(
        bot: Arc<Bot>,
        user_chat_id: ChatId,
        channel_name: String,
        analysis_type: String,
        analysis_engine: Arc<Mutex<AnalysisEngine>>,
        user_manager: Arc<UserManager>,
        user_id: i32,
        analysis_id: i32,
        channel_locks: ChannelLocks,
        lang: Lang,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!(
            "Starting {} analysis for channel: {}",
            analysis_type, channel_name
        );

        let cache = {
            let engine = analysis_engine.lock().await;
            engine.cache.clone()
        };

        if let Some((cached_result, _cached_messages)) =
            Self::check_fast_path_cache(&cache, &channel_name, &analysis_type).await
        {
            let remaining_credits = user_manager
                .atomic_complete_analysis(analysis_id, user_id)
                .await?;

            Self::deliver_or_refund(
                bot,
                &user_manager,
                user_chat_id,
                &channel_name,
                &analysis_type,
                cached_result,
                user_id,
                analysis_id,
                remaining_credits,
                lang,
            )
            .await?;

            return Ok(());
        }

        bot.send_message(user_chat_id, lang.analysis_in_progress(&analysis_type))
            .await?;

        let wait_needed = {
            let engine = analysis_engine.lock().await;
            engine.calculate_backend_wait_time()
        };

        if let Some(wait_duration) = wait_needed {
            info!(
                "Rate limit would block, waiting {}s before acquiring engine lock",
                wait_duration.as_secs()
            );
            tokio::time::sleep(wait_duration).await;
        }

        let analysis_data = {
            let mut engine = analysis_engine.lock().await;
            match engine.prepare_analysis_data(&channel_name).await {
                Ok(data) => data,
                Err(e) => {
                    error!(
                        "Failed to prepare analysis data for channel {}: {}",
                        channel_name, e
                    );
                    bot.send_message(user_chat_id, lang.error_analysis_prepare(&channel_name))
                        .parse_mode(ParseMode::Html)
                        .await?;
                    return Err(e);
                }
            }
        };

        if analysis_data.messages.is_empty() {
            bot.send_message(user_chat_id, lang.error_no_messages())
                .parse_mode(ParseMode::Html)
                .await?;
            return Err("No messages found in channel".into());
        }

        let channel_lock = {
            let mut locks = channel_locks.lock().await;
            // sweep out idle locks (strong_count == 1 means only the map holds it) so this
            // map does not grow unbounded with one entry per channel ever analyzed
            if locks.len() > 256 {
                locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            }
            locks
                .entry(channel_name.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };

        let _channel_guard = channel_lock.lock().await;

        let cached_result = {
            let engine = analysis_engine.lock().await;
            engine.cache.load_llm_result(&analysis_data.cache_key).await
        };

        let result = if let Some(cached_result) = cached_result {
            info!("Using cached LLM result for channel {}", channel_name);
            cached_result
        } else {
            let prompt =
                match crate::prompts::analysis::generate_analysis_prompt(&analysis_data.messages) {
                    Ok(p) => p,
                    Err(e) => {
                        error!(
                            "Failed to generate analysis prompt for channel {}: {}",
                            channel_name, e
                        );
                        bot.send_message(user_chat_id, lang.error_prompt_generation())
                            .parse_mode(ParseMode::Html)
                            .await?;
                        return Err(e);
                    }
                };

            info!(
                "Querying LLM for {} analysis of channel {}...",
                analysis_type, channel_name
            );
            let mut result =
                match crate::llm::analysis_query::query_and_parse_analysis(&prompt).await {
                    Ok(r) => r,
                    Err(e) => {
                        error!(
                            "Failed to query LLM for {} analysis of channel {}: {}",
                            analysis_type, channel_name, e
                        );
                        bot.send_message(user_chat_id, lang.error_ai_service())
                            .parse_mode(ParseMode::Html)
                            .await?;
                        return Err(e);
                    }
                };
            result.messages_count = analysis_data.messages.len();

            {
                let mut engine = analysis_engine.lock().await;
                if let Err(e) = engine
                    .finish_analysis(&analysis_data.cache_key, result.clone())
                    .await
                {
                    error!(
                        "Failed to cache analysis result for channel {}: {}",
                        channel_name, e
                    );
                }
            }

            result
        };

        // consume credit + mark completed atomically; delivery (and refund-on-failure) follows
        let remaining_credits = match user_manager
            .atomic_complete_analysis(analysis_id, user_id)
            .await
        {
            Ok(credits) => credits,
            Err(e) => {
                match &e {
                    UserManagerError::InsufficientCredits(user_id) => {
                        info!(
                            "Analysis {} not completed: user {} has insufficient credits",
                            analysis_id, user_id
                        );
                    }
                    _ => {
                        error!(
                            "Failed to atomically complete analysis {}: {}",
                            analysis_id, e
                        );
                    }
                }
                // mark as failed if atomic completion failed
                if let Err(mark_err) = user_manager.mark_analysis_failed(analysis_id).await {
                    error!(
                        "Failed to mark analysis {} as failed: {}",
                        analysis_id, mark_err
                    );
                }
                return Err(Box::new(e));
            }
        };

        // deliver the result, refunding the just-charged credit if delivery fails
        Self::deliver_or_refund(
            bot,
            &user_manager,
            user_chat_id,
            &channel_name,
            &analysis_type,
            result,
            user_id,
            analysis_id,
            remaining_credits,
            lang,
        )
        .await
    }

    /// delivers the completion message and analysis result. on success marks the analysis
    /// delivered; on any send failure refunds the credit (the user was charged but not served)
    /// and propagates the error. keeps money correct on routine Telegram send failures.
    #[allow(clippy::too_many_arguments)]
    async fn deliver_or_refund(
        bot: Arc<Bot>,
        user_manager: &Arc<UserManager>,
        user_chat_id: ChatId,
        channel_name: &str,
        analysis_type: &str,
        result: AnalysisResult,
        user_id: i32,
        analysis_id: i32,
        remaining_credits: i32,
        lang: Lang,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let delivery = async {
            bot.send_message(
                user_chat_id,
                lang.analysis_complete(analysis_type, user_id, remaining_credits),
            )
            .parse_mode(ParseMode::Html)
            .await?;

            Self::send_single_analysis_to_user(
                bot.clone(),
                user_chat_id,
                channel_name,
                analysis_type,
                result,
                user_id,
                lang,
            )
            .await?;

            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        }
        .await;

        match delivery {
            Ok(()) => {
                // if this write fails the row stays delivered_at=NULL and the next startup
                // reconciliation will refund an already-delivered analysis (a rare, user-favoring
                // double-benefit requiring a DB failure in this exact window); log it loudly
                if let Err(e) = user_manager.mark_analysis_delivered(analysis_id).await {
                    error!("Failed to mark analysis {} delivered: {}", analysis_id, e);
                }
                Ok(())
            }
            Err(e) => {
                error!(
                    "Delivery failed for analysis {}, refunding credit: {}",
                    analysis_id, e
                );
                if let Err(re) = user_manager.refund_analysis(analysis_id, user_id).await {
                    error!("Failed to refund analysis {}: {}", analysis_id, re);
                }
                Err(e)
            }
        }
    }

    async fn send_single_analysis_to_user(
        bot: Arc<Bot>,
        user_chat_id: ChatId,
        channel_name: &str,
        analysis_type: &str,
        result: AnalysisResult,
        user_id: i32,
        lang: Lang,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let analysis_content = match analysis_type {
            "professional" => &result.professional,
            "personal" => &result.personal,
            "roast" => &result.roast,
            _ => &None,
        };

        match analysis_content {
            Some(content) if !content.is_empty() => {
                // convert LLM markdown content to HTML first
                let html_content = MessageFormatter::markdown_to_html_safe(content);

                // prepare header template that will be added to each part
                let header = lang
                    .analysis_result_header(&MessageFormatter::escape_html(channel_name), user_id);
                let analysis_header = lang.analysis_type_header(analysis_type);

                // calculate available space for content after headers (using UTF-16 code units as Telegram does)
                const MAX_MESSAGE_LENGTH: usize = 3584;
                let headers_length = MessageFormatter::count_utf16_code_units(&header)
                    + MessageFormatter::count_utf16_code_units(&analysis_header);
                let available_content_length =
                    MAX_MESSAGE_LENGTH.saturating_sub(headers_length + 100); // buffer for part indicators

                // split content if needed
                let content_chunks = MessageFormatter::split_message_into_chunks(
                    &html_content,
                    available_content_length,
                );

                for (i, chunk) in content_chunks.iter().enumerate() {
                    let full_message = if content_chunks.len() > 1 {
                        format!(
                            "{}{}{}{}",
                            header,
                            analysis_header,
                            chunk,
                            lang.analysis_part_indicator(i + 1, content_chunks.len())
                        )
                    } else {
                        format!("{}{}{}", header, analysis_header, chunk)
                    };

                    bot.send_message(user_chat_id, full_message)
                        .parse_mode(ParseMode::Html)
                        .await?;
                }

                info!(
                    "Sent {} analysis results to user for channel: {} ({} parts)",
                    analysis_type,
                    channel_name,
                    content_chunks.len()
                );
            }
            _ => {
                error!(
                    "No {} analysis content available for channel: {} (user: {})",
                    analysis_type, channel_name, user_chat_id
                );
                bot.send_message(user_chat_id, lang.error_no_analysis_content(analysis_type))
                    .await?;
            }
        }

        Ok(())
    }
}
