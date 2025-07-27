use deadpool_postgres::Pool;
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::sync::Arc;

#[derive(Debug)]
pub enum UserManagerError {
    UserNotFound(i32),        // user_id
    InsufficientCredits(i32), // user_id
    DatabaseError(Box<dyn Error + Send + Sync>),
}

impl fmt::Display for UserManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UserManagerError::UserNotFound(user_id) => {
                write!(f, "User with id {} not found", user_id)
            }
            UserManagerError::InsufficientCredits(user_id) => {
                write!(f, "User with id {} has insufficient credits", user_id)
            }
            UserManagerError::DatabaseError(e) => write!(f, "Database error: {}", e),
        }
    }
}

impl Error for UserManagerError {}

impl From<tokio_postgres::Error> for UserManagerError {
    fn from(err: tokio_postgres::Error) -> Self {
        UserManagerError::DatabaseError(Box::new(err))
    }
}

impl From<deadpool_postgres::PoolError> for UserManagerError {
    fn from(err: deadpool_postgres::PoolError) -> Self {
        UserManagerError::DatabaseError(Box::new(err))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct User {
    pub id: i32,
    pub telegram_user_id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub analysis_credits: i32,
    pub total_analyses_performed: i32,
    pub referred_by_user_id: Option<i32>,
    pub referrals_count: i32,
    pub paid_referrals_count: i32,
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PendingAnalysis {
    pub id: i32,
    pub user_id: i32,
    pub telegram_user_id: i64,  // kept for bot notification purposes
    pub channel_name: String,
    pub analysis_type: String,
}

#[derive(Debug, Clone)]
pub struct ReferralRewardInfo {
    pub milestone_rewards: i32,
    pub paid_rewards: i32,
    pub total_credits_awarded: i32,
    pub referrer_telegram_id: Option<i64>,
    pub referrer_user_id: Option<i32>,
    pub is_celebration_milestone: bool,
    pub referral_count: i32,
}

pub struct UserManager {
    pool: Arc<Pool>,
}

impl UserManager {
    pub fn new(pool: Arc<Pool>) -> Self {
        Self { pool }
    }

    /// calculates how many milestone rewards should be earned for given referral count
    /// rewards are given every 5 referrals: 5, 10, 15, 20, 25, etc.
    fn calculate_milestone_rewards(referral_count: i32) -> i32 {
        referral_count / 5
    }

    /// checks if referral count hits a celebration milestone: 1, 5, 10, 20, 30, 40, 50, etc.
    fn is_celebration_milestone(referral_count: i32) -> bool {
        match referral_count {
            1 | 5 => true,
            n if n >= 10 && n % 10 == 0 => true,
            _ => false,
        }
    }


    /// gets existing user or creates new user with default credits
    pub async fn get_or_create_user(
        &self,
        telegram_user_id: i64,
        username: Option<&str>,
        first_name: Option<&str>,
        last_name: Option<&str>,
        referrer_user_id: Option<i32>,
        language_code: Option<&str>,
    ) -> Result<(User, Option<ReferralRewardInfo>), Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;

        // try to get existing user first
        if let Some(row) = client
            .query_opt(
                "SELECT id, telegram_user_id, username, first_name, last_name, analysis_credits, total_analyses_performed, referred_by_user_id, referrals_count, paid_referrals_count, language 
                 FROM users WHERE telegram_user_id = $1",
                &[&telegram_user_id],
            )
            .await?
        {
            let mut user = User {
                id: row.get(0),
                telegram_user_id: row.get(1),
                username: row.get(2),
                first_name: row.get(3),
                last_name: row.get(4),
                analysis_credits: row.get(5),
                total_analyses_performed: row.get(6),
                referred_by_user_id: row.get(7),
                referrals_count: row.get(8),
                paid_referrals_count: row.get(9),
                language: row.get(10),
            };
            
            // update language if provided and different from stored
            if let Some(lang) = language_code {
                if user.language.as_deref() != Some(lang) {
                    if let Err(e) = client
                        .execute(
                            "UPDATE users SET language = $1, updated_at = NOW() WHERE telegram_user_id = $2",
                            &[&lang, &telegram_user_id],
                        )
                        .await
                    {
                        error!("Failed to update user language: {}", e);
                    } else {
                        user.language = Some(lang.to_string());
                        info!("Updated language for user {} to {}", telegram_user_id, lang);
                    }
                }
            }
            
            info!("Found existing user: {} (credits: {}, language: {:?})", telegram_user_id, user.analysis_credits, user.language);
            return Ok((user, None));
        }

        // create new user with default credits
        let row = client
            .query_one(
                "INSERT INTO users (telegram_user_id, username, first_name, last_name, analysis_credits, total_analyses_performed, referred_by_user_id, referrals_count, paid_referrals_count, language) 
                 VALUES ($1, $2, $3, $4, 1, 0, $5, 0, 0, $6) 
                 RETURNING id, telegram_user_id, username, first_name, last_name, analysis_credits, total_analyses_performed, referred_by_user_id, referrals_count, paid_referrals_count, language",
                &[&telegram_user_id, &username, &first_name, &last_name, &referrer_user_id, &language_code],
            )
            .await?;

        let user = User {
            id: row.get(0),
            telegram_user_id: row.get(1),
            username: row.get(2),
            first_name: row.get(3),
            last_name: row.get(4),
            analysis_credits: row.get(5),
            total_analyses_performed: row.get(6),
            referred_by_user_id: row.get(7),
            referrals_count: row.get(8),
            paid_referrals_count: row.get(9),
            language: row.get(10),
        };

        info!(
            "Created new user: {} with {} credits",
            telegram_user_id, user.analysis_credits
        );

        // if user was referred, increment referrer's count and check for rewards
        if let Some(referrer_id) = referrer_user_id {
            info!("Processing new referral: user {} was referred by user {}", telegram_user_id, referrer_id);
            match self.process_new_referral(referrer_id).await {
                Ok(Some(reward_info)) => {
                    info!("Referral processing successful for referrer {}: {} referrals, {} milestone credits, {} paid credits, celebration: {}", 
                          referrer_id, reward_info.referral_count, reward_info.milestone_rewards, reward_info.paid_rewards, reward_info.is_celebration_milestone);
                    return Ok((user, Some(reward_info)));
                }
                Ok(None) => {
                    info!("Referral processed for referrer {} but no rewards or milestones triggered", referrer_id);
                }
                Err(e) => {
                    error!("Failed to process referral for user {}: {}", referrer_id, e);
                }
            }
        } else {
            info!("New user {} created without referrer", telegram_user_id);
        }

        Ok((user, None))
    }

    /// processes a new referral: increments count and checks for rewards/milestones
    async fn process_new_referral(&self, referrer_user_id: i32) -> Result<Option<ReferralRewardInfo>, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        
        // increment referrals count and get new count
        info!("Incrementing referral count for referrer user {}", referrer_user_id);
        let row = client
            .query_one(
                "UPDATE users SET referrals_count = referrals_count + 1 WHERE id = $1 RETURNING referrals_count, telegram_user_id",
                &[&referrer_user_id],
            )
            .await?;
        
        let new_referral_count: i32 = row.get(0);
        let telegram_user_id: i64 = row.get(1);
        
        info!("Successfully incremented referrals count for user {} (telegram_id: {}) to {}", referrer_user_id, telegram_user_id, new_referral_count);
        
        // check if this is a celebration milestone
        let is_celebration = Self::is_celebration_milestone(new_referral_count);
        info!("Referral milestone check for user {}: count={}, is_celebration={}", referrer_user_id, new_referral_count, is_celebration);
        
        // check for credit rewards (every 5 referrals)
        let expected_milestone_rewards = Self::calculate_milestone_rewards(new_referral_count);
        info!("Expected milestone rewards for {} referrals: {}", new_referral_count, expected_milestone_rewards);
        let existing_unpaid_rewards = client
            .query_one(
                "SELECT COUNT(*) FROM referral_rewards WHERE referrer_user_id = $1 AND reward_type = 'unpaid_milestone'",
                &[&referrer_user_id],
            )
            .await?
            .get::<_, i64>(0) as i32;

        let mut milestone_rewards = 0;
        if expected_milestone_rewards > existing_unpaid_rewards {
            let new_rewards = expected_milestone_rewards - existing_unpaid_rewards;
            milestone_rewards = new_rewards;
            info!("Awarding {} new milestone rewards to user {} (expected: {}, existing: {})", 
                  new_rewards, referrer_user_id, expected_milestone_rewards, existing_unpaid_rewards);
            for i in 0..new_rewards {
                info!("Awarding milestone reward {} of {} to user {}", i+1, new_rewards, referrer_user_id);
                // award 1 credit for milestone
                client
                    .execute(
                        "UPDATE users SET analysis_credits = analysis_credits + 1 WHERE id = $1",
                        &[&referrer_user_id],
                    )
                    .await?;

                // record the reward
                client
                    .execute(
                        "INSERT INTO referral_rewards (referrer_user_id, referee_user_id, reward_type, credits_awarded) VALUES ($1, $1, 'unpaid_milestone', 1)",
                        &[&referrer_user_id],
                    )
                    .await?;
                info!("Successfully awarded milestone reward {} to user {}", i+1, referrer_user_id);
            }
            info!("Completed awarding {} milestone rewards to user {}", new_rewards, referrer_user_id);
        } else {
            info!("No new milestone rewards for user {} (expected: {}, existing: {})", 
                  referrer_user_id, expected_milestone_rewards, existing_unpaid_rewards);
        }

        // return info if there are rewards or if it's a celebration milestone
        if milestone_rewards > 0 || is_celebration {
            info!("Returning reward info for user {}: milestone_rewards={}, is_celebration={}, referral_count={}", 
                  referrer_user_id, milestone_rewards, is_celebration, new_referral_count);
            Ok(Some(ReferralRewardInfo {
                milestone_rewards,
                paid_rewards: 0,
                total_credits_awarded: milestone_rewards,
                referrer_telegram_id: Some(telegram_user_id),
                referrer_user_id: Some(referrer_user_id),
                is_celebration_milestone: is_celebration,
                referral_count: new_referral_count,
            }))
        } else {
            info!("No reward info to return for user {} (milestone_rewards={}, is_celebration={})", 
                  referrer_user_id, milestone_rewards, is_celebration);
            Ok(None)
        }
    }


    /// marks analysis as failed
    pub async fn mark_analysis_failed(&self, analysis_id: i32) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = self.pool.get().await?;
        client
            .execute(
                "UPDATE user_analyses SET status = 'failed' WHERE id = $1",
                &[&analysis_id],
            )
            .await?;
        info!("Marked analysis {} as failed", analysis_id);
        Ok(())
    }

    /// creates a pending analysis record without consuming credit
    pub async fn create_pending_analysis(
        &self,
        user_id: i32,
        channel_name: &str,
        analysis_type: &str,
    ) -> Result<i32, UserManagerError> {
        let client = self.pool.get().await?;

        // create pending analysis record
        let analysis_id = client
            .query_one(
                "INSERT INTO user_analyses (user_id, channel_name, credits_used, analysis_type, status) VALUES ($1, $2, 0, $3, 'pending') RETURNING id",
                &[&user_id, &channel_name, &analysis_type],
            )
            .await?
            .get::<_, i32>(0);

        info!("Created pending analysis {} for user {} (channel: {})", analysis_id, user_id, channel_name);
        Ok(analysis_id)
    }

    /// atomically consumes credit, marks analysis completed, and returns remaining credits
    pub async fn atomic_complete_analysis(
        &self,
        analysis_id: i32,
        user_id: i32,
    ) -> Result<i32, UserManagerError> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;

        // consume credit only if user has sufficient credits
        let row = transaction
            .query_opt(
                "UPDATE users SET analysis_credits = analysis_credits - 1, total_analyses_performed = total_analyses_performed + 1, updated_at = NOW() 
                 WHERE id = $1 AND analysis_credits > 0 
                 RETURNING analysis_credits",
                &[&user_id],
            )
            .await?;

        let remaining_credits = match row {
            Some(row) => row.get::<_, i32>(0),
            None => {
                // check if user exists to provide more specific error
                let user_exists = transaction
                    .query_opt(
                        "SELECT 1 FROM users WHERE id = $1",
                        &[&user_id],
                    )
                    .await?
                    .is_some();
                
                transaction.rollback().await?;
                
                return if user_exists {
                    Err(UserManagerError::InsufficientCredits(user_id))
                } else {
                    Err(UserManagerError::UserNotFound(user_id))
                };
            }
        };

        // mark analysis as completed
        transaction
            .execute(
                "UPDATE user_analyses SET status = 'completed', credits_used = 1 WHERE id = $1",
                &[&analysis_id],
            )
            .await?;

        transaction.commit().await?;

        info!("Atomically completed analysis {} for user {} (remaining credits: {})", analysis_id, user_id, remaining_credits);
        Ok(remaining_credits)
    }

    /// gets all pending analyses for recovery
    pub async fn get_pending_analyses(&self) -> Result<Vec<PendingAnalysis>, Box<dyn std::error::Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT ua.id, ua.user_id, u.telegram_user_id, ua.channel_name, ua.analysis_type 
                 FROM user_analyses ua 
                 JOIN users u ON ua.user_id = u.id 
                 WHERE ua.status = 'pending' 
                 ORDER BY ua.analysis_timestamp ASC",
                &[],
            )
            .await?;

        let pending_analyses: Vec<PendingAnalysis> = rows
            .into_iter()
            .map(|row| PendingAnalysis {
                id: row.get(0),
                user_id: row.get(1),
                telegram_user_id: row.get(2),
                channel_name: row.get(3),
                analysis_type: row.get(4),
            })
            .collect();

        info!("Found {} pending analyses for recovery", pending_analyses.len());
        Ok(pending_analyses)
    }

    /// consume 1 credit for group analysis access 
    pub async fn consume_credit_for_group_analysis(
        &self,
        user_id: i32,
    ) -> Result<i32, UserManagerError> {
        let client = self.pool.get().await?;
        
        let row = client
            .query_opt(
                "UPDATE users SET analysis_credits = analysis_credits - 1, total_analyses_performed = total_analyses_performed + 1, updated_at = NOW() 
                 WHERE id = $1 AND analysis_credits > 0 
                 RETURNING analysis_credits",
                &[&user_id],
            )
            .await?;

        match row {
            Some(row) => {
                let remaining_credits: i32 = row.get(0);
                info!("Consumed 1 credit for group analysis for user {}, remaining: {}", user_id, remaining_credits);
                Ok(remaining_credits)
            }
            None => {
                // check if user exists to provide more specific error
                let user_exists = client
                    .query_opt("SELECT 1 FROM users WHERE id = $1", &[&user_id])
                    .await?
                    .is_some();

                if user_exists {
                    Err(UserManagerError::InsufficientCredits(user_id))
                } else {
                    Err(UserManagerError::UserNotFound(user_id))
                }
            }
        }
    }

    /// adds credits to user (for future payment integration)
    pub async fn add_credits(
        &self,
        user_id: i32,
        credits_to_add: i32,
    ) -> Result<i32, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;

        let row = client
            .query_opt(
                "UPDATE users SET analysis_credits = analysis_credits + $2, updated_at = NOW() 
                 WHERE id = $1 
                 RETURNING analysis_credits",
                &[&user_id, &credits_to_add],
            )
            .await?;

        match row {
            Some(row) => {
                let new_balance: i32 = row.get(0);
                info!(
                    "Added {} credits to user {}, new balance: {}",
                    credits_to_add, user_id, new_balance
                );
                Ok(new_balance)
            }
            None => {
                error!("User {} not found when adding credits", user_id);
                Err("User not found".into())
            }
        }
    }

    /// validates that a user ID exists and can be used as a referrer
    pub async fn validate_referrer(&self, user_id: i32) -> Result<bool, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt("SELECT 1 FROM users WHERE id = $1", &[&user_id])
            .await?;
        Ok(row.is_some())
    }

    /// checks if user qualifies for referral rewards and awards them
    pub async fn check_and_award_referral_rewards(&self, user_id: i32) -> Result<ReferralRewardInfo, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        
        // get current referral counts and telegram_user_id
        let row = client
            .query_opt(
                "SELECT referrals_count, paid_referrals_count, telegram_user_id FROM users WHERE id = $1",
                &[&user_id],
            )
            .await?;

        if let Some(row) = row {
            let referrals_count: i32 = row.get(0);
            let paid_referrals_count: i32 = row.get(1);
            let telegram_user_id: i64 = row.get(2);

            let mut milestone_rewards = 0;
            let mut paid_rewards = 0;

            // check for milestone rewards using new pattern (1, 5, 10, 20, 30, etc.)
            let expected_milestone_rewards = Self::calculate_milestone_rewards(referrals_count);
            let existing_unpaid_rewards = client
                .query_one(
                    "SELECT COUNT(*) FROM referral_rewards WHERE referrer_user_id = $1 AND reward_type = 'unpaid_milestone'",
                    &[&user_id],
                )
                .await?
                .get::<_, i64>(0) as i32;

            if expected_milestone_rewards > existing_unpaid_rewards {
                let new_rewards = expected_milestone_rewards - existing_unpaid_rewards;
                milestone_rewards = new_rewards;
                for _ in 0..new_rewards {
                    // award 1 credit for milestone
                    client
                        .execute(
                            "UPDATE users SET analysis_credits = analysis_credits + 1 WHERE id = $1",
                            &[&user_id],
                        )
                        .await?;

                    // record the reward
                    client
                        .execute(
                            "INSERT INTO referral_rewards (referrer_user_id, referee_user_id, reward_type, credits_awarded) VALUES ($1, $1, 'unpaid_milestone', 1)",
                            &[&user_id],
                        )
                        .await?;
                }
                info!("Awarded {} milestone rewards to user {}", new_rewards, user_id);
            }

            // check for paid user rewards
            let existing_paid_rewards = client
                .query_one(
                    "SELECT COUNT(*) FROM referral_rewards WHERE referrer_user_id = $1 AND reward_type = 'paid_user'",
                    &[&user_id],
                )
                .await?
                .get::<_, i64>(0) as i32;

            if paid_referrals_count > existing_paid_rewards {
                let new_paid_rewards = paid_referrals_count - existing_paid_rewards;
                paid_rewards = new_paid_rewards;
                for _ in 0..new_paid_rewards {
                    // award 1 credit for paid referral
                    client
                        .execute(
                            "UPDATE users SET analysis_credits = analysis_credits + 1 WHERE id = $1",
                            &[&user_id],
                        )
                        .await?;

                    // record the reward
                    client
                        .execute(
                            "INSERT INTO referral_rewards (referrer_user_id, referee_user_id, reward_type, credits_awarded) VALUES ($1, $1, 'paid_user', 1)",
                            &[&user_id],
                        )
                        .await?;
                }
                info!("Awarded {} paid referral rewards to user {}", new_paid_rewards, user_id);
            }

            Ok(ReferralRewardInfo {
                milestone_rewards,
                paid_rewards,
                total_credits_awarded: milestone_rewards + paid_rewards,
                referrer_telegram_id: if milestone_rewards > 0 || paid_rewards > 0 { Some(telegram_user_id) } else { None },
                referrer_user_id: if milestone_rewards > 0 || paid_rewards > 0 { Some(user_id) } else { None },
                is_celebration_milestone: Self::is_celebration_milestone(referrals_count),
                referral_count: referrals_count,
            })
        } else {
            Ok(ReferralRewardInfo {
                milestone_rewards: 0,
                paid_rewards: 0,
                total_credits_awarded: 0,
                referrer_telegram_id: None,
                referrer_user_id: None,
                is_celebration_milestone: false,
                referral_count: 0,
            })
        }
    }

    /// increments paid referrals count when a referred user makes a payment
    pub async fn record_paid_referral(&self, user_id: i32) -> Result<Option<ReferralRewardInfo>, Box<dyn Error + Send + Sync>> {
        info!("Processing paid referral for user {}", user_id);
        let client = self.pool.get().await?;
        
        // find if this user was referred and update referrer's paid count
        let row = client
            .query_opt(
                "SELECT referred_by_user_id FROM users WHERE id = $1",
                &[&user_id],
            )
            .await?;

        if let Some(row) = row {
            if let Some(referrer_id) = row.get::<_, Option<i32>>(0) {
                info!("User {} was referred by user {}, incrementing paid referral count", user_id, referrer_id);
                // increment paid referrals count
                client
                    .execute(
                        "UPDATE users SET paid_referrals_count = paid_referrals_count + 1 WHERE id = $1",
                        &[&referrer_id],
                    )
                    .await?;
                info!("Successfully incremented paid referral count for referrer {}", referrer_id);

                // check and award rewards
                info!("Checking and awarding referral rewards for referrer {}", referrer_id);
                let reward_info = self.check_and_award_referral_rewards(referrer_id).await?;
                
                info!("Recorded paid referral for user {}, referrer {} - rewards: milestone={}, paid={}, total={}", 
                      user_id, referrer_id, reward_info.milestone_rewards, reward_info.paid_rewards, reward_info.total_credits_awarded);
                return Ok(Some(reward_info));
            } else {
                info!("User {} was not referred by anyone (referred_by_user_id is NULL)", user_id);
            }
        } else {
            info!("User {} not found in database", user_id);
        }

        info!("No paid referral to record for user {}", user_id);
        Ok(None)
    }

    /// records access to a group analysis for tracking and billing purposes
    pub async fn record_group_analysis_access(
        &self,
        user_id: i32,
        group_analysis_id: i32,
        analysis_type: &str,
        target_user_id: i64,
    ) -> Result<(), UserManagerError> {
        let client = self.pool.get().await?;
        
        client
            .execute(
                "INSERT INTO group_analysis_access (user_id, group_analysis_id, analysis_type, target_user_id) 
                 VALUES ($1, $2, $3, $4)",
                &[&user_id, &group_analysis_id, &analysis_type, &target_user_id],
            )
            .await?;

        info!("Recorded group analysis access: user_id={}, group_analysis_id={}, analysis_type={}, target_user_id={}", 
              user_id, group_analysis_id, analysis_type, target_user_id);
        Ok(())
    }
}
