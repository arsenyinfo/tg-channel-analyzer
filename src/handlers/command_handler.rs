use log::{error, info};
use teloxide::prelude::*;
use teloxide::types::{ChatId, ParseMode};

use crate::bot::{BotContext, Command};
use crate::handlers::{
    payment_handler::{
        BULK_PACKAGE_AMOUNT, BULK_PACKAGE_PRICE, SINGLE_PACKAGE_AMOUNT, SINGLE_PACKAGE_PRICE,
    },
    CallbackHandler, PaymentHandler,
};
use crate::localization::Lang;

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
    pub async fn handle_command(ctx: BotContext, msg: Message, cmd: Command) -> ResponseResult<()> {
        let lang = Lang::from_code(
            msg.from
                .as_ref()
                .and_then(|user| user.language_code.as_deref()),
        );

        match cmd {
            Command::Start => {
                Self::handle_start_command(ctx, msg, lang).await?;
            }
            Command::Buy1 => {
                Self::handle_buy_command(
                    ctx,
                    msg,
                    SINGLE_PACKAGE_AMOUNT,
                    SINGLE_PACKAGE_PRICE,
                    lang.invoice_single_title(),
                    lang.invoice_single_description(),
                )
                .await?;
            }
            Command::Buy10 => {
                let discount =
                    (SINGLE_PACKAGE_PRICE * BULK_PACKAGE_AMOUNT as u32) - BULK_PACKAGE_PRICE;
                Self::handle_buy_command(
                    ctx,
                    msg,
                    BULK_PACKAGE_AMOUNT,
                    BULK_PACKAGE_PRICE,
                    lang.invoice_bulk_title(),
                    &lang.invoice_bulk_description(discount),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn handle_start_command(
        ctx: BotContext,
        msg: Message,
        lang: Lang,
    ) -> ResponseResult<()> {
        // parse referral code from message text
        let referrer_user_id = Self::parse_referral_code(&ctx, &msg).await;

        // get user info from telegram message
        let user_info = Self::extract_user_info_from_message(&msg);

        // get or create user to check credit balance
        let (user, maybe_reward_info) = match ctx
            .user_manager
            .get_or_create_user(
                user_info.telegram_user_id,
                user_info.username,
                user_info.first_name,
                user_info.last_name,
                referrer_user_id,
                user_info.language_code,
            )
            .await
        {
            Ok((user, reward_info)) => (user, reward_info),
            Err(e) => {
                error!("Failed to get/create user: {}", e);
                ctx.bot
                    .send_message(msg.chat.id, lang.error_account_access())
                    .await?;
                return Ok(());
            }
        };

        // send referral milestone notification if applicable
        Self::send_referral_notifications(&ctx, maybe_reward_info, lang).await;

        // send appropriate welcome message based on user's credit balance
        if user.analysis_credits <= 0 {
            Self::send_no_credits_welcome(&ctx, &msg, &user, lang).await?;
        } else {
            Self::send_credits_available_welcome(&ctx, &msg, &user, lang).await?;
        }

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
            language_code: msg
                .from
                .as_ref()
                .and_then(|user| user.language_code.as_deref()),
        }
    }

    async fn send_referral_notifications(
        ctx: &BotContext,
        maybe_reward_info: Option<crate::user_manager::ReferralRewardInfo>,
        lang: Lang,
    ) {
        if let Some(reward_info) = maybe_reward_info {
            info!("Received reward info for referral: referral_count={}, milestone_rewards={}, paid_rewards={}, is_celebration={}, referrer_telegram_id={:?}",
                  reward_info.referral_count, reward_info.milestone_rewards, reward_info.paid_rewards,
                  reward_info.is_celebration_milestone, reward_info.referrer_telegram_id);

            if let Some(referrer_telegram_id) = reward_info.referrer_telegram_id {
                let reward_msg = Self::build_referral_message(&reward_info, lang);

                if !reward_msg.is_empty() {
                    info!(
                        "Sending referral notification to telegram user {}: {}",
                        referrer_telegram_id,
                        reward_msg.replace("\n", " ")
                    );
                    match ctx
                        .bot
                        .send_message(ChatId(referrer_telegram_id), reward_msg)
                        .parse_mode(ParseMode::Html)
                        .await
                    {
                        Ok(_) => info!(
                            "Successfully sent referral notification to telegram user {}",
                            referrer_telegram_id
                        ),
                        Err(e) => error!(
                            "Failed to send referral notification to telegram user {}: {}",
                            referrer_telegram_id, e
                        ),
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

    fn build_referral_message(
        reward_info: &crate::user_manager::ReferralRewardInfo,
        lang: Lang,
    ) -> String {
        let referrer_user_id = reward_info.referrer_user_id.unwrap_or(0);

        if reward_info.is_celebration_milestone && reward_info.total_credits_awarded > 0 {
            lang.referral_milestone_with_credits(
                reward_info.referral_count,
                reward_info.total_credits_awarded,
                referrer_user_id,
            )
        } else if reward_info.is_celebration_milestone {
            lang.referral_milestone_no_credits(reward_info.referral_count, referrer_user_id)
        } else if reward_info.total_credits_awarded > 0 {
            lang.referral_reward(
                reward_info.total_credits_awarded,
                reward_info.referral_count,
                referrer_user_id,
            )
        } else {
            String::new()
        }
    }

    async fn send_no_credits_welcome(
        ctx: &BotContext,
        msg: &Message,
        user: &crate::user_manager::User,
        lang: Lang,
    ) -> ResponseResult<()> {
        let referral_info = if user.referrals_count > 0 {
            lang.referral_info_has_referrals(user.referrals_count)
        } else {
            lang.referral_info_no_referrals().to_string()
        };

        let bulk_discount =
            (SINGLE_PACKAGE_PRICE * BULK_PACKAGE_AMOUNT as u32) - BULK_PACKAGE_PRICE;

        let intro_text = lang.welcome_no_credits(
            user.id,
            SINGLE_PACKAGE_PRICE,
            BULK_PACKAGE_PRICE,
            bulk_discount,
            &referral_info,
        );

        ctx.bot
            .send_message(msg.chat.id, intro_text)
            .parse_mode(ParseMode::Html)
            .reply_markup(CallbackHandler::create_payment_keyboard(lang))
            .await?;

        Ok(())
    }

    async fn send_credits_available_welcome(
        ctx: &BotContext,
        msg: &Message,
        user: &crate::user_manager::User,
        lang: Lang,
    ) -> ResponseResult<()> {
        let referral_section = Self::build_referral_section(user, lang);

        let intro_text = lang.welcome_with_credits(user.id, &referral_section);

        ctx.bot
            .send_message(msg.chat.id, intro_text)
            .parse_mode(ParseMode::Html)
            .await?;

        Ok(())
    }

    fn build_referral_section(user: &crate::user_manager::User, lang: Lang) -> String {
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
            lang.referral_section_with_referrals(
                user.analysis_credits,
                user.total_analyses_performed,
                user.referrals_count,
                user.paid_referrals_count,
                referrals_to_next,
                user.id,
            )
        } else {
            lang.referral_section_no_referrals(
                user.analysis_credits,
                user.total_analyses_performed,
                user.id,
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
