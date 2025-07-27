use log::{error, info, warn};
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, ChatId, InlineKeyboardButton, InlineKeyboardMarkup, MaybeInaccessibleMessage, ParseMode};

use crate::bot::BotContext;
use crate::handlers::payment_handler::{PaymentHandler, SINGLE_PACKAGE_PRICE, BULK_PACKAGE_PRICE, SINGLE_PACKAGE_AMOUNT, BULK_PACKAGE_AMOUNT};
use crate::user_manager::UserManagerError;
use crate::user_session::SessionState;

pub struct CallbackHandler;

impl CallbackHandler {
    fn get_chat_id(message: &MaybeInaccessibleMessage) -> ChatId {
        match message {
            MaybeInaccessibleMessage::Regular(msg) => msg.chat.id,
            MaybeInaccessibleMessage::Inaccessible(msg) => msg.chat.id,
        }
    }
    pub fn create_payment_keyboard() -> InlineKeyboardMarkup {
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

    pub fn create_analysis_selection_keyboard(channel_name: &str) -> InlineKeyboardMarkup {
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

    pub async fn handle_callback_query(
        ctx: BotContext,
        query: CallbackQuery,
    ) -> ResponseResult<()> {
        if let Some(data) = &query.data {
            if let Some(message) = &query.message {
                match data.as_str() {
                    "menu_channels" => {
                        Self::handle_menu_channels_callback(ctx, message, &query).await?;
                    }
                    "menu_groups" => {
                        Self::handle_menu_groups_callback(ctx, message, &query).await?;
                    }
                    "menu_buy" => {
                        Self::handle_menu_buy_callback(ctx, message, &query).await?;
                    }
                    "buy_single" => {
                        Self::handle_buy_single_callback(ctx, message, &query).await?;
                    }
                    "buy_bulk" => {
                        Self::handle_buy_bulk_callback(ctx, message, &query).await?;
                    }
                    callback_data if callback_data.starts_with("analysis_") => {
                        Self::handle_analysis_callback(ctx, message, &query, callback_data).await?;
                    }
                    callback_data if callback_data.starts_with("select_group_") => {
                        Self::handle_group_selection_callback(ctx, message, &query, callback_data).await?;
                    }
                    callback_data if callback_data.starts_with("group_analysis_") => {
                        Self::handle_group_analysis_type_callback(ctx, message, &query, callback_data).await?;
                    }
                    callback_data if callback_data.starts_with("group_user_") => {
                        Self::handle_group_user_selection_callback(ctx, message, &query, callback_data).await?;
                    }
                    callback_data if callback_data.starts_with("channel_analysis_") => {
                        Self::handle_channel_analysis_type_callback(ctx, message, &query, callback_data).await?;
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
    ) -> ResponseResult<()> {
        PaymentHandler::send_payment_invoice(
            ctx.bot.clone(),
            Self::get_chat_id(message),
            SINGLE_PACKAGE_AMOUNT,
            SINGLE_PACKAGE_PRICE,
            "1 Channel Analysis",
            "Get 1 analysis credit to analyze any Telegram channel",
        )
        .await?;

        ctx.bot.answer_callback_query(&query.id).await?;
        Ok(())
    }

    async fn handle_buy_bulk_callback(
        ctx: BotContext,
        message: &MaybeInaccessibleMessage,
        query: &CallbackQuery,
    ) -> ResponseResult<()> {
        PaymentHandler::send_payment_invoice(
            ctx.bot.clone(),
            Self::get_chat_id(message),
            BULK_PACKAGE_AMOUNT,
            BULK_PACKAGE_PRICE,
            "10 Channel Analyses",
            &format!("Get 10 analysis credits to analyze any Telegram channels ({} stars discount!)",
                (SINGLE_PACKAGE_PRICE * BULK_PACKAGE_AMOUNT as u32) - BULK_PACKAGE_PRICE),
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
    ) -> ResponseResult<()> {
        // parse analysis type and channel from callback data
        let parts: Vec<&str> = callback_data.splitn(3, '_').collect();
        if parts.len() >= 3 {
            let analysis_type = parts[1]; // professional, personal, or roast
            let channel_name = parts[2];

            let telegram_user_id = query.from.id.0 as i64;

            // check if user has credits before starting analysis
            let user = match ctx.user_manager
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
                    ctx.bot.send_message(
                        Self::get_chat_id(message),
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

                ctx.bot.send_message(Self::get_chat_id(message), message_text)
                    .reply_markup(Self::create_payment_keyboard())
                    .await?;

                ctx.bot.answer_callback_query(&query.id).await?;
                return Ok(());
            }

            // create pending analysis record first
            let analysis_id = match ctx.user_manager.create_pending_analysis(
                user.id,
                &channel_name,
                &analysis_type,
            ).await {
                Ok(id) => id,
                Err(e) => {
                    let error_msg = match e {
                        UserManagerError::UserNotFound(_) => "❌ User not found. Please try again.",
                        _ => "❌ Failed to start analysis. Please try again.",
                    };
                    let _ = ctx.bot.send_message(Self::get_chat_id(message), error_msg).await;
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
            ).await;
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
    ) {
        use crate::bot::TelegramBot;
        
        let bot_clone = ctx.bot.clone();
        let analysis_engine_clone = ctx.analysis_engine.clone();
        let user_manager_clone = ctx.user_manager.clone();
        let user_manager_error_clone = ctx.user_manager.clone();

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
            )
            .await
            {
                // mark analysis as failed
                if let Err(mark_err) = user_manager_error_clone.mark_analysis_failed(analysis_id).await {
                    error!("Failed to mark analysis {} as failed: {}", analysis_id, mark_err);
                }

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
                            error!("Analysis failed for channel {} (type: {}): {}", channel_name, analysis_type, e);
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
                    error!("Analysis failed for channel {} (type: {}): {}", channel_name, analysis_type, e);
                    error!("Non-user error during analysis: {}", e);
                    // Don't send generic error - it's already handled in perform_single_analysis
                }
            }
        });
    }

    async fn handle_menu_channels_callback(
        ctx: BotContext,
        message: &MaybeInaccessibleMessage,
        query: &CallbackQuery,
    ) -> ResponseResult<()> {
        let user_id = query.from.id.0 as i64;
        
        // set user session to awaiting channel input
        ctx.session_manager.set_session(user_id, SessionState::ChannelAnalysisAwaitingInput).await;
        
        let chat_id = Self::get_chat_id(message);
        let instruction_text = "📊 <b>Channel Analysis</b>\n\n\
            Send me a channel username or link:\n\
            • Format: <code>@channelname</code>\n\
            • Or: <code>https://t.me/channelname</code>\n\n\
            I'll validate the channel and show analysis options.";
        
        let message_id = message.id();
        ctx.bot.edit_message_text(chat_id, message_id, instruction_text)
            .parse_mode(ParseMode::Html)
            .await?;
        
        ctx.bot.answer_callback_query(&query.id).await?;
        Ok(())
    }

    async fn handle_menu_groups_callback(
        ctx: BotContext,
        message: &MaybeInaccessibleMessage,
        query: &CallbackQuery,
    ) -> ResponseResult<()> {
        let user_id = query.from.id.0 as i64;
        
        // set user session to selecting group
        ctx.session_manager.set_session(user_id, SessionState::GroupAnalysisSelectingGroup).await;
        
        // get available group analyses
        let available_groups = match ctx.group_handler.get_user_groups(user_id).await {
            Ok(chat_ids) => {
                let mut groups = Vec::new();
                for chat_id in chat_ids {
                    if let Ok(Some(analysis)) = ctx.group_handler.get_available_analyses(chat_id).await {
                        if !analysis.analyzed_users.is_empty() {
                            // get real group name from database
                            let group_name = match ctx.group_handler.get_group_name(chat_id).await {
                                Ok(Some(name)) => name,
                                _ => format!("Group {}", chat_id), // fallback to ID
                            };
                            groups.push((chat_id, group_name));
                        }
                    }
                }
                groups
            },
            Err(_) => Vec::new(),
        };

        if available_groups.is_empty() {
            ctx.session_manager.clear_session(user_id).await;
            ctx.bot.answer_callback_query(&query.id)
                .text("❌ No group analyses available")
                .await?;
            return Ok(());
        }

        // create keyboard with available groups
        let mut keyboard = Vec::new();
        for (chat_id, group_name) in available_groups.iter().take(10) { // limit to 10 groups
            keyboard.push(vec![InlineKeyboardButton::callback(
                group_name,
                format!("select_group_{}", chat_id)
            )]);
        }
        
        let group_keyboard = InlineKeyboardMarkup::new(keyboard);
        
        let group_text = "🎭 <b>Available Group Analyses</b>\n\n\
            Select a group to analyze:";
        
        let message_id = message.id();
        ctx.bot.edit_message_text(Self::get_chat_id(message), message_id, group_text)
            .parse_mode(ParseMode::Html)
            .reply_markup(group_keyboard)
            .await?;
        
        ctx.bot.answer_callback_query(&query.id).await?;
        Ok(())
    }

    async fn handle_menu_buy_callback(
        ctx: BotContext,
        message: &MaybeInaccessibleMessage,
        query: &CallbackQuery,
    ) -> ResponseResult<()> {
        let buy_text = "💰 <b>Purchase Analysis Credits</b>\n\n\
            Choose a package below:";
        
        let message_id = message.id();
        ctx.bot.edit_message_text(Self::get_chat_id(message), message_id, buy_text)
            .parse_mode(ParseMode::Html)
            .reply_markup(Self::create_payment_keyboard())
            .await?;
        
        ctx.bot.answer_callback_query(&query.id).await?;
        Ok(())
    }

    async fn handle_group_selection_callback(
        ctx: BotContext,
        message: &MaybeInaccessibleMessage,
        query: &CallbackQuery,
        callback_data: &str,
    ) -> ResponseResult<()> {
        let user_id = query.from.id.0 as i64;
        
        // verify user is in correct state
        let current_state = ctx.session_manager.get_session(user_id).await;
        if !matches!(current_state, SessionState::GroupAnalysisSelectingGroup) {
            ctx.bot.answer_callback_query(&query.id)
                .text("❌ Invalid session state")
                .await?;
            return Ok(());
        }
        
        // parse group ID from callback data
        if let Some(chat_id_str) = callback_data.strip_prefix("select_group_") {
            if let Ok(chat_id) = chat_id_str.parse::<i64>() {
                // get group name
                let group_name = match ctx.group_handler.get_group_name(chat_id).await {
                    Ok(Some(name)) => name,
                    _ => format!("Group {}", chat_id),
                };
                
                // set session to selecting analysis type
                ctx.session_manager.set_session(
                    user_id, 
                    SessionState::GroupAnalysisSelectingType { chat_id, group_name: group_name.clone() }
                ).await;
                
                // create analysis type selection keyboard
                let keyboard = InlineKeyboardMarkup::new(vec![
                    vec![InlineKeyboardButton::callback("💼 Professional Analysis", 
                        format!("group_analysis_professional_{}", chat_id))],
                    vec![InlineKeyboardButton::callback("🧠 Personal Analysis", 
                        format!("group_analysis_personal_{}", chat_id))],
                    vec![InlineKeyboardButton::callback("🔥 Roast Analysis", 
                        format!("group_analysis_roast_{}", chat_id))],
                ]);
                
                let analysis_text = format!(
                    "🎭 <b>Group: {}</b>\n\n\
                    Choose the type of analysis you want to perform:\n\n\
                    💼 <b>Professional:</b> Expert assessment for hiring\n\
                    🧠 <b>Personal:</b> Psychological profile insights\n\
                    🔥 <b>Roast:</b> Fun, brutally honest critique\n\n\
                    <i>Cost: 1 credit per analysis</i>",
                    crate::utils::MessageFormatter::escape_html(&group_name)
                );
                
                let message_id = message.id();
                ctx.bot.edit_message_text(Self::get_chat_id(message), message_id, analysis_text)
                    .parse_mode(ParseMode::Html)
                    .reply_markup(keyboard)
                    .await?;
            }
        }
        
        ctx.bot.answer_callback_query(&query.id).await?;
        Ok(())
    }

    async fn handle_group_analysis_type_callback(
        ctx: BotContext,
        message: &MaybeInaccessibleMessage,
        query: &CallbackQuery,
        callback_data: &str,
    ) -> ResponseResult<()> {
        let user_id = query.from.id.0 as i64;
        
        // verify user is in correct state and extract chat_id
        let (chat_id, group_name) = match ctx.session_manager.get_session(user_id).await {
            SessionState::GroupAnalysisSelectingType { chat_id, group_name } => (chat_id, group_name),
            _ => {
                ctx.bot.answer_callback_query(&query.id)
                    .text("❌ Invalid session state")
                    .await?;
                return Ok(());
            }
        };
        
        // parse analysis type
        let analysis_type = if callback_data.contains("_professional_") {
            "professional"
        } else if callback_data.contains("_personal_") {
            "personal"
        } else if callback_data.contains("_roast_") {
            "roast"
        } else {
            ctx.bot.answer_callback_query(&query.id)
                .text("❌ Invalid analysis type")
                .await?;
            return Ok(());
        };
        
        // get analyzed users from the group analysis
        let available_users = match ctx.group_handler.get_available_analyses(chat_id).await {
            Ok(Some(analysis)) => analysis.analyzed_users,
            Ok(None) => {
                ctx.session_manager.clear_session(user_id).await;
                ctx.bot.answer_callback_query(&query.id)
                    .text("❌ No analysis available for this group")
                    .await?;
                return Ok(());
            }
            Err(e) => {
                error!("Failed to get group analysis: {}", e);
                ctx.session_manager.clear_session(user_id).await;
                ctx.bot.answer_callback_query(&query.id)
                    .text("❌ Error accessing group analysis")
                    .await?;
                return Ok(());
            }
        };
        
        // set session to selecting user
        ctx.session_manager.set_session(
            user_id,
            SessionState::GroupAnalysisSelectingUser {
                chat_id,
                group_name: group_name.clone(),
                analysis_type: analysis_type.to_string(),
                available_users: available_users.clone(),
            }
        ).await;
        
        // create user selection keyboard
        let mut keyboard = Vec::new();
        for user in available_users.iter().take(10) { // limit to 10 users
            let display_name = if let Some(username) = &user.username {
                format!("@{} ({} msgs)", username, user.message_count)
            } else if let Some(first_name) = &user.first_name {
                format!("{} ({} msgs)", first_name, user.message_count)
            } else {
                format!("User {} ({} msgs)", user.telegram_user_id, user.message_count)
            };
            
            keyboard.push(vec![InlineKeyboardButton::callback(
                display_name,
                format!("group_user_{}_{}", analysis_type, user.telegram_user_id)
            )]);
        }
        
        let user_keyboard = InlineKeyboardMarkup::new(keyboard);
        
        let user_text = format!(
            "👥 <b>Select User to Analyze</b>\n\n\
            Group: <b>{}</b>\n\
            Analysis: <b>{}</b>\n\n\
            Choose which member you want to analyze:",
            crate::utils::MessageFormatter::escape_html(&group_name),
            analysis_type.chars().next().unwrap().to_uppercase().collect::<String>() + &analysis_type[1..]
        );
        
        let message_id = message.id();
        ctx.bot.edit_message_text(Self::get_chat_id(message), message_id, user_text)
            .parse_mode(ParseMode::Html)
            .reply_markup(user_keyboard)
            .await?;
        
        ctx.bot.answer_callback_query(&query.id).await?;
        Ok(())
    }

    async fn handle_group_user_selection_callback(
        ctx: BotContext,
        message: &MaybeInaccessibleMessage,
        query: &CallbackQuery,
        callback_data: &str,
    ) -> ResponseResult<()> {
        let user_id = query.from.id.0 as i64;
        
        // verify user is in correct state
        let (chat_id, _group_name, analysis_type, available_users) = match ctx.session_manager.get_session(user_id).await {
            SessionState::GroupAnalysisSelectingUser { chat_id, group_name, analysis_type, available_users } => 
                (chat_id, group_name, analysis_type, available_users),
            _ => {
                ctx.bot.answer_callback_query(&query.id)
                    .text("❌ Invalid session state")
                    .await?;
                return Ok(());
            }
        };
        
        // parse user ID from callback data
        let parts: Vec<&str> = callback_data.split('_').collect();
        if parts.len() < 4 {
            ctx.bot.answer_callback_query(&query.id)
                .text("❌ Invalid callback data")
                .await?;
            return Ok(());
        }
        
        let target_user_id = match parts[3].parse::<i64>() {
            Ok(id) => id,
            Err(_) => {
                ctx.bot.answer_callback_query(&query.id)
                    .text("❌ Invalid user ID")
                    .await?;
                return Ok(());
            }
        };
        
        // find the selected user
        let selected_user = available_users.iter()
            .find(|u| u.telegram_user_id == target_user_id);
        
        let selected_user = match selected_user {
            Some(user) => user,
            None => {
                ctx.bot.answer_callback_query(&query.id)
                    .text("❌ User not found")
                    .await?;
                return Ok(());
            }
        };
        
        // clear session - analysis is starting
        ctx.session_manager.clear_session(user_id).await;
        
        // get or create user and check credits
        let (user_data, _) = match ctx.user_manager.get_or_create_user(
            user_id,
            query.from.username.as_deref(),
            Some(&query.from.first_name),
            query.from.last_name.as_deref(),
            None,
            query.from.language_code.as_deref(),
        ).await {
            Ok(result) => result,
            Err(e) => {
                error!("Failed to get/create user: {}", e);
                ctx.bot.answer_callback_query(&query.id)
                    .text("❌ Error processing request")
                    .await?;
                return Ok(());
            }
        };

        // check if user has credits
        if user_data.analysis_credits <= 0 {
            ctx.bot.answer_callback_query(&query.id)
                .text("❌ No credits available. Please purchase credits first.")
                .await?;
            return Ok(());
        }

        // send analysis results for the selected user and analysis type
        if let Err(e) = Self::send_single_group_analysis_result(
            &ctx, Self::get_chat_id(message), chat_id, &analysis_type, selected_user, user_data
        ).await {
            error!("Failed to send group analysis result: {}", e);
            ctx.bot.answer_callback_query(&query.id)
                .text("❌ Failed to send analysis")
                .await?;
            return Ok(());
        }
        
        ctx.bot.answer_callback_query(&query.id)
            .text("✅ Analysis sent!")
            .await?;
        Ok(())
    }

    async fn handle_channel_analysis_type_callback(
        ctx: BotContext,
        message: &MaybeInaccessibleMessage,
        query: &CallbackQuery,
        callback_data: &str,
    ) -> ResponseResult<()> {
        let user_id = query.from.id.0 as i64;
        
        // verify user is in correct state and extract channel name
        let channel_name = match ctx.session_manager.get_session(user_id).await {
            SessionState::ChannelAnalysisSelectingType { channel_name } => channel_name,
            _ => {
                ctx.bot.answer_callback_query(&query.id)
                    .text("❌ Invalid session state")
                    .await?;
                return Ok(());
            }
        };
        
        // parse analysis type
        let analysis_type = if callback_data.contains("_professional") {
            "professional"
        } else if callback_data.contains("_personal") {
            "personal"
        } else if callback_data.contains("_roast") {
            "roast"
        } else {
            ctx.bot.answer_callback_query(&query.id)
                .text("❌ Invalid analysis type")
                .await?;
            return Ok(());
        };
        
        // clear session - analysis is starting
        ctx.session_manager.clear_session(user_id).await;
        
        // get or create user and check credits
        let (user_data, _) = match ctx.user_manager.get_or_create_user(
            user_id,
            query.from.username.as_deref(),
            Some(&query.from.first_name),
            query.from.last_name.as_deref(),
            None,
            query.from.language_code.as_deref(),
        ).await {
            Ok(result) => result,
            Err(e) => {
                error!("Failed to get/create user: {}", e);
                ctx.bot.answer_callback_query(&query.id)
                    .text("❌ Error processing request")
                    .await?;
                return Ok(());
            }
        };

        // check if user has credits
        if user_data.analysis_credits <= 0 {
            ctx.bot.answer_callback_query(&query.id)
                .text("❌ No credits available. Please purchase credits first.")
                .await?;
            return Ok(());
        }

        // create pending analysis
        let analysis_id = match ctx.user_manager.create_pending_analysis(
            user_data.id,
            &channel_name,
            analysis_type,
        ).await {
            Ok(id) => id,
            Err(e) => {
                error!("Failed to create pending analysis: {}", e);
                ctx.bot.answer_callback_query(&query.id)
                    .text("❌ Error creating analysis")
                    .await?;
                return Ok(());
            }
        };

        // start analysis in background
        Self::start_analysis_in_background(
            ctx.clone(),
            Self::get_chat_id(message),
            channel_name.clone(),
            analysis_type.to_string(),
            user_data,
            analysis_id,
        ).await;
        
        ctx.bot.answer_callback_query(&query.id)
            .text("✅ Analysis started!")
            .await?;
        Ok(())
    }

    async fn send_single_group_analysis_result(
        ctx: &BotContext,
        chat_id: ChatId,
        group_chat_id: i64,
        analysis_type: &str,
        selected_user: &crate::handlers::group_handler::GroupUser,
        user_data: crate::user_manager::User,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // get analysis info for tracking and timestamp
        let (analysis, group_analysis_id) = match ctx.group_handler.get_available_analyses_with_id(group_chat_id).await? {
            Some((analysis, id)) => (analysis, id),
            None => return Err("No analysis available for this group".into()),
        };

        // get individual user analysis content
        let analysis_content = match ctx.group_handler.get_individual_user_analysis(
            group_chat_id,
            selected_user.telegram_user_id,
            analysis_type,
        ).await? {
            Some(content) if !content.is_empty() => content,
            _ => return Err("No individual analysis available for this user and analysis type".into()),
        };

        // consume credit
        ctx.user_manager.consume_credit_for_group_analysis(user_data.id).await?;

        // record analysis access
        if let Err(e) = ctx.user_manager.record_group_analysis_access(
            user_data.id,
            group_analysis_id,
            analysis_type,
            selected_user.telegram_user_id,
        ).await {
            warn!("Failed to record group analysis access: {}", e);
        }

        // get user display name
        let user_display = if let Some(username) = &selected_user.username {
            format!("@{}", username)
        } else if let Some(first_name) = &selected_user.first_name {
            first_name.clone()
        } else {
            format!("User {}", selected_user.telegram_user_id)
        };

        // get group name
        let group_name = match ctx.group_handler.get_group_name(group_chat_id).await {
            Ok(Some(name)) => name,
            _ => format!("Group {}", group_chat_id),
        };

        let analysis_emoji = match analysis_type {
            "professional" => "💼",
            "personal" => "🧠",
            "roast" => "🔥",
            _ => "🔍",
        };

        // send analysis result
        let result_msg = format!(
            "{} <b>{} Analysis for {}</b>\n\n\
            📊 <b>Group:</b> {}\n\
            👤 <b>User:</b> {} ({} messages)\n\
            📅 <b>Analysis Date:</b> {}\n\n\
            {}",
            analysis_emoji,
            analysis_type.chars().next().unwrap().to_uppercase().collect::<String>() + &analysis_type[1..],
            user_display,
            crate::utils::MessageFormatter::escape_html(&group_name),
            user_display,
            selected_user.message_count,
            analysis.analysis_timestamp.format("%Y-%m-%d %H:%M UTC"),
            crate::utils::MessageFormatter::escape_html(&analysis_content)
        );

        ctx.bot.send_message(chat_id, result_msg)
            .parse_mode(ParseMode::Html)
            .await?;

        Ok(())
    }
}