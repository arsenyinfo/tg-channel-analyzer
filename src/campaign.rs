use chrono::{DateTime, Duration, NaiveTime, Utc};
use chrono_tz::Tz;
use deadpool_postgres::{GenericClient, Pool};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::str::FromStr;
use std::sync::Arc;
use tokio_postgres::Row;

use crate::campaign_schedule::next_allowed_time;

pub type CampaignError = Box<dyn Error + Send + Sync>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CampaignConfig {
    pub inactivity_days: i64,
    pub contact_cooldown_days: i64,
    pub timezone: String,
    pub window_start: String,
    pub window_end: String,
    pub cadence_seconds: i64,
    pub assignment_version: String,
    pub holdout_bps: u16,
    pub message_bps: u16,
    pub message_credit_bps: u16,
    pub paid_credit: i32,
    pub free_credit: i32,
    pub copy_version: String,
}

impl Default for CampaignConfig {
    fn default() -> Self {
        Self {
            inactivity_days: 30,
            contact_cooldown_days: 14,
            timezone: "UTC".to_string(),
            window_start: "08:00".to_string(),
            window_end: "20:00".to_string(),
            cadence_seconds: 20,
            assignment_version: "campaign-arm-v1".to_string(),
            holdout_bps: 0,
            message_bps: 5_000,
            message_credit_bps: 5_000,
            paid_credit: 1,
            free_credit: 1,
            copy_version: "google-gemini-v1".to_string(),
        }
    }
}

impl CampaignConfig {
    pub fn validate(&self) -> Result<(Tz, NaiveTime, NaiveTime), CampaignError> {
        if self.inactivity_days < 1 || self.contact_cooldown_days < 0 {
            return Err("inactivity days must be positive and cooldown cannot be negative".into());
        }
        if self.cadence_seconds < 1 {
            return Err("cadence must be at least one second".into());
        }
        if u32::from(self.holdout_bps)
            + u32::from(self.message_bps)
            + u32::from(self.message_credit_bps)
            != 10_000
        {
            return Err("campaign arm weights must sum to exactly 10,000 basis points".into());
        }
        if self.message_credit_bps > 0 && (self.paid_credit <= 0 || self.free_credit <= 0) {
            return Err(
                "the message-credit arm requires positive paid and free credit grants".into(),
            );
        }
        if self.assignment_version != "campaign-arm-v1" {
            return Err(format!(
                "unsupported campaign assignment version: {}",
                self.assignment_version
            )
            .into());
        }
        if self.copy_version != "google-gemini-v1" {
            return Err(format!("unsupported campaign copy version: {}", self.copy_version).into());
        }

        let timezone = Tz::from_str(&self.timezone)
            .map_err(|_| format!("unknown IANA timezone: {}", self.timezone))?;
        let start = NaiveTime::parse_from_str(&self.window_start, "%H:%M")?;
        let end = NaiveTime::parse_from_str(&self.window_end, "%H:%M")?;
        if start >= end {
            return Err("send window must be daytime-style: start must be before end".into());
        }
        Ok((timezone, start, end))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cohort {
    Paid,
    Free,
    LegacyUnknown,
}

impl Cohort {
    fn as_str(self) -> &'static str {
        match self {
            Self::Paid => "paid",
            Self::Free => "free",
            Self::LegacyUnknown => "legacy_unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Holdout,
    Message,
    MessageCredit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyVariant {
    A,
    B,
    C,
}

impl CopyVariant {
    fn as_str(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
        }
    }
}

impl Variant {
    fn as_str(self) -> &'static str {
        match self {
            Self::Holdout => "holdout",
            Self::Message => "message",
            Self::MessageCredit => "message_credit",
        }
    }
}

#[derive(Clone, Debug)]
struct Candidate {
    user_id: i32,
    telegram_user_id: i64,
    language: Option<String>,
    credits: i32,
    cohort: Cohort,
}

impl Candidate {
    fn from_row(row: &Row) -> Result<Self, CampaignError> {
        let cohort: String = row.get("cohort");
        let cohort = match cohort.as_str() {
            "paid" => Cohort::Paid,
            "free" => Cohort::Free,
            "legacy_unknown" => Cohort::LegacyUnknown,
            other => return Err(format!("unexpected campaign cohort: {other}").into()),
        };
        Ok(Self {
            user_id: row.get("id"),
            telegram_user_id: row.get("telegram_user_id"),
            language: row.get("language"),
            credits: row.get("analysis_credits"),
            cohort,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CampaignCounts {
    pub paid: usize,
    pub free: usize,
    pub legacy_unknown: usize,
    pub russian: usize,
    pub english_fallback: usize,
    pub with_balance: usize,
    pub holdout: usize,
    pub message: usize,
    pub message_credit: usize,
    pub copy_a: usize,
    pub copy_b: usize,
    pub copy_c: usize,
    pub maximum_credit_liability: i32,
}

impl CampaignCounts {
    pub fn total(&self) -> usize {
        self.paid + self.free + self.legacy_unknown
    }

    fn add(&mut self, candidate: &Candidate) {
        match candidate.cohort {
            Cohort::Paid => self.paid += 1,
            Cohort::Free => self.free += 1,
            Cohort::LegacyUnknown => self.legacy_unknown += 1,
        }
        if candidate.language.as_deref() == Some("ru") {
            self.russian += 1;
        } else {
            self.english_fallback += 1;
        }
        if candidate.credits > 0 {
            self.with_balance += 1;
        }
    }

    fn add_assignment(&mut self, variant: Variant, grant: i32) {
        match variant {
            Variant::Holdout => self.holdout += 1,
            Variant::Message => self.message += 1,
            Variant::MessageCredit => self.message_credit += 1,
        }
        self.maximum_credit_liability += grant;
    }

    fn add_copy(&mut self, copy: CopyVariant) {
        match copy {
            CopyVariant::A => self.copy_a += 1,
            CopyVariant::B => self.copy_b += 1,
            CopyVariant::C => self.copy_c += 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CampaignPreview {
    pub counts: CampaignCounts,
    pub sample_copy_a_en: String,
    pub sample_copy_b_en: String,
    pub sample_copy_c_en: String,
    pub sample_copy_a_ru: String,
    pub sample_copy_b_ru: String,
    pub sample_copy_c_ru: String,
    pub paid_credit_addendum_en: String,
    pub paid_credit_addendum_ru: String,
    pub free_credit_addendum_en: String,
    pub free_credit_addendum_ru: String,
}

#[derive(Clone, Debug, Default)]
pub struct EnrollmentResult {
    pub enrolled: usize,
    pub queued: usize,
    pub holdout: usize,
    pub credits_granted: i32,
    pub already_enrolled: usize,
}

pub struct CampaignManager {
    pool: Arc<Pool>,
}

impl CampaignManager {
    pub fn new(pool: Arc<Pool>) -> Self {
        Self { pool }
    }

    pub async fn preview(
        &self,
        campaign_key: &str,
        config: &CampaignConfig,
        batch_size: usize,
    ) -> Result<CampaignPreview, CampaignError> {
        validate_campaign_key(campaign_key)?;
        if batch_size == 0 {
            return Err("batch size must be positive".into());
        }
        config.validate()?;
        let client = self.pool.get().await?;
        let campaign_id = client
            .query_opt(
                "SELECT id, configuration FROM campaigns WHERE campaign_key = $1",
                &[&campaign_key],
            )
            .await?
            .map(|row| -> Result<i64, CampaignError> {
                let stored: serde_json::Value = row.get(1);
                if !configuration_matches_except_copy(&stored, config)? {
                    Err("campaign key already exists with different configuration".into())
                } else {
                    Ok(row.get::<_, i64>(0))
                }
            })
            .transpose()?;

        let rows = eligible_rows(&client, campaign_id, config, batch_size).await?;
        let candidates = rows
            .iter()
            .map(Candidate::from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let mut counts = CampaignCounts::default();
        for candidate in &candidates {
            counts.add(candidate);
            let (variant, grant, _) = assignment(campaign_key, candidate, config);
            counts.add_assignment(variant, grant);
            if variant != Variant::Holdout {
                counts.add_copy(copy_assignment(campaign_key, candidate, variant).0);
            }
        }

        Ok(CampaignPreview {
            counts,
            sample_copy_a_en: render_message("en", 0, CopyVariant::A),
            sample_copy_b_en: render_message("en", 0, CopyVariant::B),
            sample_copy_c_en: render_message("en", 0, CopyVariant::C),
            sample_copy_a_ru: render_message("ru", 0, CopyVariant::A),
            sample_copy_b_ru: render_message("ru", 0, CopyVariant::B),
            sample_copy_c_ru: render_message("ru", 0, CopyVariant::C),
            paid_credit_addendum_en: render_credit_addendum("en", config.paid_credit),
            paid_credit_addendum_ru: render_credit_addendum("ru", config.paid_credit),
            free_credit_addendum_en: render_credit_addendum("en", config.free_credit),
            free_credit_addendum_ru: render_credit_addendum("ru", config.free_credit),
        })
    }

    pub async fn enroll(
        &self,
        campaign_key: &str,
        config: &CampaignConfig,
        batch_size: usize,
    ) -> Result<EnrollmentResult, CampaignError> {
        validate_campaign_key(campaign_key)?;
        if batch_size == 0 {
            return Err("batch size must be positive".into());
        }
        let (timezone, window_start, window_end) = config.validate()?;
        let configuration = serde_json::to_value(config)?;
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;
        transaction
            .query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                &[&campaign_key],
            )
            .await?;

        let campaign_row = transaction
            .query_opt(
                "SELECT id, configuration, status
                 FROM campaigns WHERE campaign_key = $1 FOR UPDATE",
                &[&campaign_key],
            )
            .await?;
        let campaign_id = if let Some(row) = campaign_row {
            let stored: serde_json::Value = row.get(1);
            if !configuration_matches_except_copy(&stored, config)? {
                return Err("campaign key already exists with different configuration".into());
            }
            let status: String = row.get(2);
            if status != "active" {
                return Err(format!("campaign is {status}; resume it before enrolling").into());
            }
            let campaign_id = row.get(0);
            if stored != configuration {
                transaction
                    .execute(
                        "UPDATE campaigns
                         SET configuration = $2, updated_at = NOW()
                         WHERE id = $1",
                        &[&campaign_id, &configuration],
                    )
                    .await?;
            }
            campaign_id
        } else {
            transaction
                .query_one(
                    "INSERT INTO campaigns
                        (campaign_key, configuration, timezone, send_window_start,
                         send_window_end, cadence_seconds)
                     VALUES ($1, $2, $3, $4, $5, CAST($6 AS BIGINT)::INTEGER)
                     RETURNING id",
                    &[
                        &campaign_key,
                        &configuration,
                        &config.timezone,
                        &window_start,
                        &window_end,
                        &config.cadence_seconds,
                    ],
                )
                .await?
                .get(0)
        };

        let rows = eligible_rows(&transaction, Some(campaign_id), config, batch_size).await?;
        let candidates = rows
            .iter()
            .map(Candidate::from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let already_enrolled: i64 = transaction
            .query_one(
                "SELECT COUNT(*) FROM campaign_recipients WHERE campaign_id = $1",
                &[&campaign_id],
            )
            .await?
            .get(0);
        let last_scheduled: Option<DateTime<Utc>> = transaction
            .query_one(
                "SELECT MAX(scheduled_at) FROM message_queue WHERE campaign_id = $1",
                &[&campaign_id],
            )
            .await?
            .get(0);
        let now = Utc::now();
        let mut cursor = last_scheduled
            .map(|last| (last + Duration::seconds(config.cadence_seconds)).max(now))
            .unwrap_or(now);
        let mut result = EnrollmentResult {
            already_enrolled: usize::try_from(already_enrolled)?,
            ..EnrollmentResult::default()
        };

        for candidate in candidates {
            let (variant, grant, assignment_bucket) = assignment(campaign_key, &candidate, config);
            let copy = if variant == Variant::Holdout {
                None
            } else {
                Some(copy_assignment(campaign_key, &candidate, variant))
            };
            let copy_variant = copy.map(|(copy_variant, _)| copy_variant);
            let copy_variant_label = copy_variant.map(CopyVariant::as_str);
            let copy_assignment_bucket = copy.map(|(_, bucket)| i32::from(bucket));
            let copy_version = copy.map(|_| config.copy_version.as_str());
            let inserted = transaction
                .execute(
                    "INSERT INTO campaign_recipients
                        (campaign_id, user_id, cohort, variant, credits_granted,
                         assignment_version, assignment_bucket, baseline_credits,
                         copy_variant, copy_assignment_bucket, copy_version)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                     ON CONFLICT (campaign_id, user_id) DO NOTHING",
                    &[
                        &campaign_id,
                        &candidate.user_id,
                        &candidate.cohort.as_str(),
                        &variant.as_str(),
                        &grant,
                        &config.assignment_version,
                        &i32::from(assignment_bucket),
                        &candidate.credits,
                        &copy_variant_label,
                        &copy_assignment_bucket,
                        &copy_version,
                    ],
                )
                .await?;
            if inserted == 0 {
                result.already_enrolled += 1;
                continue;
            }
            result.enrolled += 1;

            if variant == Variant::Holdout {
                result.holdout += 1;
                continue;
            }

            if grant > 0 {
                transaction
                    .execute(
                        "INSERT INTO campaign_credit_grants (campaign_id, user_id, credits)
                         VALUES ($1, $2, $3)",
                        &[&campaign_id, &candidate.user_id, &grant],
                    )
                    .await?;
                transaction
                    .execute(
                        "UPDATE users
                         SET analysis_credits = analysis_credits + $2, updated_at = NOW()
                         WHERE id = $1",
                        &[&candidate.user_id, &grant],
                    )
                    .await?;
                result.credits_granted += grant;
            }

            let scheduled_at = next_allowed_time(cursor, timezone, window_start, window_end)?;
            cursor = scheduled_at + Duration::seconds(config.cadence_seconds);
            let language = if candidate.language.as_deref() == Some("ru") {
                "ru"
            } else {
                "en"
            };
            let copy_variant = copy_variant
                .ok_or("non-holdout campaign recipient has no message-copy assignment")?;
            let message = render_message(language, grant, copy_variant);
            let queued = transaction
                .execute(
                    "INSERT INTO message_queue
                        (telegram_user_id, user_id, campaign_id, message, parse_mode,
                         scheduled_at, next_attempt_at)
                     VALUES ($1, $2, $3, $4, 'HTML', $5, $5)
                     ON CONFLICT (campaign_id, user_id) WHERE campaign_id IS NOT NULL DO NOTHING",
                    &[
                        &candidate.telegram_user_id,
                        &candidate.user_id,
                        &campaign_id,
                        &message,
                        &scheduled_at,
                    ],
                )
                .await?;
            if queued != 1 {
                return Err(
                    "campaign recipient was enrolled but its queue row was not created".into(),
                );
            }
            result.queued += 1;
        }

        transaction.commit().await?;
        Ok(result)
    }

    pub async fn set_status(
        &self,
        campaign_key: &str,
        status: &str,
    ) -> Result<bool, CampaignError> {
        if !matches!(status, "active" | "paused" | "completed") {
            return Err("invalid campaign status".into());
        }
        let client = self.pool.get().await?;
        let transition = match status {
            "paused" => "active",
            "active" => "paused",
            "completed" => "active_or_paused",
            _ => unreachable!(),
        };
        let updated = if transition == "active_or_paused" {
            client
                .execute(
                    "UPDATE campaigns SET status = 'completed', updated_at = NOW()
                     WHERE campaign_key = $1 AND status IN ('active', 'paused')",
                    &[&campaign_key],
                )
                .await?
        } else {
            client
                .execute(
                    "UPDATE campaigns SET status = $2::VARCHAR, updated_at = NOW(),
                         next_send_at = CASE WHEN $2::VARCHAR = 'active'
                             THEN GREATEST(next_send_at, NOW()) ELSE next_send_at END
                     WHERE campaign_key = $1 AND status = $3",
                    &[&campaign_key, &status, &transition],
                )
                .await?
        };
        Ok(updated == 1)
    }

    pub async fn report(&self, campaign_key: &str) -> Result<Vec<(String, i64)>, CampaignError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "WITH recipient_outcomes AS (
                    SELECT cr.*,
                           EXISTS (
                               SELECT 1 FROM user_analyses ua
                               WHERE ua.user_id = cr.user_id
                                 AND ua.status = 'completed' AND ua.delivered_at IS NOT NULL
                                 AND ua.delivered_at >= cr.enrolled_at
                                 AND ua.delivered_at < cr.enrolled_at + INTERVAL '7 days'
                           ) AS analysis_7d,
                           EXISTS (
                               SELECT 1 FROM payments p
                               WHERE p.user_id = cr.user_id
                                 AND p.created_at >= cr.enrolled_at
                                 AND p.created_at < cr.enrolled_at + INTERVAL '14 days'
                           ) AS payment_14d
                    FROM campaign_recipients cr
                    JOIN campaigns c ON c.id = cr.campaign_id
                    WHERE c.campaign_key = $1
                 )
                 SELECT label, count FROM (
                    SELECT 'campaign:status:' || status AS label, 1::BIGINT AS count
                    FROM campaigns WHERE campaign_key = $1
                    UNION ALL
                    SELECT 'recipient:' || cohort || ':' || variant AS label, COUNT(*) AS count
                    FROM campaign_recipients cr
                    JOIN campaigns c ON c.id = cr.campaign_id
                    WHERE c.campaign_key = $1
                    GROUP BY cohort, variant
                    UNION ALL
                    SELECT 'recipient_copy:' || cohort || ':' || variant || ':'
                           || COALESCE(copy_version, 'unassigned') || ':'
                           || COALESCE(copy_variant, 'unassigned') AS label,
                           COUNT(*) AS count
                    FROM campaign_recipients cr
                    JOIN campaigns c ON c.id = cr.campaign_id
                    WHERE c.campaign_key = $1
                    GROUP BY cohort, variant, copy_version, copy_variant
                    UNION ALL
                    SELECT 'queue:' || mq.status AS label, COUNT(*) AS count
                    FROM message_queue mq
                    JOIN campaigns c ON c.id = mq.campaign_id
                    WHERE c.campaign_key = $1
                    GROUP BY mq.status
                    UNION ALL
                    SELECT 'outcome:' || cohort || ':' || variant || ':analysis_7d' AS label, COUNT(*) AS count
                    FROM recipient_outcomes
                    WHERE analysis_7d
                    GROUP BY cohort, variant
                    UNION ALL
                    SELECT 'outcome_copy:' || cohort || ':' || variant || ':'
                           || COALESCE(copy_version, 'unassigned') || ':'
                           || COALESCE(copy_variant, 'unassigned') || ':analysis_7d' AS label,
                           COUNT(*) AS count
                    FROM recipient_outcomes
                    WHERE analysis_7d
                    GROUP BY cohort, variant, copy_version, copy_variant
                    UNION ALL
                    SELECT 'outcome:' || cohort || ':' || variant || ':payment_14d' AS label, COUNT(*) AS count
                    FROM recipient_outcomes
                    WHERE payment_14d
                    GROUP BY cohort, variant
                    UNION ALL
                    SELECT 'outcome_copy:' || cohort || ':' || variant || ':'
                           || COALESCE(copy_version, 'unassigned') || ':'
                           || COALESCE(copy_variant, 'unassigned') || ':payment_14d' AS label,
                           COUNT(*) AS count
                    FROM recipient_outcomes
                    WHERE payment_14d
                    GROUP BY cohort, variant, copy_version, copy_variant
                    UNION ALL
                    SELECT 'credits:granted' AS label, COALESCE(SUM(cg.credits), 0)::BIGINT AS count
                    FROM campaigns c
                    LEFT JOIN campaign_credit_grants cg ON cg.campaign_id = c.id
                    WHERE c.campaign_key = $1
                    GROUP BY c.id
                    UNION ALL
                    SELECT 'llm:' || cr.cohort || ':' || cr.variant || ':total_tokens' AS label,
                           COALESCE(SUM(la.total_tokens), 0)::BIGINT AS count
                    FROM campaign_recipients cr
                    JOIN campaigns c ON c.id = cr.campaign_id
                    LEFT JOIN user_analyses ua ON ua.experiment_campaign_id = cr.campaign_id
                                                  AND ua.user_id = cr.user_id
                    LEFT JOIN llm_attempts la ON la.user_analysis_id = ua.id
                                                AND la.status = 'succeeded'
                    WHERE c.campaign_key = $1
                    GROUP BY cr.cohort, cr.variant
                    UNION ALL
                    SELECT 'llm:' || cr.cohort || ':' || cr.variant || ':unknown_attempts' AS label,
                           COUNT(la.attempt_key)::BIGINT AS count
                    FROM campaign_recipients cr
                    JOIN campaigns c ON c.id = cr.campaign_id
                    LEFT JOIN user_analyses ua ON ua.experiment_campaign_id = cr.campaign_id
                                                  AND ua.user_id = cr.user_id
                    LEFT JOIN llm_attempts la ON la.user_analysis_id = ua.id
                                                AND la.billing_certainty = 'unknown'
                    WHERE c.campaign_key = $1
                    GROUP BY cr.cohort, cr.variant
                    UNION ALL
                    SELECT 'analysis:' || cr.cohort || ':' || cr.variant || ':cache' AS label,
                           COUNT(ua.id)::BIGINT AS count
                    FROM campaign_recipients cr
                    JOIN campaigns c ON c.id = cr.campaign_id
                    LEFT JOIN user_analyses ua ON ua.experiment_campaign_id = cr.campaign_id
                                                  AND ua.user_id = cr.user_id
                                                  AND ua.result_source = 'cache'
                    WHERE c.campaign_key = $1
                    GROUP BY cr.cohort, cr.variant
                 ) stats ORDER BY label",
                &[&campaign_key],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect())
    }
}

async fn eligible_rows<C: GenericClient + Sync>(
    client: &C,
    campaign_id: Option<i64>,
    config: &CampaignConfig,
    batch_size: usize,
) -> Result<Vec<Row>, CampaignError> {
    let limit = i64::try_from(batch_size.max(1))?;
    let inactivity = i32::try_from(config.inactivity_days)?;
    let cooldown = i32::try_from(config.contact_cooldown_days)?;
    Ok(client
        .query(
            r#"
            WITH payment_tracking AS (
                SELECT applied_at FROM schema_migrations WHERE version = 6
            )
            SELECT u.id, u.telegram_user_id, u.language, u.analysis_credits,
                   CASE
                     WHEN EXISTS (SELECT 1 FROM payments p WHERE p.user_id = u.id) THEN 'paid'
                     WHEN u.created_at >= COALESCE((SELECT applied_at FROM payment_tracking), 'infinity') THEN 'free'
                     ELSE 'legacy_unknown'
                   END AS cohort
            FROM users u
            WHERE u.created_at < NOW() - ($2::INTEGER * INTERVAL '1 day')
              AND EXISTS (
                  SELECT 1 FROM user_analyses ua
                  WHERE ua.user_id = u.id
                    AND ua.status = 'completed'
                    AND ua.delivered_at IS NOT NULL
              )
              AND COALESCE((
                  SELECT MAX(ua.delivered_at) FROM user_analyses ua
                  WHERE ua.user_id = u.id
                    AND ua.status = 'completed'
                    AND ua.delivered_at IS NOT NULL
              ), u.created_at) < NOW() - ($2::INTEGER * INTERVAL '1 day')
              AND NOT EXISTS (
                  SELECT 1 FROM user_analyses ua
                  WHERE ua.user_id = u.id
                    AND (
                        ua.status = 'pending'
                        OR (ua.status = 'completed' AND ua.delivered_at IS NULL)
                    )
              )
              AND u.analysis_credits = 0
              AND NOT EXISTS (
                  SELECT 1 FROM payments p
                  WHERE p.user_id = u.id
                    AND p.created_at >= NOW() - ($2::INTEGER * INTERVAL '1 day')
              )
              AND NOT EXISTS (
                  SELECT 1 FROM campaign_suppressions cs
                  WHERE cs.telegram_user_id = u.telegram_user_id
              )
              AND NOT EXISTS (
                  SELECT 1 FROM campaign_recipients cr
                  WHERE cr.campaign_id = $1 AND cr.user_id = u.id
              )
              AND NOT EXISTS (
                  SELECT 1 FROM campaign_recipients prior
                  WHERE prior.user_id = u.id
                    AND prior.enrolled_at >= NOW() - ($3::INTEGER * INTERVAL '1 day')
              )
            ORDER BY (
                SELECT MAX(ua.delivered_at) FROM user_analyses ua
                WHERE ua.user_id = u.id AND ua.delivered_at IS NOT NULL
            ), u.id
            LIMIT $4
            "#,
            &[&campaign_id, &inactivity, &cooldown, &limit],
        )
        .await?)
}

fn validate_campaign_key(key: &str) -> Result<(), CampaignError> {
    if key.is_empty()
        || key.len() > 128
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("campaign key must be 1-128 ASCII letters, digits, '.', '_' or '-'".into());
    }
    Ok(())
}

fn configuration_matches_except_copy(
    stored: &serde_json::Value,
    requested: &CampaignConfig,
) -> Result<bool, CampaignError> {
    let mut stored_config: CampaignConfig = serde_json::from_value(stored.clone())?;
    stored_config.copy_version = requested.copy_version.clone();
    Ok(stored_config == *requested)
}

fn stable_bucket(campaign_key: &str, cohort: Cohort, user_id: i32, version: &str) -> u16 {
    let mut hasher = Sha256::new();
    hasher.update(version.as_bytes());
    hasher.update([0]);
    hasher.update(campaign_key.as_bytes());
    hasher.update([0]);
    hasher.update(cohort.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(user_id.to_be_bytes());
    let digest = hasher.finalize();
    let prefix = u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    );
    (prefix % 10_000) as u16
}

fn assignment(
    campaign_key: &str,
    candidate: &Candidate,
    config: &CampaignConfig,
) -> (Variant, i32, u16) {
    let bucket = stable_bucket(
        campaign_key,
        candidate.cohort,
        candidate.user_id,
        &config.assignment_version,
    );
    if bucket < config.holdout_bps {
        (Variant::Holdout, 0, bucket)
    } else if bucket < config.holdout_bps + config.message_bps {
        (Variant::Message, 0, bucket)
    } else {
        let grant = match candidate.cohort {
            Cohort::Paid => config.paid_credit,
            Cohort::Free => config.free_credit,
            Cohort::LegacyUnknown => config.free_credit,
        };
        (Variant::MessageCredit, grant, bucket)
    }
}

fn copy_assignment(
    campaign_key: &str,
    candidate: &Candidate,
    treatment: Variant,
) -> (CopyVariant, u16) {
    let mut hasher = Sha256::new();
    hasher.update(b"copy-arm-v1");
    hasher.update([0]);
    hasher.update(campaign_key.as_bytes());
    hasher.update([0]);
    hasher.update(candidate.cohort.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(treatment.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(candidate.user_id.to_be_bytes());
    let digest = hasher.finalize();
    let prefix = u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    );
    let bucket = (prefix % 10_000) as u16;
    let copy = if bucket < 3_334 {
        CopyVariant::A
    } else if bucket < 6_667 {
        CopyVariant::B
    } else {
        CopyVariant::C
    };
    (copy, bucket)
}

fn render_credit_addendum(language: &str, granted: i32) -> String {
    if language == "ru" {
        let last_two = granted.rem_euclid(100);
        let analyses = if (11..=14).contains(&last_two) {
            "бесплатных анализов"
        } else {
            match granted.rem_euclid(10) {
                1 => "бесплатный анализ",
                2..=4 => "бесплатных анализа",
                _ => "бесплатных анализов",
            }
        };
        format!("🎁 А ещё мы добавили вам <b>{granted} {analyses}</b>.")
    } else {
        let analyses = if granted == 1 {
            "free analysis"
        } else {
            "free analyses"
        };
        format!("🎁 As a bonus, we added <b>{granted} {analyses}</b> to your balance.")
    }
}

fn render_message(language: &str, granted: i32, copy_variant: CopyVariant) -> String {
    let russian = language == "ru";
    let (opening, cta) = match (russian, copy_variant) {
        (false, CopyVariant::A) => (
            "⚡ <b>Channel analysis now runs on Google’s newest Gemini model</b>\n\nIt understands more context, catches subtler patterns, and produces a deeper, sharper report.",
            "Send a link to your own channel, a blogger, or any public figure.",
        ),
        (false, CopyVariant::B) => (
            "👀 <b>What does a Telegram channel reveal between the lines?</b>\n\nGoogle’s newest Gemini model analyzes posts together, connects details across time, and notices patterns that are easy to miss.",
            "Send any public channel link and see what it finds.",
        ),
        (false, CopyVariant::C) => (
            "🔎 <b>Send a channel. We’ll read between the lines.</b>\n\nChannel analysis just got a major upgrade with Google’s newest Gemini model: more context, better connections, deeper conclusions.",
            "Try it on your own channel—or someone you’re curious about.",
        ),
        (true, CopyVariant::A) => (
            "⚡ <b>Теперь каналы анализирует новейшая модель Gemini от Google</b>\n\nОна учитывает больше контекста, замечает тонкие закономерности и делает разбор глубже и точнее.",
            "Отправьте ссылку на свой канал, канал блогера или другого публичного автора.",
        ),
        (true, CopyVariant::B) => (
            "👀 <b>Что Telegram-канал рассказывает между строк?</b>\n\nНовейшая модель Gemini от Google анализирует посты в контексте, связывает детали и замечает закономерности, которые легко упустить.",
            "Отправьте ссылку на любой публичный канал и посмотрите, что она найдёт.",
        ),
        (true, CopyVariant::C) => (
            "🔎 <b>Пришлите канал — мы прочитаем между строк</b>\n\nМы перевели анализ на новейшую модель Gemini от Google: больше контекста, точнее связи, глубже выводы.",
            "Проверьте свой канал или кого-то, кто вам интересен.",
        ),
    };

    if granted > 0 {
        format!(
            "{opening}\n\n{}\n\n{cta}",
            render_credit_addendum(language, granted)
        )
    } else {
        format!("{opening}\n\n{cta}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignments_are_stable_and_use_one_three_arm_bucket() {
        let config = CampaignConfig {
            holdout_bps: 0,
            message_bps: 0,
            message_credit_bps: 10_000,
            ..CampaignConfig::default()
        };
        let paid = Candidate {
            user_id: 42,
            telegram_user_id: 100,
            language: None,
            credits: 0,
            cohort: Cohort::Paid,
        };
        assert_eq!(
            assignment("launch", &paid, &config),
            (
                Variant::MessageCredit,
                1,
                stable_bucket("launch", Cohort::Paid, 42, "campaign-arm-v1")
            )
        );
        assert_eq!(
            assignment("launch", &paid, &config),
            (
                Variant::MessageCredit,
                1,
                stable_bucket("launch", Cohort::Paid, 42, "campaign-arm-v1")
            )
        );

        let free = Candidate {
            cohort: Cohort::Free,
            credits: 0,
            ..paid
        };
        let (_, free_grant, _) = assignment("launch", &free, &config);
        assert_eq!(free_grant, 1);
    }

    #[test]
    fn default_assignment_contacts_every_user_and_uses_both_message_arms() {
        let config = CampaignConfig::default();
        for cohort in [Cohort::Paid, Cohort::Free] {
            let mut seen = [false; 3];
            for user_id in 1..10_000 {
                let candidate = Candidate {
                    user_id,
                    telegram_user_id: i64::from(user_id),
                    language: None,
                    credits: 0,
                    cohort,
                };
                match assignment("launch", &candidate, &config).0 {
                    Variant::Holdout => seen[0] = true,
                    Variant::Message => seen[1] = true,
                    Variant::MessageCredit => seen[2] = true,
                }
            }
            assert_eq!(seen, [false, true, true]);
        }
    }

    #[test]
    fn credit_is_the_only_copy_difference_between_treatments() {
        for language in ["en", "ru"] {
            for copy_variant in [CopyVariant::A, CopyVariant::B, CopyVariant::C] {
                let without_credit = render_message(language, 0, copy_variant);
                let with_credit = render_message(language, 1, copy_variant);
                let addendum = format!("\n\n{}", render_credit_addendum(language, 1));
                assert_eq!(with_credit.replace(&addendum, ""), without_credit);
                assert!(!without_credit.contains("/buy1"));
                assert!(!with_credit.contains("/buy1"));
            }
        }
    }

    #[test]
    fn credit_addendum_pluralizes_grants() {
        assert!(render_credit_addendum("en", 1).contains("1 free analysis"));
        assert!(render_credit_addendum("en", 3).contains("3 free analyses"));
        assert!(render_credit_addendum("ru", 1).contains("1 бесплатный анализ"));
        assert!(render_credit_addendum("ru", 3).contains("3 бесплатных анализа"));
        assert!(render_credit_addendum("ru", 11).contains("11 бесплатных анализов"));
        assert!(render_credit_addendum("ru", 21).contains("21 бесплатный анализ"));
    }

    #[test]
    fn all_three_copy_variants_are_distinct() {
        let copies = [CopyVariant::A, CopyVariant::B, CopyVariant::C]
            .map(|copy| render_message("en", 0, copy));
        assert_ne!(copies[0], copies[1]);
        assert_ne!(copies[1], copies[2]);
        assert_ne!(copies[0], copies[2]);
        assert!(copies
            .iter()
            .all(|copy| copy.contains("Google") && copy.contains("Gemini")));
    }

    #[test]
    fn copy_assignment_is_stable_and_spans_all_variants() {
        let mut seen = [false; 3];
        for user_id in 1..1_000 {
            let candidate = Candidate {
                user_id,
                telegram_user_id: i64::from(user_id),
                language: None,
                credits: 0,
                cohort: Cohort::LegacyUnknown,
            };
            let first = copy_assignment("launch", &candidate, Variant::MessageCredit);
            let second = copy_assignment("launch", &candidate, Variant::MessageCredit);
            assert_eq!(first, second);
            match first.0 {
                CopyVariant::A => seen[0] = true,
                CopyVariant::B => seen[1] = true,
                CopyVariant::C => seen[2] = true,
            }
        }
        assert_eq!(seen, [true, true, true]);
    }

    #[test]
    fn legacy_users_receive_the_neutral_credit_offer() {
        let config = CampaignConfig {
            holdout_bps: 0,
            message_bps: 0,
            message_credit_bps: 10_000,
            free_credit: 2,
            ..CampaignConfig::default()
        };
        let candidate = Candidate {
            user_id: 42,
            telegram_user_id: 100,
            language: None,
            credits: 0,
            cohort: Cohort::LegacyUnknown,
        };
        assert_eq!(assignment("launch", &candidate, &config).1, 2);
        let copy = render_message("en", 2, CopyVariant::A);
        assert!(copy.contains("2 free analyses"));
        assert!(!copy.contains("/buy1"));
    }

    #[test]
    fn campaign_configuration_allows_only_copy_version_to_change() {
        let stored = CampaignConfig::default();
        let stored_json = serde_json::to_value(&stored).expect("config serializes");

        let mut new_copy = stored.clone();
        new_copy.copy_version = "future-copy-v2".to_string();
        assert!(configuration_matches_except_copy(&stored_json, &new_copy)
            .expect("stored config deserializes"));

        new_copy.cadence_seconds += 1;
        assert!(!configuration_matches_except_copy(&stored_json, &new_copy)
            .expect("stored config deserializes"));
    }
}
