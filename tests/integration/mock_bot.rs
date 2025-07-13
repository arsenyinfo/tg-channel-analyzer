use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tg_main::user_manager::{UserManager, ReferralRewardInfo};

/// represents a sent message for verification in tests
#[derive(Debug, Clone)]
pub struct SentMessage {
    pub chat_id: i64,
    pub text: String,
    pub parse_mode: Option<String>,
}

/// mock telegram bot that simulates bot behavior without real API calls
#[derive(Debug, Clone)]
pub struct MockTelegramBot {
    /// stores all sent messages for verification
    pub sent_messages: Arc<Mutex<Vec<SentMessage>>>,
    /// tracks user interactions
    pub user_interactions: Arc<Mutex<HashMap<i64, Vec<String>>>>,
}

impl MockTelegramBot {
    pub fn new() -> Self {
        Self {
            sent_messages: Arc::new(Mutex::new(Vec::new())),
            user_interactions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// simulates sending a message (stores it for verification)
    pub fn send_message(&self, chat_id: i64, text: String, parse_mode: Option<String>) {
        let message = SentMessage {
            chat_id,
            text: text.clone(),
            parse_mode,
        };
        
        self.sent_messages.lock().unwrap().push(message);
        
        // track user interaction
        self.user_interactions
            .lock()
            .unwrap()
            .entry(chat_id)
            .or_insert_with(Vec::new)
            .push(text);
    }

    /// gets all sent messages for verification
    pub fn get_sent_messages(&self) -> Vec<SentMessage> {
        self.sent_messages.lock().unwrap().clone()
    }

    /// gets messages sent to a specific chat
    pub fn get_messages_for_chat(&self, chat_id: i64) -> Vec<SentMessage> {
        self.sent_messages
            .lock()
            .unwrap()
            .iter()
            .filter(|msg| msg.chat_id == chat_id)
            .cloned()
            .collect()
    }

    /// clears all sent messages (useful between tests)
    pub fn clear_messages(&self) {
        self.sent_messages.lock().unwrap().clear();
        self.user_interactions.lock().unwrap().clear();
    }

    /// gets count of messages sent to a specific chat
    pub fn message_count_for_chat(&self, chat_id: i64) -> usize {
        self.get_messages_for_chat(chat_id).len()
    }

    /// checks if any message to chat contains specific text
    pub fn chat_received_message_containing(&self, chat_id: i64, text: &str) -> bool {
        self.get_messages_for_chat(chat_id)
            .iter()
            .any(|msg| msg.text.contains(text))
    }

    /// simulates a user starting the bot (with optional referral)
    pub async fn simulate_user_start(
        &self,
        user_manager: &UserManager,
        telegram_user_id: i64,
        username: Option<&str>,
        first_name: Option<&str>,
        last_name: Option<&str>,
        referrer_user_id: Option<i32>,
    ) -> Result<(tg_main::user_manager::User, Option<ReferralRewardInfo>), Box<dyn std::error::Error + Send + Sync>> {
        // simulate /start command processing with referrer validation (like real bot)
        let validated_referrer = if let Some(referrer_id) = referrer_user_id {
            match user_manager.validate_referrer(referrer_id).await {
                Ok(true) => Some(referrer_id),
                _ => None,
            }
        } else {
            None
        };
        
        let (user, reward_info) = user_manager
            .get_or_create_user(telegram_user_id, username, first_name, last_name, validated_referrer)
            .await?;

        // simulate sending welcome message
        let welcome_msg = if user.analysis_credits > 0 {
            format!("Welcome! You have {} credits", user.analysis_credits)
        } else {
            "Welcome! You need to buy credits".to_string()
        };
        
        self.send_message(telegram_user_id, welcome_msg, Some("Html".to_string()));

        // simulate referral notification if applicable
        if let Some(reward_info) = &reward_info {
            if let Some(referrer_telegram_id) = reward_info.referrer_telegram_id {
                let reward_msg = if reward_info.total_credits_awarded > 0 && reward_info.is_celebration_milestone {
                    format!(
                        "🎉 Referral Milestone! You've reached {} referrals and earned {} credit(s)!",
                        reward_info.referral_count, reward_info.total_credits_awarded
                    )
                } else if reward_info.total_credits_awarded > 0 {
                    format!(
                        "🎉 Referral Reward! You've earned {} credit(s) for reaching {} referrals!",
                        reward_info.total_credits_awarded, reward_info.referral_count
                    )
                } else if reward_info.is_celebration_milestone {
                    format!(
                        "🎊 Referral Milestone! Congratulations! You've reached {} referrals!",
                        reward_info.referral_count
                    )
                } else {
                    String::new()
                };

                if !reward_msg.is_empty() {
                    self.send_message(referrer_telegram_id, reward_msg, Some("Html".to_string()));
                }
            }
        }

        Ok((user, reward_info))
    }

    /// simulates a user making a payment (triggering paid referral logic)
    pub async fn simulate_user_payment(
        &self,
        user_manager: &UserManager,
        telegram_user_id: i64,
        credits: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // add credits to user
        let new_balance = user_manager.add_credits(telegram_user_id, credits).await?;
        
        // simulate payment success message
        let success_msg = format!(
            "🎉 Payment Successful! Added {} credits. New balance: {}",
            credits, new_balance
        );
        self.send_message(telegram_user_id, success_msg, Some("Html".to_string()));

        // process referral rewards for paid user
        if let Some(reward_info) = user_manager.record_paid_referral(telegram_user_id).await? {
            if let Some(referrer_telegram_id) = reward_info.referrer_telegram_id {
                let reward_msg = if reward_info.paid_rewards > 0 && reward_info.milestone_rewards > 0 {
                    format!(
                        "🎉 Referral Rewards! You've earned {} credits: {} for paid referral + {} for milestone bonus",
                        reward_info.total_credits_awarded, reward_info.paid_rewards, reward_info.milestone_rewards
                    )
                } else if reward_info.paid_rewards > 0 {
                    format!(
                        "🎉 Referral Reward! You've earned {} credit(s) for a paid referral!",
                        reward_info.paid_rewards
                    )
                } else if reward_info.milestone_rewards > 0 {
                    format!(
                        "🎉 Milestone Reward! You've earned {} credit(s) for reaching a referral milestone!",
                        reward_info.milestone_rewards
                    )
                } else {
                    String::new()
                };

                if !reward_msg.is_empty() {
                    self.send_message(referrer_telegram_id, reward_msg, Some("Html".to_string()));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_bot_basic_functionality() {
        let bot = MockTelegramBot::new();
        
        // test sending messages
        bot.send_message(123, "Hello".to_string(), None);
        bot.send_message(456, "World".to_string(), Some("Html".to_string()));
        
        // verify messages were stored
        let messages = bot.get_sent_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].chat_id, 123);
        assert_eq!(messages[0].text, "Hello");
        assert_eq!(messages[1].chat_id, 456);
        assert_eq!(messages[1].text, "World");
        assert_eq!(messages[1].parse_mode, Some("Html".to_string()));
        
        // test chat-specific filtering
        let chat_123_messages = bot.get_messages_for_chat(123);
        assert_eq!(chat_123_messages.len(), 1);
        assert_eq!(chat_123_messages[0].text, "Hello");
        
        // test message search
        assert!(bot.chat_received_message_containing(123, "Hello"));
        assert!(!bot.chat_received_message_containing(123, "World"));
        
        // test clearing
        bot.clear_messages();
        assert_eq!(bot.get_sent_messages().len(), 0);
    }
}