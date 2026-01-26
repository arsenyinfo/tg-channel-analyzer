use log::{error, info};
use teloxide::prelude::*;
use teloxide::types::{
    CallbackQuery, ChatId, InlineKeyboardButton, InlineKeyboardMarkup, MaybeInaccessibleMessage,
};

use crate::bot::BotContext;
use crate::handlers::payment_handler::{
    PaymentHandler, BULK_PACKAGE_AMOUNT, BULK_PACKAGE_PRICE, SINGLE_PACKAGE_AMOUNT,
    SINGLE_PACKAGE_PRICE,
};
use crate::localization::Lang;
use crate::user_manager::UserManagerError;

pub struct CallbackHandler;

impl CallbackHandler {
    fn get_chat_id(message: &MaybeInaccessibleMessage) -> ChatId {
        match message {
            MaybeInaccessibleMessage::Regular(msg) => msg.chat.id,
            MaybeInaccessibleMessage::Inaccessible(msg) => msg.chat.id,
        }
    }

    pub fn create_payment_keyboard(lang: Lang) -> InlineKeyboardMarkup {
        let single_button = InlineKeyboardButton::callback(
            lang.btn_buy_single(SINGLE_PACKAGE_AMOUNT, SINGLE_PACKAGE_PRICE),
            "buy_single",
        );
        let bulk_button = InlineKeyboardButton::callback(
            lang.btn_buy_bulk(BULK_PACKAGE_AMOUNT, BULK_PACKAGE_PRICE),
            "buy_bulk",
        );

        InlineKeyboardMarkup::new(vec![vec![single_button], vec![bulk_button]])
    }

    pub fn create_analysis_selection_keyboard(channel_name: &str, lang: Lang) -> InlineKeyboardMarkup {
        let professional_button = InlineKeyboardButton::callback(
            lang.btn_professional_analysis(),
            format!("analysis_professional_{}", channel_name),
        );
        let personal_button = InlineKeyboardButton::callback(
            lang.btn_personal_analysis(),
            format!("analysis_personal_{}", channel_name),
        );
        let roast_button = InlineKeyboardButton::callback(
            lang.btn_roast_analysis(),
            format!("analysis_roast_{}", channel_name),
        );

        InlineKeyboardMarkup::new(vec![
            vec![professional_button],
            vec![personal_button],
            vec![roast_button],
        ])
    }

    pub async fn handle_callback_query(
        ctx: BotContext,
        query: CallbackQuery,
    ) -> ResponseResult<()> {
        let lang = Lang::from_code(query.from.language_code.as_deref());

        if let Some(data) = &query.data {
            if let Some(message) = &query.message {
                match data.as_str() {
                    "buy_single" => {
                        Self::handle_buy_single_callback(ctx, message, &query, lang).await?;
                    }
                    "buy_bulk" => {
                        Self::handle_buy_bulk_callback(ctx, message, &query, lang).await?;
                    }
                    callback_data if callback_data.starts_with("analysis_") => {
                        Self::handle_analysis_callback(ctx, message, &query, callback_data, lang)
                            .await?;
                    }
                    _ => {
                        ctx.bot.answer_callback_query(&query.id).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_buy_single_callback(
        ctx: BotContext,
        message: &MaybeInaccessibleMessage,
        query: &CallbackQuery,
        lang: Lang,
    ) -> ResponseResult<()> {
        PaymentHandler::send_payment_invoice(
            ctx.bot.clone(),
            Self::get_chat_id(message),
            SINGLE_PACKAGE_AMOUNT,
            SINGLE_PACKAGE_PRICE,
            lang.invoice_single_title(),
            lang.invoice_single_description(),
        )
        .await?;

        ctx.bot.answer_callback_query(&query.id).await?;
        Ok(())
    }

    async fn handle_buy_bulk_callback(
        ctx: BotContext,
        message: &MaybeInaccessibleMessage,
        query: &CallbackQuery,
        lang: Lang,
    ) -> ResponseResult<()> {
        let discount = (SINGLE_PACKAGE_PRICE * BULK_PACKAGE_AMOUNT as u32) - BULK_PACKAGE_PRICE;
        PaymentHandler::send_payment_invoice(
            ctx.bot.clone(),
            Self::get_chat_id(message),
            BULK_PACKAGE_AMOUNT,
            BULK_PACKAGE_PRICE,
            lang.invoice_bulk_title(),
            &lang.invoice_bulk_description(discount),
        )
        .await?;

        ctx.bot.answer_callback_query(&query.id).await?;
        Ok(())
    }

    async fn handle_analysis_callback(
        ctx: BotContext,
        message: &MaybeInaccessibleMessage,
        query: &CallbackQuery,
        callback_data: &str,
        lang: Lang,
    ) -> ResponseResult<()> {
        // parse analysis type and channel from callback data
        let parts: Vec<&str> = callback_data.splitn(3, '_').collect();
        if parts.len() >= 3 {
            let analysis_type = parts[1]; // professional, personal, or roast
            let channel_name = parts[2];

            let telegram_user_id = query.from.id.0 as i64;

            // check if user has credits before starting analysis
            let user = match ctx
                .user_manager
                .get_or_create_user(
                    telegram_user_id,
                    query.from.username.as_deref(),
                    Some(query.from.first_name.as_str()),
                    query.from.last_name.as_deref(),
                    None, // no referral in callback queries
                    query.from.language_code.as_deref(),
                )
                .await
            {
                Ok((user, _)) => user,
                Err(e) => {
                    error!("Failed to get user: {}", e);
                    ctx.bot
                        .send_message(Self::get_chat_id(message), lang.error_check_credits())
                        .await?;
                    return Ok(());
                }
            };

            if user.analysis_credits <= 0 {
                // no credits available, send payment options
                ctx.bot
                    .send_message(Self::get_chat_id(message), lang.no_credits_short())
                    .reply_markup(Self::create_payment_keyboard(lang))
                    .await?;

                ctx.bot.answer_callback_query(&query.id).await?;
                return Ok(());
            }

            // create pending analysis record first
            let analysis_id = match ctx
                .user_manager
                .create_pending_analysis(
                    user.id,
                    channel_name,
                    analysis_type,
                    query.from.language_code.as_deref(),
                )
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    let error_msg = match e {
                        UserManagerError::UserNotFound(_) => lang.error_user_not_found(),
                        _ => lang.error_start_analysis(),
                    };
                    let _ = ctx
                        .bot
                        .send_message(Self::get_chat_id(message), error_msg)
                        .await;
                    ctx.bot.answer_callback_query(&query.id).await?;
                    return Ok(());
                }
            };

            // start analysis in background
            Self::start_analysis_in_background(
                ctx.clone(),
                Self::get_chat_id(message),
                channel_name.to_string(),
                analysis_type.to_string(),
                user,
                analysis_id,
                lang,
            )
            .await;
        }

        ctx.bot.answer_callback_query(&query.id).await?;
        Ok(())
    }

    async fn start_analysis_in_background(
        ctx: BotContext,
        user_chat_id: ChatId,
        channel_name: String,
        analysis_type: String,
        user: crate::user_manager::User,
        analysis_id: i32,
        lang: Lang,
    ) {
        use crate::bot::TelegramBot;

        let bot_clone = ctx.bot.clone();
        let analysis_engine_clone = ctx.analysis_engine.clone();
        let user_manager_clone = ctx.user_manager.clone();
        let user_manager_error_clone = ctx.user_manager.clone();
        let channel_locks_clone = ctx.channel_locks.clone();

        tokio::spawn(async move {
            if let Err(e) = TelegramBot::perform_single_analysis(
                bot_clone.clone(),
                user_chat_id,
                channel_name.clone(),
                analysis_type.clone(),
                analysis_engine_clone,
                user_manager_clone,
                user.id,
                analysis_id,
                channel_locks_clone,
                lang,
            )
            .await
            {
                // mark analysis as failed
                if let Err(mark_err) = user_manager_error_clone
                    .mark_analysis_failed(analysis_id)
                    .await
                {
                    error!(
                        "Failed to mark analysis {} as failed: {}",
                        analysis_id, mark_err
                    );
                }

                if let Some(user_error) = e.downcast_ref::<crate::user_manager::UserManagerError>()
                {
                    match user_error {
                        crate::user_manager::UserManagerError::InsufficientCredits(user_id) => {
                            info!("Analysis failed: User {} has insufficient credits", user_id);
                            let _ = bot_clone
                                .send_message(user_chat_id, lang.error_insufficient_credits())
                                .await;
                        }
                        _ => {
                            error!(
                                "Analysis failed for channel {} (type: {}): {}",
                                channel_name, analysis_type, e
                            );
                            error!("User manager error during analysis: {}", user_error);
                            let _ = bot_clone
                                .send_message(user_chat_id, lang.error_system())
                                .await;
                        }
                    }
                } else {
                    // log the full error details
                    error!(
                        "Analysis failed for channel {} (type: {}): {}",
                        channel_name, analysis_type, e
                    );
                    error!("Non-user error during analysis: {}", e);
                    // don't send generic error - it's already handled in perform_single_analysis
                }
            }
        });
    }
}
