use log::{error, info};
use teloxide::prelude::*;
use teloxide::types::{ChatId, ParseMode, InlineKeyboardButton, InlineKeyboardMarkup};

use crate::bot::{BotContext, Command};
use crate::handlers::{PaymentHandler, CallbackHandler, payment_handler::{SINGLE_PACKAGE_PRICE, BULK_PACKAGE_PRICE, SINGLE_PACKAGE_AMOUNT, BULK_PACKAGE_AMOUNT}};

#[derive(Debug)]
struct UserInfo<'a> {
    telegram_user_id: i64,
    username: Option<&'a str>,
    first_name: Option<&'a str>,
    last_name: Option<&'a str>,
    language_code: Option<&'a str>,
}

pub struct CommandHandler;

impl CommandHandler {
    pub async fn handle_command(
        ctx: BotContext,
        msg: Message,
        cmd: Command,
    ) -> ResponseResult<()> {
        match cmd {
            Command::Start => {
                Self::handle_start_command(ctx, msg).await?;
            }
            Command::Buy1 => {
                Self::handle_buy_command(ctx, msg, SINGLE_PACKAGE_AMOUNT, SINGLE_PACKAGE_PRICE, "1 Channel Analysis", "Get 1 analysis credit to analyze any Telegram channel").await?;
            }
            Command::Buy10 => {
                Self::handle_buy_command(
                    ctx, 
                    msg, 
                    BULK_PACKAGE_AMOUNT, 
                    BULK_PACKAGE_PRICE, 
                    "10 Channel Analyses", 
                    &format!("Get 10 analysis credits to analyze any Telegram channels ({} stars discount!)",
                        (SINGLE_PACKAGE_PRICE * BULK_PACKAGE_AMOUNT as u32) - BULK_PACKAGE_PRICE)
                ).await?;
            }
        }
        Ok(())
    }

    async fn handle_start_command(ctx: BotContext, msg: Message) -> ResponseResult<()> {
        // parse referral code from message text
        let referrer_user_id = Self::parse_referral_code(&ctx, &msg).await;

        // get user info from telegram message
        let user_info = Self::extract_user_info_from_message(&msg);

        // get or create user to check credit balance
        let (user, maybe_reward_info) = match ctx.user_manager
            .get_or_create_user(
                user_info.telegram_user_id, 
                user_info.username, 
                user_info.first_name, 
                user_info.last_name, 
                referrer_user_id, 
                user_info.language_code
            )
            .await
        {
            Ok((user, reward_info)) => (user, reward_info),
            Err(e) => {
                error!("Failed to get/create user: {}", e);
                ctx.bot.send_message(msg.chat.id, "❌ Sorry, there was an error accessing your account. Please try again later.")
                    .await?;
                return Ok(());
            }
        };

        // send referral milestone notification if applicable
        Self::send_referral_notifications(&ctx, maybe_reward_info).await;

        // check for available group analyses
        let available_groups = Self::get_user_group_analyses(&ctx, user_info.telegram_user_id).await;

        // send welcome message with main menu
        Self::send_welcome_with_menu(&ctx, &msg, &user, !available_groups.is_empty()).await?;

        Ok(())
    }

    async fn parse_referral_code(ctx: &BotContext, msg: &Message) -> Option<i32> {
        if let Some(text) = msg.text() {
            info!("Processing /start command with text: {}", text);
            if let Some(args) = text.strip_prefix("/start ") {
                info!("Found referral code in /start command: {}", args);
                if let Ok(user_id) = args.trim().parse::<i32>() {
                    info!("Parsed referrer user ID: {}", user_id);
                    // validate that referrer exists
                    match ctx.user_manager.validate_referrer(user_id).await {
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
        }
    }

    fn extract_user_info_from_message(msg: &Message) -> UserInfo {
        UserInfo {
            telegram_user_id: msg.from.as_ref().map(|user| user.id.0 as i64).unwrap_or(0),
            username: msg.from.as_ref().and_then(|user| user.username.as_deref()),
            first_name: msg.from.as_ref().map(|user| user.first_name.as_str()),
            last_name: msg.from.as_ref().and_then(|user| user.last_name.as_deref()),
            language_code: msg.from.as_ref().and_then(|user| user.language_code.as_deref()),
        }
    }

    async fn send_referral_notifications(ctx: &BotContext, maybe_reward_info: Option<crate::user_manager::ReferralRewardInfo>) {
        if let Some(reward_info) = maybe_reward_info {
            info!("Received reward info for referral: referral_count={}, milestone_rewards={}, paid_rewards={}, is_celebration={}, referrer_telegram_id={:?}", 
                  reward_info.referral_count, reward_info.milestone_rewards, reward_info.paid_rewards, 
                  reward_info.is_celebration_milestone, reward_info.referrer_telegram_id);
            
            if let Some(referrer_telegram_id) = reward_info.referrer_telegram_id {
                let reward_msg = Self::build_referral_message(&reward_info);

                if !reward_msg.is_empty() {
                    info!("Sending referral notification to telegram user {}: {}", referrer_telegram_id, reward_msg.replace("\n", " "));
                    match ctx.bot.send_message(
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
    }

    fn build_referral_message(reward_info: &crate::user_manager::ReferralRewardInfo) -> String {
        if reward_info.is_celebration_milestone && reward_info.total_credits_awarded > 0 {
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
        }
    }

    #[allow(dead_code)]
    async fn send_no_credits_welcome(ctx: &BotContext, msg: &Message, user: &crate::user_manager::User) -> ResponseResult<()> {
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

        ctx.bot.send_message(msg.chat.id, intro_text)
            .parse_mode(ParseMode::Html)
            .reply_markup(CallbackHandler::create_payment_keyboard())
            .await?;

        Ok(())
    }

    async fn get_user_group_analyses(ctx: &BotContext, telegram_user_id: i64) -> Vec<(i64, String)> {
        match ctx.group_handler.get_user_groups(telegram_user_id).await {
            Ok(chat_ids) => {
                let mut available_groups = Vec::new();
                for chat_id in chat_ids {
                    if let Ok(Some(analysis)) = ctx.group_handler.get_available_analyses(chat_id).await {
                        if !analysis.analyzed_users.is_empty() {
                            // get real group name from database
                            let group_name = match ctx.group_handler.get_group_name(chat_id).await {
                                Ok(Some(name)) => name,
                                _ => format!("Group {}", chat_id), // fallback to ID
                            };
                            available_groups.push((chat_id, group_name));
                        }
                    }
                }
                available_groups
            },
            Err(e) => {
                error!("Failed to get user groups for {}: {}", telegram_user_id, e);
                Vec::new()
            }
        }
    }

    #[allow(dead_code)]
    async fn send_credits_available_welcome(ctx: &BotContext, msg: &Message, user: &crate::user_manager::User, available_groups: &[(i64, String)]) -> ResponseResult<()> {
        let referral_section = Self::build_referral_section(user);

        let group_analysis_section = if !available_groups.is_empty() {
            let group_list = available_groups.iter()
                .take(3)
                .map(|(_, name)| format!("• {}", name))
                .collect::<Vec<_>>()
                .join("\n");
            
            let additional_groups = if available_groups.len() > 3 {
                format!(" and {} more", available_groups.len() - 3)
            } else {
                String::new()
            };

            format!(
                "🎭 <b>Group Analysis Available!</b>\n\
                You have access to group analyses{} for 1 credit each:\n\
                {}\n\n\
                Send me a group ID to access the analysis!\n\n",
                additional_groups,
                group_list
            )
        } else {
            String::new()
        };

        let intro_text = format!(
            "🤖 <b><a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">@ScratchAuthorEgoBot</a> - Channel Analyzer</b>\n\n\
            Welcome back! I can analyze Telegram channels and provide insights.\n\n\
            {}📋 <b>How to use:</b>\n\
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
            group_analysis_section,
            referral_section
        );

        ctx.bot.send_message(msg.chat.id, intro_text)
            .parse_mode(ParseMode::Html)
            .await?;

        Ok(())
    }

    fn create_main_menu_keyboard(has_group_analyses: bool) -> InlineKeyboardMarkup {
        let mut keyboard = vec![
            vec![InlineKeyboardButton::callback("📊 Analyze Channel", "menu_channels")],
        ];
        
        if has_group_analyses {
            keyboard.push(vec![InlineKeyboardButton::callback("🎭 View Group Analysis", "menu_groups")]);
        }
        
        keyboard.push(vec![InlineKeyboardButton::callback("💰 Buy Credits", "menu_buy")]);
        
        InlineKeyboardMarkup::new(keyboard)
    }

    async fn send_welcome_with_menu(ctx: &BotContext, msg: &Message, user: &crate::user_manager::User, has_group_analyses: bool) -> ResponseResult<()> {
        let referral_section = Self::build_referral_section(user);
        
        let group_status = if has_group_analyses {
            "✅ You have group analyses available!\n\n"
        } else {
            ""
        };

        let intro_text = format!(
            "🤖 <b><a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">@ScratchAuthorEgoBot</a> - Channel Analyzer</b>\n\n\
            Welcome! I can analyze Telegram channels and group chats.\n\n\
            {}⚡ <b>Analysis Types:</b>\n\
            • 💼 Professional: Expert assessment for hiring\n\
            • 🧠 Personal: Psychological profile insights\n\
            • 🔥 Roast: Fun, brutally honest critique\n\n\
            {}Choose an option below to get started!",
            user.id,
            group_status,
            referral_section
        );

        ctx.bot.send_message(msg.chat.id, intro_text)
            .parse_mode(ParseMode::Html)
            .reply_markup(Self::create_main_menu_keyboard(has_group_analyses))
            .await?;

        Ok(())
    }

    fn build_referral_section(user: &crate::user_manager::User) -> String {
        if user.referrals_count > 0 {
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
        }
    }

    async fn handle_buy_command(
        ctx: BotContext,
        msg: Message,
        credits: i32,
        stars: u32,
        title: &str,
        description: &str,
    ) -> ResponseResult<()> {
        PaymentHandler::send_payment_invoice(
            ctx.bot,
            msg.chat.id,
            credits,
            stars,
            title,
            description,
        )
        .await?;
        Ok(())
    }
}