use log::{error, info};
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{ChatId, LabeledPrice, ParseMode, PreCheckoutQuery, SuccessfulPayment};

use crate::localization::Lang;
use crate::user_manager::UserManager;

// payment configuration constants
pub const SINGLE_PACKAGE_PRICE: u32 = 100;
pub const BULK_PACKAGE_PRICE: u32 = 500;
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
        // use Lang::En for the label since it's internal and not user-facing
        let lang = Lang::En;
        let prices = vec![LabeledPrice {
            label: lang.credits_label(credits),
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
        let lang = Lang::from_code(language_code);

        // get user info for referral link
        let (user, _) = match self
            .user_manager
            .get_or_create_user(telegram_user_id, None, None, None, None, language_code)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!("Failed to get user info during payment: {}", e);
                bot.send_message(msg.chat.id, lang.error_payment_processing())
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
                let success_msg = lang.payment_success(user.id, credits, new_balance);

                bot.send_message(msg.chat.id, success_msg)
                    .parse_mode(ParseMode::Html)
                    .await?;

                info!(
                    "Successfully processed payment: {} credits for user {}",
                    credits, telegram_user_id
                );

                // process referral rewards if user was referred
                if let Err(e) = self.process_referral_rewards(bot, user.id, lang).await {
                    error!(
                        "Failed to process referral rewards for user {}: {}",
                        user.id, e
                    );
                }
            }
            Err(e) => {
                error!(
                    "Failed to add credits after payment for user {}: {}",
                    telegram_user_id, e
                );
                bot.send_message(msg.chat.id, lang.error_payment_credits())
                    .await?;
            }
        }

        Ok(())
    }

    async fn process_referral_rewards(
        &self,
        bot: Arc<Bot>,
        user_id: i32,
        lang: Lang,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match self.user_manager.record_paid_referral(user_id).await {
            Ok(Some(reward_info)) => {
                if let Some(referrer_telegram_id) = reward_info.referrer_telegram_id {
                    let referrer_user_id = reward_info.referrer_user_id.unwrap_or(0);

                    // send notification to referrer
                    let reward_msg = if reward_info.paid_rewards > 0
                        && reward_info.milestone_rewards > 0
                    {
                        lang.referral_paid_and_milestone(
                            reward_info.total_credits_awarded,
                            reward_info.referral_count,
                            reward_info.paid_rewards,
                            reward_info.milestone_rewards,
                            referrer_user_id,
                        )
                    } else if reward_info.paid_rewards > 0 {
                        lang.referral_paid_only(
                            reward_info.paid_rewards,
                            reward_info.referral_count,
                            referrer_user_id,
                        )
                    } else if reward_info.milestone_rewards > 0 {
                        lang.referral_milestone_only(
                            reward_info.milestone_rewards,
                            reward_info.referral_count,
                            referrer_user_id,
                        )
                    } else {
                        String::new()
                    };

                    if !reward_msg.is_empty() {
                        let _ = bot
                            .send_message(ChatId(referrer_telegram_id), reward_msg)
                            .parse_mode(ParseMode::Html)
                            .await;
                    }
                }
            }
            Ok(None) => {
                // no referral rewards
            }
            Err(e) => {
                error!(
                    "Failed to process paid referral for user {}: {}",
                    user_id, e
                );
            }
        }
        Ok(())
    }
}
