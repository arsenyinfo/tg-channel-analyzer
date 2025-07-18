use log::{error, info};
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{
    ChatId, LabeledPrice, ParseMode, PreCheckoutQuery, SuccessfulPayment,
};

use crate::user_manager::UserManager;

// payment configuration constants
pub const SINGLE_PACKAGE_PRICE: u32 = 40;
pub const BULK_PACKAGE_PRICE: u32 = 200;
pub const SINGLE_PACKAGE_AMOUNT: i32 = 1;
pub const BULK_PACKAGE_AMOUNT: i32 = 10;

#[derive(Clone)]
pub struct PaymentHandler {
    user_manager: Arc<UserManager>,
}

impl PaymentHandler {
    pub fn new(user_manager: Arc<UserManager>) -> Self {
        Self { user_manager }
    }

    pub async fn send_payment_invoice(
        bot: Arc<Bot>,
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

    pub async fn handle_pre_checkout_query(
        bot: Arc<Bot>,
        query: PreCheckoutQuery,
    ) -> ResponseResult<()> {
        // approve all pre-checkout queries for digital goods
        // in a real implementation, you might want to add additional validation
        bot.answer_pre_checkout_query(query.id, true).await?;
        info!(
            "Approved pre-checkout query for {} stars",
            query.total_amount
        );
        Ok(())
    }

    pub async fn handle_successful_payment(
        &self,
        bot: Arc<Bot>,
        msg: Message,
        payment: SuccessfulPayment,
    ) -> ResponseResult<()> {
        let telegram_user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
        let language_code = msg.from.as_ref().and_then(|u| u.language_code.as_deref());

        // get user info for referral link
        let (user, _) = match self.user_manager
            .get_or_create_user(telegram_user_id, None, None, None, None, language_code)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!("Failed to get user info during payment: {}", e);
                bot.send_message(msg.chat.id, "❌ Error processing payment. Please contact support.")
                    .await?;
                return Ok(());
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
        match self.user_manager.add_credits(user.id, credits).await {
            Ok(new_balance) => {
                let success_msg = format!(
                    "🎉 <b>Payment Successful!</b> - <a href=\"https://t.me/ScratchAuthorEgoBot?start={}\">@ScratchAuthorEgoBot</a>\n\n\
                    ✅ Added {} credits to your account\n\
                    💳 New balance: {} credits\n\n\
                    You can now analyze channels by sending me a channel username like <code>@channelname</code>",
                    user.id,
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
                if let Err(e) = self.process_referral_rewards(bot, user.id).await {
                    error!("Failed to process referral rewards for user {}: {}", user.id, e);
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

    async fn process_referral_rewards(
        &self,
        bot: Arc<Bot>,
        user_id: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match self.user_manager.record_paid_referral(user_id).await {
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
                error!("Failed to process paid referral for user {}: {}", user_id, e);
            }
        }
        Ok(())
    }
}