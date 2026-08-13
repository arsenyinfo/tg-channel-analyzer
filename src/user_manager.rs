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
    pub telegram_user_id: i64, // kept for bot notification purposes
    pub channel_name: String,
    pub analysis_type: String,
    pub language: Option<String>,
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
    pub fn pool(&self) -> Arc<Pool> {
        self.pool.clone()
    }

    pub async fn suppress_campaigns(
        &self,
        telegram_user_id: i64,
        reason: &str,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;
        transaction
            .execute(
                "INSERT INTO campaign_suppressions (telegram_user_id, reason)
                 VALUES ($1, $2)
                 ON CONFLICT (telegram_user_id) DO UPDATE
                 SET reason = EXCLUDED.reason, created_at = NOW()",
                &[&telegram_user_id, &reason],
            )
            .await?;
        transaction
            .execute(
                "UPDATE message_queue
                 SET status = 'failed', last_error_code = 'user_opt_out',
                     error_message = 'User opted out before delivery'
                 WHERE telegram_user_id = $1 AND campaign_id IS NOT NULL
                   AND status = 'pending'",
                &[&telegram_user_id],
            )
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub fn new(pool: Arc<Pool>) -> Self {
        Self { pool }
    }

    /// number of milestone rewards earned at a given referral count.
    /// canonical schedule (one credit each): 1, 5, 10, 20, 30, 40, ...
    pub fn milestones_reached(referral_count: i32) -> i32 {
        match referral_count {
            n if n < 1 => 0,
            n if n < 5 => 1,
            n if n < 10 => 2,
            n => 2 + n / 10, // 10->3, 20->4, 30->5, ...
        }
    }

    /// referral count that triggers the next milestone after `referral_count`
    pub fn next_milestone(referral_count: i32) -> i32 {
        match referral_count {
            n if n < 1 => 1,
            n if n < 5 => 5,
            n if n < 10 => 10,
            n => ((n / 10) + 1) * 10,
        }
    }

    /// true when `referral_count` lands exactly on a celebration milestone: 1, 5, 10, 20, 30, ...
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
        let mut client = self.pool.get().await?;

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

        // create new user and process any referral atomically in one transaction,
        // so a referral-processing failure rolls back the user insert rather than
        // leaving referred_by_user_id set with no referrer increment
        let transaction = client.transaction().await?;

        let row = transaction
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

        let reward_info = match referrer_user_id {
            Some(referrer_id) => {
                info!(
                    "Processing new referral: user {} was referred by user {}",
                    telegram_user_id, referrer_id
                );
                Self::process_new_referral_tx(&transaction, referrer_id).await?
            }
            None => {
                info!("New user {} created without referrer", telegram_user_id);
                None
            }
        };

        transaction.commit().await?;
        Ok((user, reward_info))
    }

    /// processes a new referral within an open transaction: increments the referrer's
    /// count (locking the row) and awards any newly-reached milestone rewards idempotently
    async fn process_new_referral_tx(
        transaction: &tokio_postgres::Transaction<'_>,
        referrer_user_id: i32,
    ) -> Result<Option<ReferralRewardInfo>, Box<dyn Error + Send + Sync>> {
        // increment referrals count (row lock held for the transaction) and get new count
        let row = transaction
            .query_one(
                "UPDATE users SET referrals_count = referrals_count + 1 WHERE id = $1 RETURNING referrals_count, telegram_user_id",
                &[&referrer_user_id],
            )
            .await?;

        let new_referral_count: i32 = row.get(0);
        let telegram_user_id: i64 = row.get(1);

        let milestone_rewards =
            Self::award_milestone_rewards(transaction, referrer_user_id, new_referral_count)
                .await?;

        let is_celebration = Self::is_celebration_milestone(new_referral_count);
        info!(
            "Referral processed for referrer {} (telegram_id: {}): count={}, milestone_credits={}, celebration={}",
            referrer_user_id, telegram_user_id, new_referral_count, milestone_rewards, is_celebration
        );

        if milestone_rewards > 0 || is_celebration {
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
            Ok(None)
        }
    }

    /// awards credits for every milestone ordinal newly reached at `referral_count`.
    /// idempotent: the partial unique index on (referrer_user_id, milestone) makes a
    /// concurrent or repeated grant of the same ordinal a no-op. returns credits granted.
    async fn award_milestone_rewards(
        transaction: &tokio_postgres::Transaction<'_>,
        referrer_user_id: i32,
        referral_count: i32,
    ) -> Result<i32, Box<dyn Error + Send + Sync>> {
        let target = Self::milestones_reached(referral_count);
        let existing = transaction
            .query_one(
                "SELECT COUNT(*) FROM referral_rewards WHERE referrer_user_id = $1 AND reward_type = 'unpaid_milestone'",
                &[&referrer_user_id],
            )
            .await?
            .get::<_, i64>(0) as i32;

        let mut granted = 0;
        for ordinal in (existing + 1)..=target {
            let inserted = transaction
                .execute(
                    "INSERT INTO referral_rewards (referrer_user_id, referee_user_id, reward_type, credits_awarded, milestone)
                     VALUES ($1, $1, 'unpaid_milestone', 1, $2)
                     ON CONFLICT (referrer_user_id, milestone) WHERE reward_type = 'unpaid_milestone' DO NOTHING",
                    &[&referrer_user_id, &ordinal],
                )
                .await?;
            if inserted == 1 {
                transaction
                    .execute(
                        "UPDATE users SET analysis_credits = analysis_credits + 1 WHERE id = $1",
                        &[&referrer_user_id],
                    )
                    .await?;
                granted += 1;
            }
        }
        if granted > 0 {
            info!(
                "Awarded {} milestone credits to user {} (target ordinal {})",
                granted, referrer_user_id, target
            );
        }
        Ok(granted)
    }

    /// marks analysis as failed
    pub async fn mark_analysis_failed(
        &self,
        analysis_id: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = self.pool.get().await?;
        // only a still-pending analysis may be failed; never clobber a completed
        // (charged) row. a completed-but-undelivered analysis is handled by refund_analysis.
        let updated = client
            .execute(
                "UPDATE user_analyses SET status = 'failed' WHERE id = $1 AND status = 'pending'",
                &[&analysis_id],
            )
            .await?;
        if updated == 1 {
            info!("Marked analysis {} as failed", analysis_id);
        }
        Ok(())
    }

    /// refunds a completed (charged) analysis whose result could not be delivered.
    /// no-op unless the analysis is in 'completed' state, so it is safe to call redundantly.
    pub async fn refund_analysis(
        &self,
        analysis_id: i32,
        user_id: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;
        let updated = transaction
            .execute(
                "UPDATE user_analyses SET status = 'failed' WHERE id = $1 AND status = 'completed'",
                &[&analysis_id],
            )
            .await?;
        if updated == 1 {
            transaction
                .execute(
                    "UPDATE users SET analysis_credits = analysis_credits + 1, updated_at = NOW() WHERE id = $1",
                    &[&user_id],
                )
                .await?;
            transaction.commit().await?;
            info!(
                "Refunded analysis {} for user {} (delivery failed)",
                analysis_id, user_id
            );
        } else {
            transaction.rollback().await?;
        }
        Ok(())
    }

    /// marks a completed analysis as delivered
    pub async fn mark_analysis_delivered(
        &self,
        analysis_id: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = self.pool.get().await?;
        client
            .execute(
                "UPDATE user_analyses SET delivered_at = NOW() WHERE id = $1 AND status = 'completed'",
                &[&analysis_id],
            )
            .await?;
        Ok(())
    }

    /// Claims a Telegram callback and creates its pending analysis without consuming credit.
    /// Returns `None` when the callback was already claimed by another delivery.
    pub async fn create_pending_analysis(
        &self,
        user_id: i32,
        channel_name: &str,
        analysis_type: &str,
        language: Option<&str>,
        telegram_callback_query_id: &str,
    ) -> Result<Option<i32>, UserManagerError> {
        let client = self.pool.get().await?;

        let analysis_id = client
            .query_opt(
                "INSERT INTO user_analyses
                    (user_id, channel_name, credits_used, analysis_type, status, language,
                     telegram_callback_query_id, experiment_campaign_id)
                 VALUES ($1, $2, 0, $3, 'pending', $4, $5, (
                     SELECT cr.campaign_id
                     FROM campaign_recipients cr
                     LEFT JOIN message_queue mq
                       ON mq.campaign_id = cr.campaign_id AND mq.user_id = cr.user_id
                     WHERE cr.user_id = $1
                       AND cr.enrolled_at >= NOW() - INTERVAL '30 days'
                       AND (cr.variant IN ('holdout', 'message_credit') OR mq.sent_at IS NOT NULL)
                     ORDER BY COALESCE(mq.sent_at, cr.enrolled_at) DESC
                     LIMIT 1
                 ))
                 ON CONFLICT (telegram_callback_query_id) DO NOTHING
                 RETURNING id",
                &[
                    &user_id,
                    &channel_name,
                    &analysis_type,
                    &language,
                    &telegram_callback_query_id,
                ],
            )
            .await?
            .map(|row| row.get::<_, i32>(0));

        if let Some(analysis_id) = analysis_id {
            info!(
                "Created pending analysis {} for user {} (channel: {}, lang: {:?})",
                analysis_id, user_id, channel_name, language
            );
        } else {
            info!("Callback query was already claimed; skipping duplicate analysis");
        }
        Ok(analysis_id)
    }

    /// atomically consumes credit, marks analysis completed, and returns remaining credits
    pub async fn atomic_complete_analysis(
        &self,
        analysis_id: i32,
        user_id: i32,
        result_source: &str,
        llm_cache_key: Option<&str>,
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
                    .query_opt("SELECT 1 FROM users WHERE id = $1", &[&user_id])
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

        // guard against double-completion; no-op if already completed
        let updated = transaction
            .execute(
                "UPDATE user_analyses
                 SET status = 'completed', credits_used = 1,
                     result_source = $3, llm_cache_key = $4
                 WHERE id = $1 AND user_id = $2 AND status = 'pending'",
                &[&analysis_id, &user_id, &result_source, &llm_cache_key],
            )
            .await?;

        if updated == 0 {
            // analysis already completed (race condition); roll back the credit deduction
            transaction.rollback().await?;
            info!(
                "Analysis {} already completed, skipping charge for user {}",
                analysis_id, user_id
            );
            let client = self.pool.get().await?;
            let row = client
                .query_opt(
                    "SELECT analysis_credits FROM users WHERE id = $1",
                    &[&user_id],
                )
                .await?
                .ok_or(UserManagerError::UserNotFound(user_id))?;
            return Ok(row.get(0));
        }

        transaction.commit().await?;

        info!(
            "Atomically completed analysis {} for user {} (remaining credits: {})",
            analysis_id, user_id, remaining_credits
        );
        Ok(remaining_credits)
    }

    /// gets all pending analyses for recovery
    pub async fn get_pending_analyses(
        &self,
    ) -> Result<Vec<PendingAnalysis>, Box<dyn std::error::Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT ua.id, ua.user_id, u.telegram_user_id, ua.channel_name, ua.analysis_type, ua.language 
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
                language: row.get(5),
            })
            .collect();

        info!(
            "Found {} pending analyses for recovery",
            pending_analyses.len()
        );
        Ok(pending_analyses)
    }

    /// refunds and fails any analysis that was charged but never delivered (e.g. the bot
    /// crashed between the credit deduction and result delivery). returns the count reconciled.
    /// run once at startup before recovery.
    pub async fn reconcile_undelivered_analyses(
        &self,
    ) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;

        let rows = transaction
            .query(
                "SELECT id, user_id FROM user_analyses WHERE status = 'completed' AND delivered_at IS NULL FOR UPDATE",
                &[],
            )
            .await?;

        if rows.is_empty() {
            transaction.rollback().await?;
            return Ok(0);
        }

        let ids: Vec<i32> = rows.iter().map(|r| r.get(0)).collect();
        for row in &rows {
            let user_id: i32 = row.get(1);
            transaction
                .execute(
                    "UPDATE users SET analysis_credits = analysis_credits + 1, updated_at = NOW() WHERE id = $1",
                    &[&user_id],
                )
                .await?;
        }
        transaction
            .execute(
                "UPDATE user_analyses SET status = 'failed' WHERE id = ANY($1)",
                &[&ids],
            )
            .await?;
        transaction.commit().await?;

        info!(
            "Reconciled {} charged-but-undelivered analyses (refunded)",
            ids.len()
        );
        Ok(ids.len())
    }

    /// idempotently records a successful Telegram payment and credits the user.
    /// returns Ok(Some(new_balance)) on first processing, Ok(None) if this charge id
    /// was already processed (duplicate redelivery), so the caller skips re-crediting.
    pub async fn process_payment(
        &self,
        user_id: i32,
        telegram_payment_charge_id: &str,
        credits: i32,
        stars_amount: i32,
        invoice_payload: &str,
    ) -> Result<Option<i32>, Box<dyn Error + Send + Sync>> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;

        let inserted = transaction
            .execute(
                "INSERT INTO payments (telegram_payment_charge_id, user_id, credits, stars_amount, invoice_payload)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (telegram_payment_charge_id) DO NOTHING",
                &[&telegram_payment_charge_id, &user_id, &credits, &stars_amount, &invoice_payload],
            )
            .await?;

        if inserted == 0 {
            transaction.rollback().await?;
            return Ok(None);
        }

        let new_balance: i32 = transaction
            .query_one(
                "UPDATE users SET analysis_credits = analysis_credits + $2, updated_at = NOW() WHERE id = $1 RETURNING analysis_credits",
                &[&user_id, &credits],
            )
            .await?
            .get(0);

        transaction.commit().await?;
        info!(
            "Processed payment {} for user {}: +{} credits, new balance {}",
            telegram_payment_charge_id, user_id, credits, new_balance
        );
        Ok(Some(new_balance))
    }

    /// validates that a user ID exists and can be used as a referrer
    pub async fn validate_referrer(
        &self,
        user_id: i32,
    ) -> Result<bool, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt("SELECT 1 FROM users WHERE id = $1", &[&user_id])
            .await?;
        Ok(row.is_some())
    }

    /// awards the one-time paid-referral bonus when a referred user makes their FIRST payment.
    /// idempotent per-referee via users.first_payment_rewarded, so repeat payments grant nothing.
    /// `referee_id` is the paying user.
    pub async fn record_paid_referral(
        &self,
        referee_id: i32,
    ) -> Result<Option<ReferralRewardInfo>, Box<dyn Error + Send + Sync>> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;

        // claim the first-payment bonus for this referee (row-locks via the conditional update)
        let claimed = transaction
            .execute(
                "UPDATE users SET first_payment_rewarded = TRUE WHERE id = $1 AND first_payment_rewarded = FALSE",
                &[&referee_id],
            )
            .await?;

        if claimed == 0 {
            // already rewarded for a prior payment, or user does not exist
            transaction.rollback().await?;
            return Ok(None);
        }

        let referred_by: Option<i32> = transaction
            .query_one(
                "SELECT referred_by_user_id FROM users WHERE id = $1",
                &[&referee_id],
            )
            .await?
            .get(0);

        let Some(referrer_id) = referred_by else {
            // first payment, but this user was not referred by anyone
            transaction.commit().await?;
            return Ok(None);
        };

        // award the referrer one credit and increment their paid count (row-locked by the update)
        let row = transaction
            .query_one(
                "UPDATE users SET paid_referrals_count = paid_referrals_count + 1, analysis_credits = analysis_credits + 1, updated_at = NOW()
                 WHERE id = $1 RETURNING telegram_user_id, referrals_count",
                &[&referrer_id],
            )
            .await?;
        let referrer_telegram_id: i64 = row.get(0);
        let referrals_count: i32 = row.get(1);

        transaction
            .execute(
                "INSERT INTO referral_rewards (referrer_user_id, referee_user_id, reward_type, credits_awarded, milestone) VALUES ($1, $2, 'paid_user', 1, NULL)",
                &[&referrer_id, &referee_id],
            )
            .await?;

        transaction.commit().await?;

        info!(
            "Awarded paid-referral bonus: referee {} first payment -> referrer {} +1 credit",
            referee_id, referrer_id
        );

        Ok(Some(ReferralRewardInfo {
            milestone_rewards: 0,
            paid_rewards: 1,
            total_credits_awarded: 1,
            referrer_telegram_id: Some(referrer_telegram_id),
            referrer_user_id: Some(referrer_id),
            is_celebration_milestone: false,
            referral_count: referrals_count,
        }))
    }
}
