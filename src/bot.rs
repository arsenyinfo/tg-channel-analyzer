use chrono::{DateTime, Duration, NaiveTime, Utc};
use log::{error, info};
use regex::Regex;
use std::collections::HashMap;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, ChatId, ParseMode, PreCheckoutQuery, SuccessfulPayment};
use teloxide::utils::command::BotCommands;
use teloxide::{ApiError, RequestError};
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

struct QueuedMessage {
    id: i32,
    telegram_user_id: i64,
    message: String,
    parse_mode: String,
    campaign_id: Option<i64>,
    attempt_count: i32,
    max_attempts: i32,
    timezone: Option<String>,
    window_start: Option<NaiveTime>,
    window_end: Option<NaiveTime>,
    lease_token: String,
}

// per-channel locks to prevent concurrent LLM calls for the same channel
pub type ChannelLocks = Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Supported commands:")]
pub enum Command {
    #[command(description = "start the bot")]
    Start,
    #[command(description = "stop campaign notifications")]
    Stop,
    #[command(description = "buy 1 analysis for 100 stars")]
    Buy1,
    #[command(description = "buy 10 analyses for 500 stars")]
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
    const MESSAGE_QUEUE_LOCK_ID: i64 = 7_623_417_991;

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
                    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    continue;
                }
            };
            let transaction = match client.transaction().await {
                Ok(transaction) => transaction,
                Err(e) => {
                    error!("Failed to start queue claim transaction: {}", e);
                    continue;
                }
            };
            // The transaction-scoped advisory lock serializes claims across replicas and is
            // always released by PostgreSQL on commit/rollback/connection loss.
            let owns_lock: bool = match transaction
                .query_one(
                    "SELECT pg_try_advisory_xact_lock($1)",
                    &[&Self::MESSAGE_QUEUE_LOCK_ID],
                )
                .await
            {
                Ok(row) => row.get(0),
                Err(e) => {
                    error!("Failed to coordinate queue claim: {}", e);
                    continue;
                }
            };
            if !owns_lock {
                continue;
            }

            if let Err(e) = transaction
                .execute(
                    "UPDATE message_queue
                     SET status = 'delivery_unknown', lease_token = NULL, leased_until = NULL,
                         last_error_code = 'lease_expired',
                         error_message = 'Delivery outcome unknown after worker interruption'
                     WHERE status = 'processing'
                       AND (leased_until IS NULL OR leased_until < NOW())",
                    &[],
                )
                .await
            {
                error!("Failed to reconcile expired queue leases: {}", e);
                continue;
            }

            let lease_token = format!("{:032x}", rand::random::<u128>());
            let row = match transaction
                .query_opt(
                    r#"
                    SELECT mq.id, mq.telegram_user_id, mq.message, mq.parse_mode,
                           mq.campaign_id, mq.attempt_count, mq.max_attempts,
                           c.timezone, c.send_window_start, c.send_window_end,
                           c.cadence_seconds
                    FROM message_queue mq
                    LEFT JOIN campaigns c ON c.id = mq.campaign_id
                    WHERE mq.status = 'pending'
                      AND mq.attempt_count < mq.max_attempts
                      AND mq.scheduled_at <= NOW()
                      AND mq.next_attempt_at <= NOW()
                      AND NOT EXISTS (
                          SELECT 1 FROM campaign_suppressions cs
                          WHERE cs.telegram_user_id = mq.telegram_user_id
                      )
                      AND (
                          mq.campaign_id IS NULL
                          OR (
                              c.status = 'active'
                              AND c.next_send_at <= NOW()
                              AND (CURRENT_TIMESTAMP AT TIME ZONE c.timezone)::time
                                  >= c.send_window_start
                              AND (CURRENT_TIMESTAMP AT TIME ZONE c.timezone)::time
                                  < c.send_window_end
                          )
                      )
                    ORDER BY mq.next_attempt_at, mq.scheduled_at, mq.id
                    LIMIT 1
                    FOR UPDATE OF mq SKIP LOCKED
                    "#,
                    &[],
                )
                .await
            {
                Ok(row) => row,
                Err(e) => {
                    error!("Message queue selection failed: {}", e);
                    continue;
                }
            };
            let Some(row) = row else {
                if let Err(e) = transaction.commit().await {
                    error!("Failed to commit empty queue claim: {}", e);
                }
                continue;
            };

            let queued = QueuedMessage {
                id: row.get(0),
                telegram_user_id: row.get(1),
                message: row.get(2),
                parse_mode: row.get(3),
                campaign_id: row.get(4),
                attempt_count: row.get::<_, i32>(5) + 1,
                max_attempts: row.get(6),
                timezone: row.get(7),
                window_start: row.get(8),
                window_end: row.get(9),
                lease_token,
            };
            if let Err(e) = transaction
                .execute(
                    "UPDATE message_queue
                     SET status = 'processing', attempt_count = attempt_count + 1,
                         lease_token = $2, leased_until = NOW() + INTERVAL '5 minutes'
                     WHERE id = $1",
                    &[&queued.id, &queued.lease_token],
                )
                .await
            {
                error!("Message queue claim update failed: {}", e);
                continue;
            }
            if let Some(campaign_id) = queued.campaign_id {
                let cadence_seconds: i32 = row.get(10);
                if let Err(e) = transaction
                    .execute(
                        "UPDATE campaigns
                         SET next_send_at = NOW() + ($2::INTEGER * INTERVAL '1 second'),
                             updated_at = NOW()
                         WHERE id = $1",
                        &[&campaign_id, &cadence_seconds],
                    )
                    .await
                {
                    error!("Failed to advance campaign pacing: {}", e);
                    continue;
                }
            }
            if let Err(e) = transaction.commit().await {
                error!("Failed to commit message queue claim: {}", e);
                continue;
            }

            if queued.campaign_id.is_some() {
                match client
                    .query_opt(
                        "SELECT 1 FROM campaign_suppressions WHERE telegram_user_id = $1",
                        &[&queued.telegram_user_id],
                    )
                    .await
                {
                    Ok(Some(_)) => {
                        let _ = Self::finish_queue_row(
                            &client,
                            &queued,
                            "failed",
                            "user_opt_out",
                            "Recipient opted out before delivery",
                        )
                        .await;
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        error!(
                            "Could not verify suppression for queue row {}; leaving lease for safe reconciliation: {}",
                            queued.id, e
                        );
                        continue;
                    }
                }
            }

            let parse_mode = match queued.parse_mode.as_str() {
                "HTML" => ParseMode::Html,
                "MarkdownV2" => ParseMode::MarkdownV2,
                _ => {
                    error!("Queue row {} has invalid parse mode", queued.id);
                    let _ = Self::finish_queue_row(
                        &client,
                        &queued,
                        "failed",
                        "invalid_parse_mode",
                        "Invalid parse mode",
                    )
                    .await;
                    continue;
                }
            };

            match bot
                .send_message(ChatId(queued.telegram_user_id), &queued.message)
                .parse_mode(parse_mode)
                .await
            {
                Ok(_) => match client
                    .execute(
                        "UPDATE message_queue
                         SET status = 'sent', sent_at = NOW(), lease_token = NULL,
                             leased_until = NULL, error_message = NULL, last_error_code = NULL
                         WHERE id = $1 AND status = 'processing' AND lease_token = $2",
                        &[&queued.id, &queued.lease_token],
                    )
                    .await
                {
                    Ok(1) => {}
                    Ok(_) => error!(
                        "Queue row {} lost its lease after Telegram accepted it",
                        queued.id
                    ),
                    Err(e) => error!(
                        "Telegram accepted queue row {}, but recording success failed: {}",
                        queued.id, e
                    ),
                },
                Err(e) => {
                    if let Err(update_error) = Self::handle_queue_error(&client, &queued, &e).await
                    {
                        error!(
                            "Failed to record delivery error for queue row {}: {}",
                            queued.id, update_error
                        );
                    }
                    if matches!(e, RequestError::Api(ApiError::InvalidToken)) {
                        error!("Telegram rejected the bot token; queue worker backing off");
                        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
                    }
                }
            }
        }
    }

    async fn finish_queue_row(
        client: &deadpool_postgres::Object,
        queued: &QueuedMessage,
        status: &str,
        error_code: &str,
        error_message: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let updated = client
            .execute(
                "UPDATE message_queue
                 SET status = $3, lease_token = NULL, leased_until = NULL,
                     last_error_code = $4, error_message = $5
                 WHERE id = $1 AND status = 'processing' AND lease_token = $2",
                &[
                    &queued.id,
                    &queued.lease_token,
                    &status,
                    &error_code,
                    &error_message,
                ],
            )
            .await?;
        if updated != 1 {
            return Err(format!("queue row {} lease was lost", queued.id).into());
        }
        Ok(())
    }

    fn retry_delay(attempt_count: i32) -> Duration {
        let exponent = u32::try_from((attempt_count - 1).clamp(0, 4)).unwrap_or(0);
        Duration::seconds((30_i64 * 4_i64.pow(exponent)).min(3600))
    }

    fn normalize_queue_retry(
        queued: &QueuedMessage,
        candidate: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, Box<dyn std::error::Error + Send + Sync>> {
        match (
            queued.timezone.as_deref(),
            queued.window_start,
            queued.window_end,
        ) {
            (Some(timezone), Some(start), Some(end)) => {
                crate::campaign_schedule::normalize_retry_time(candidate, timezone, start, end)
            }
            _ => Ok(candidate),
        }
    }

    async fn retry_queue_row(
        client: &deadpool_postgres::Object,
        queued: &QueuedMessage,
        candidate: DateTime<Utc>,
        error_code: &str,
        error_message: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if queued.attempt_count >= queued.max_attempts {
            return Self::finish_queue_row(
                client,
                queued,
                "failed",
                "attempts_exhausted",
                "Maximum delivery attempts exhausted",
            )
            .await;
        }
        let retry_at = Self::normalize_queue_retry(queued, candidate)?;
        let updated = client
            .execute(
                "UPDATE message_queue
                 SET status = 'pending', next_attempt_at = $3, lease_token = NULL,
                     leased_until = NULL, last_error_code = $4, error_message = $5
                 WHERE id = $1 AND status = 'processing' AND lease_token = $2",
                &[
                    &queued.id,
                    &queued.lease_token,
                    &retry_at,
                    &error_code,
                    &error_message,
                ],
            )
            .await?;
        if updated != 1 {
            return Err(format!("queue row {} lease was lost", queued.id).into());
        }
        Ok(())
    }

    async fn handle_queue_error(
        client: &deadpool_postgres::Object,
        queued: &QueuedMessage,
        error: &RequestError,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match error {
            RequestError::RetryAfter(delay) => {
                let candidate = Utc::now() + delay.chrono_duration() + Duration::seconds(1);
                Self::retry_queue_row(
                    client,
                    queued,
                    candidate,
                    "telegram_retry_after",
                    "Telegram requested a delayed retry",
                )
                .await
            }
            RequestError::MigrateToChatId(new_chat_id) => {
                let updated = client
                    .execute(
                        "UPDATE message_queue
                         SET telegram_user_id = $3
                         WHERE id = $1 AND status = 'processing' AND lease_token = $2",
                        &[&queued.id, &queued.lease_token, &new_chat_id.0],
                    )
                    .await?;
                if updated != 1 {
                    return Err(format!("queue row {} lease was lost", queued.id).into());
                }
                Self::retry_queue_row(
                    client,
                    queued,
                    Utc::now(),
                    "migrate_to_chat",
                    "Telegram migrated the chat identifier",
                )
                .await
            }
            RequestError::Network(source) if source.is_connect() => {
                let candidate = Utc::now() + Self::retry_delay(queued.attempt_count);
                Self::retry_queue_row(
                    client,
                    queued,
                    candidate,
                    "network_connect",
                    "Could not connect to Telegram",
                )
                .await
            }
            RequestError::Network(_) | RequestError::InvalidJson { .. } => {
                Self::finish_queue_row(
                    client,
                    queued,
                    "delivery_unknown",
                    "ambiguous_delivery",
                    "Telegram delivery may have succeeded; automatic retry suppressed",
                )
                .await
            }
            RequestError::Io(_) => {
                let candidate = Utc::now() + Self::retry_delay(queued.attempt_count);
                Self::retry_queue_row(
                    client,
                    queued,
                    candidate,
                    "local_io",
                    "Local I/O failure before delivery",
                )
                .await
            }
            RequestError::Api(
                ApiError::BotBlocked
                | ApiError::UserDeactivated
                | ApiError::CantInitiateConversation
                | ApiError::ChatNotFound
                | ApiError::CantTalkWithBots
                | ApiError::BotKicked
                | ApiError::BotKickedFromSupergroup
                | ApiError::BotKickedFromChannel,
            ) => {
                client
                    .execute(
                        "INSERT INTO campaign_suppressions (telegram_user_id, reason)
                         VALUES ($1, 'telegram_permanent_failure')
                         ON CONFLICT (telegram_user_id) DO NOTHING",
                        &[&queued.telegram_user_id],
                    )
                    .await?;
                client
                    .execute(
                        "UPDATE message_queue
                         SET status = 'failed', last_error_code = 'recipient_unreachable',
                             error_message = 'Recipient suppressed after permanent Telegram failure'
                         WHERE telegram_user_id = $1 AND campaign_id IS NOT NULL
                           AND status = 'pending'",
                        &[&queued.telegram_user_id],
                    )
                    .await?;
                Self::finish_queue_row(
                    client,
                    queued,
                    "failed",
                    "recipient_unreachable",
                    "Telegram recipient is permanently unreachable",
                )
                .await
            }
            RequestError::Api(
                ApiError::MessageIsTooLong
                | ApiError::MessageTextIsEmpty
                | ApiError::CantParseEntities(_)
                | ApiError::CantParseUrl
                | ApiError::WrongHttpUrl
                | ApiError::RequestEntityTooLarge,
            ) => {
                if let Some(campaign_id) = queued.campaign_id {
                    client
                        .execute(
                            "UPDATE campaigns SET status = 'paused', updated_at = NOW()
                             WHERE id = $1",
                            &[&campaign_id],
                        )
                        .await?;
                }
                Self::finish_queue_row(
                    client,
                    queued,
                    "failed",
                    "invalid_content",
                    "Campaign paused because Telegram rejected the message content",
                )
                .await
            }
            RequestError::Api(ApiError::InvalidToken) => {
                client
                    .execute(
                        "UPDATE campaigns SET status = 'paused', updated_at = NOW()
                         WHERE status = 'active'",
                        &[],
                    )
                    .await?;
                Self::retry_queue_row(
                    client,
                    queued,
                    Utc::now() + Duration::hours(1),
                    "invalid_bot_token",
                    "All campaigns paused because Telegram rejected the bot token",
                )
                .await
            }
            RequestError::Api(_) => {
                let candidate = Utc::now() + Self::retry_delay(queued.attempt_count);
                Self::retry_queue_row(
                    client,
                    queued,
                    candidate,
                    "telegram_api",
                    "Telegram rejected the request; scheduled for bounded retry",
                )
                .await
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

        if let Some((cached_result, cached_messages)) =
            Self::check_fast_path_cache(&cache, &channel_name, &analysis_type).await
        {
            let cache_key = cache.get_analysis_cache_key(&cached_messages);
            let remaining_credits = user_manager
                .atomic_complete_analysis(analysis_id, user_id, "cache", Some(&cache_key))
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

        let (result, result_source) = if let Some(cached_result) = cached_result {
            info!("Using cached LLM result for channel {}", channel_name);
            (cached_result, "cache")
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
            let llm_context = crate::llm::LlmRunContext::new(
                user_manager.pool(),
                "channel_analysis",
                Some(analysis_id),
            );
            let mut result =
                match crate::llm::analysis_query::query_and_parse_analysis(&prompt, &llm_context)
                    .await
                {
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

            (result, "generated")
        };

        // consume credit + mark completed atomically; delivery (and refund-on-failure) follows
        let remaining_credits = match user_manager
            .atomic_complete_analysis(
                analysis_id,
                user_id,
                result_source,
                Some(&analysis_data.cache_key),
            )
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
