use deadpool_postgres::Pool;
use log::info;
use tokio_postgres::Transaction;

pub struct MigrationManager;

impl MigrationManager {
    pub async fn run_migrations(
        pool: &Pool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("Running database migrations...");
        let mut client = pool.get().await?;
        let transaction = client.transaction().await?;
        transaction
            .query_one("SELECT pg_advisory_xact_lock(7623417990)", &[])
            .await?;

        // check if migrations table exists and create if not
        let needs_init = transaction
            .query_opt(
                "SELECT 1 FROM pg_tables WHERE schemaname = 'public' AND tablename = 'schema_migrations'",
                &[],
            )
            .await?
            .is_none();

        if needs_init {
            Self::initial_setup(&transaction).await?;
            info!("Initial database setup completed");
        }

        // check if we need to run any new migrations (always check, even after initial setup)
        let current_version: i32 = transaction
            .query_one("SELECT MAX(version) FROM schema_migrations", &[])
            .await?
            .get::<_, Option<i32>>(0)
            .unwrap_or(0);
        if current_version < Self::latest_version() {
            Self::run_pending_migrations(&transaction, current_version).await?;
            info!("Database migrations completed");
        } else {
            info!("Database schema is up to date");
        }
        transaction.commit().await?;

        Ok(())
    }

    async fn initial_setup(
        transaction: &Transaction<'_>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // create all tables and indexes in a single transaction
        let migration_sql = r#"
            -- Migration tracking table
            CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
            );

            -- Channel messages table
            CREATE TABLE channel_messages (
                id SERIAL PRIMARY KEY,
                channel_name VARCHAR(255) NOT NULL UNIQUE,
                messages_data JSONB NOT NULL,
                created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
                updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
            );

            -- LLM results table
            CREATE TABLE llm_results (
                id SERIAL PRIMARY KEY,
                cache_key VARCHAR(64) NOT NULL UNIQUE,
                analysis_result JSONB NOT NULL,
                created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
            );

            -- Users table
            CREATE TABLE users (
                id SERIAL PRIMARY KEY,
                telegram_user_id BIGINT NOT NULL UNIQUE,
                username VARCHAR(255),
                first_name VARCHAR(255),
                last_name VARCHAR(255),
                analysis_credits INTEGER NOT NULL DEFAULT 1,
                total_analyses_performed INTEGER NOT NULL DEFAULT 0,
                created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
                updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
            );

            -- User analyses table
            CREATE TABLE user_analyses (
                id SERIAL PRIMARY KEY,
                user_id INTEGER REFERENCES users(id),
                channel_name VARCHAR(255) NOT NULL,
                analysis_timestamp TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
                credits_used INTEGER NOT NULL DEFAULT 1
            );

            -- Create all indexes
            CREATE INDEX idx_channel_messages_name ON channel_messages(channel_name);
            CREATE INDEX idx_llm_results_key ON llm_results(cache_key);
            CREATE INDEX idx_channel_messages_updated ON channel_messages(updated_at);
            CREATE INDEX idx_llm_results_created ON llm_results(created_at);
            CREATE INDEX idx_users_telegram_id ON users(telegram_user_id);
            CREATE INDEX idx_user_analyses_user_id ON user_analyses(user_id);
            CREATE INDEX idx_user_analyses_timestamp ON user_analyses(analysis_timestamp);

            -- Record initial migration
            INSERT INTO schema_migrations (version) VALUES (1);
        "#;

        transaction.batch_execute(migration_sql).await?;
        Ok(())
    }

    fn latest_version() -> i32 {
        10 // increment this when adding new migrations
    }

    async fn run_pending_migrations(
        transaction: &Transaction<'_>,
        current_version: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for version in (current_version + 1)..=Self::latest_version() {
            match version {
                2 => {
                    // add user_analysis_choices table for tracking pending analysis requests
                    let migration_sql = r#"
                        CREATE TABLE user_analysis_choices (
                            id SERIAL PRIMARY KEY,
                            user_id INTEGER NOT NULL REFERENCES users(id),
                            telegram_user_id BIGINT NOT NULL,
                            channel_name VARCHAR(255) NOT NULL,
                            analysis_type VARCHAR(50) NOT NULL CHECK (analysis_type IN ('professional', 'personal', 'roast')),
                            created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
                        );

                        CREATE INDEX idx_user_analysis_choices_user_id ON user_analysis_choices(user_id);
                        CREATE INDEX idx_user_analysis_choices_telegram_id ON user_analysis_choices(telegram_user_id);
                        CREATE INDEX idx_user_analysis_choices_created ON user_analysis_choices(created_at);
                    "#;
                    transaction.batch_execute(migration_sql).await?;
                }
                3 => {
                    // add analysis_type field to user_analyses table and referral system
                    let migration_sql = r#"
                        ALTER TABLE user_analyses 
                        ADD COLUMN analysis_type VARCHAR(50) CHECK (analysis_type IN ('professional', 'personal', 'roast'));

                        -- Add referral tracking columns to users table
                        ALTER TABLE users 
                        ADD COLUMN referred_by_user_id INTEGER REFERENCES users(id),
                        ADD COLUMN referrals_count INTEGER NOT NULL DEFAULT 0,
                        ADD COLUMN paid_referrals_count INTEGER NOT NULL DEFAULT 0;

                        -- Create referral_rewards table for tracking credit awards
                        CREATE TABLE referral_rewards (
                            id SERIAL PRIMARY KEY,
                            referrer_user_id INTEGER NOT NULL REFERENCES users(id),
                            referee_user_id INTEGER NOT NULL REFERENCES users(id),
                            reward_type VARCHAR(20) NOT NULL CHECK (reward_type IN ('unpaid_milestone', 'paid_user')),
                            credits_awarded INTEGER NOT NULL,
                            created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
                        );

                        CREATE INDEX idx_referral_rewards_referrer ON referral_rewards(referrer_user_id);
                        CREATE INDEX idx_referral_rewards_referee ON referral_rewards(referee_user_id);
                        CREATE INDEX idx_users_referred_by ON users(referred_by_user_id);
                    "#;
                    transaction.batch_execute(migration_sql).await?;
                }
                4 => {
                    // add message queue table for bulk messaging and language field to users
                    let migration_sql = r#"
                        CREATE TABLE message_queue (
                            id SERIAL PRIMARY KEY,
                            telegram_user_id BIGINT NOT NULL,
                            message TEXT NOT NULL,
                            parse_mode VARCHAR(20) DEFAULT 'HTML',
                            status VARCHAR(20) DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
                            created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
                            sent_at TIMESTAMP WITH TIME ZONE,
                            error_message TEXT
                        );

                        CREATE INDEX idx_message_queue_status ON message_queue(status, created_at);

                        -- Add language field to users table
                        ALTER TABLE users ADD COLUMN language VARCHAR(2);

                        -- Add status column to user_analyses for task resumption
                        ALTER TABLE user_analyses ADD COLUMN status VARCHAR(20) DEFAULT 'completed' CHECK (status IN ('pending', 'completed', 'failed'));
                        CREATE INDEX idx_user_analyses_status ON user_analyses(status, analysis_timestamp);
                    "#;
                    transaction.batch_execute(migration_sql).await?;
                }
                5 => {
                    // add language column to user_analyses for localized recovery messages
                    let migration_sql = r#"
                        ALTER TABLE user_analyses ADD COLUMN language VARCHAR(2);
                    "#;
                    transaction.batch_execute(migration_sql).await?;
                }
                6 => {
                    // payment idempotency, referral reward idempotency, delivery tracking
                    let migration_sql = r#"
                        -- idempotent payment ledger keyed on the Telegram charge id
                        CREATE TABLE payments (
                            id SERIAL PRIMARY KEY,
                            telegram_payment_charge_id VARCHAR(255) NOT NULL UNIQUE,
                            user_id INTEGER NOT NULL REFERENCES users(id),
                            credits INTEGER NOT NULL,
                            stars_amount INTEGER NOT NULL,
                            invoice_payload VARCHAR(64) NOT NULL,
                            created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
                        );
                        CREATE INDEX idx_payments_user_id ON payments(user_id);

                        -- once-per-referee guard for the paid-referral bonus.
                        -- backfill existing users to TRUE: pre-migration paid history is not
                        -- recoverable per-referee (old paid_user rows stored referee=referrer),
                        -- so mark all existing users as already-rewarded to avoid duplicate
                        -- bonuses; only users created after this migration earn the bonus.
                        ALTER TABLE users ADD COLUMN first_payment_rewarded BOOLEAN NOT NULL DEFAULT FALSE;
                        UPDATE users SET first_payment_rewarded = TRUE;

                        -- ordinal milestone index for idempotent unpaid-milestone rewards
                        ALTER TABLE referral_rewards ADD COLUMN milestone INTEGER;

                        -- backfill existing milestone rows with a per-referrer ordinal so the
                        -- unique index below is satisfiable and future grants continue past them
                        WITH numbered AS (
                            SELECT id, ROW_NUMBER() OVER (
                                PARTITION BY referrer_user_id ORDER BY id
                            ) AS ord
                            FROM referral_rewards
                            WHERE reward_type = 'unpaid_milestone'
                        )
                        UPDATE referral_rewards r
                        SET milestone = numbered.ord
                        FROM numbered
                        WHERE r.id = numbered.id;

                        CREATE UNIQUE INDEX idx_referral_rewards_milestone
                        ON referral_rewards (referrer_user_id, milestone)
                        WHERE reward_type = 'unpaid_milestone';

                        -- delivery tracking so a charged-but-undelivered analysis can be reconciled.
                        -- backfill existing completed analyses as delivered, otherwise the startup
                        -- reconciliation would refund every historical completed analysis.
                        ALTER TABLE user_analyses ADD COLUMN delivered_at TIMESTAMP WITH TIME ZONE;
                        UPDATE user_analyses SET delivered_at = analysis_timestamp WHERE status = 'completed';
                    "#;
                    transaction.batch_execute(migration_sql).await?;
                }
                7 => {
                    // Telegram may redeliver the same callback query. Persist its globally
                    // unique ID on the analysis row so only one worker can claim it.
                    let migration_sql = r#"
                        ALTER TABLE user_analyses
                        ADD COLUMN telegram_callback_query_id VARCHAR(255);

                        CREATE UNIQUE INDEX idx_user_analyses_callback_query_id
                        ON user_analyses (telegram_callback_query_id);
                    "#;
                    transaction.batch_execute(migration_sql).await?;
                }
                8 => {
                    // Idempotent, scheduled campaigns and a restart-safe delivery queue.
                    let migration_sql = r#"
                        CREATE TABLE campaigns (
                            id BIGSERIAL PRIMARY KEY,
                            campaign_key VARCHAR(128) NOT NULL UNIQUE,
                            configuration JSONB NOT NULL,
                            timezone VARCHAR(64) NOT NULL,
                            send_window_start TIME NOT NULL,
                            send_window_end TIME NOT NULL,
                            cadence_seconds INTEGER NOT NULL CHECK (cadence_seconds > 0),
                            next_send_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
                            status VARCHAR(20) NOT NULL DEFAULT 'active'
                                CHECK (status IN ('active', 'paused', 'completed')),
                            created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
                            updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
                        );

                        CREATE TABLE campaign_recipients (
                            campaign_id BIGINT NOT NULL REFERENCES campaigns(id),
                            user_id INTEGER NOT NULL REFERENCES users(id),
                            cohort VARCHAR(20) NOT NULL
                                CHECK (cohort IN ('paid', 'free', 'legacy_unknown')),
                            variant VARCHAR(20) NOT NULL
                                CHECK (variant IN ('holdout', 'message', 'message_credit')),
                            credits_granted INTEGER NOT NULL DEFAULT 0 CHECK (credits_granted >= 0),
                            enrolled_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
                            PRIMARY KEY (campaign_id, user_id)
                        );
                        CREATE INDEX idx_campaign_recipients_user
                            ON campaign_recipients(user_id, campaign_id);

                        CREATE TABLE campaign_credit_grants (
                            campaign_id BIGINT NOT NULL REFERENCES campaigns(id),
                            user_id INTEGER NOT NULL REFERENCES users(id),
                            credits INTEGER NOT NULL CHECK (credits > 0),
                            granted_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
                            PRIMARY KEY (campaign_id, user_id)
                        );

                        CREATE TABLE campaign_suppressions (
                            telegram_user_id BIGINT PRIMARY KEY,
                            reason VARCHAR(64) NOT NULL,
                            created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
                        );

                        ALTER TABLE payments
                            ADD CONSTRAINT payments_credits_positive CHECK (credits > 0) NOT VALID,
                            ADD CONSTRAINT payments_stars_positive CHECK (stars_amount > 0) NOT VALID;

                        ALTER TABLE message_queue
                            ADD COLUMN campaign_id BIGINT REFERENCES campaigns(id),
                            ADD COLUMN user_id INTEGER REFERENCES users(id),
                            ADD COLUMN scheduled_at TIMESTAMP WITH TIME ZONE,
                            ADD COLUMN next_attempt_at TIMESTAMP WITH TIME ZONE,
                            ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0,
                            ADD COLUMN max_attempts INTEGER NOT NULL DEFAULT 6,
                            ADD COLUMN lease_token VARCHAR(64),
                            ADD COLUMN leased_until TIMESTAMP WITH TIME ZONE,
                            ADD COLUMN last_error_code VARCHAR(64);

                        UPDATE message_queue
                        SET status = COALESCE(status, 'pending'),
                            parse_mode = CASE
                                WHEN UPPER(COALESCE(parse_mode, 'HTML')) = 'HTML' THEN 'HTML'
                                WHEN LOWER(COALESCE(parse_mode, '')) IN ('markdownv2', 'markdown')
                                    THEN 'MarkdownV2'
                                ELSE 'HTML'
                            END,
                            scheduled_at = COALESCE(created_at, NOW()),
                            next_attempt_at = COALESCE(created_at, NOW());

                        ALTER TABLE message_queue DROP CONSTRAINT message_queue_status_check;
                        ALTER TABLE message_queue
                            ALTER COLUMN status SET NOT NULL,
                            ALTER COLUMN parse_mode SET NOT NULL,
                            ALTER COLUMN scheduled_at SET DEFAULT NOW(),
                            ALTER COLUMN scheduled_at SET NOT NULL,
                            ALTER COLUMN next_attempt_at SET DEFAULT NOW(),
                            ALTER COLUMN next_attempt_at SET NOT NULL,
                            ADD CONSTRAINT message_queue_status_check
                                CHECK (status IN ('pending', 'processing', 'sent', 'failed', 'delivery_unknown')),
                            ADD CONSTRAINT message_queue_parse_mode_check
                                CHECK (parse_mode IN ('HTML', 'MarkdownV2')),
                            ADD CONSTRAINT message_queue_attempts_check
                                CHECK (attempt_count >= 0 AND max_attempts > 0),
                            ADD CONSTRAINT message_queue_lease_state_check CHECK (
                                (status = 'processing' AND lease_token IS NOT NULL AND leased_until IS NOT NULL)
                                OR
                                (status <> 'processing' AND lease_token IS NULL AND leased_until IS NULL)
                            );

                        CREATE UNIQUE INDEX idx_message_queue_campaign_user
                            ON message_queue(campaign_id, user_id)
                            WHERE campaign_id IS NOT NULL;
                        CREATE INDEX idx_message_queue_due
                            ON message_queue(next_attempt_at, scheduled_at, id)
                            WHERE status = 'pending';
                        CREATE INDEX idx_message_queue_lease
                            ON message_queue(leased_until)
                            WHERE status = 'processing';
                        CREATE INDEX idx_message_queue_campaign_status
                            ON message_queue(campaign_id, status)
                            WHERE campaign_id IS NOT NULL;
                    "#;
                    transaction.batch_execute(migration_sql).await?;
                }
                9 => {
                    // Versioned three-arm assignment and provider-neutral LLM usage ledger.
                    let migration_sql = r#"
                        ALTER TABLE campaign_recipients
                            ADD COLUMN assignment_version VARCHAR(64),
                            ADD COLUMN assignment_bucket INTEGER,
                            ADD COLUMN baseline_credits INTEGER;

                        UPDATE campaign_recipients
                        SET assignment_version = 'legacy-conditional-v0',
                            baseline_credits = 0;

                        ALTER TABLE campaign_recipients
                            ALTER COLUMN assignment_version SET NOT NULL,
                            ALTER COLUMN baseline_credits SET NOT NULL,
                            ADD CONSTRAINT campaign_assignment_bucket_check
                                CHECK (assignment_bucket IS NULL OR assignment_bucket BETWEEN 0 AND 9999),
                            ADD CONSTRAINT campaign_baseline_credits_check
                                CHECK (baseline_credits >= 0);

                        ALTER TABLE user_analyses
                            ADD COLUMN result_source VARCHAR(16)
                                CHECK (result_source IN ('generated', 'cache')),
                            ADD COLUMN llm_cache_key VARCHAR(64),
                            ADD COLUMN experiment_campaign_id BIGINT REFERENCES campaigns(id);
                        CREATE INDEX idx_user_analyses_experiment_campaign
                            ON user_analyses(experiment_campaign_id, analysis_timestamp);

                        CREATE TABLE llm_attempts (
                            attempt_key VARCHAR(64) PRIMARY KEY,
                            generation_key VARCHAR(64) NOT NULL,
                            user_analysis_id INTEGER REFERENCES user_analyses(id) ON DELETE SET NULL,
                            operation VARCHAR(32) NOT NULL,
                            provider VARCHAR(32) NOT NULL,
                            model VARCHAR(128) NOT NULL,
                            model_stage VARCHAR(16) NOT NULL,
                            response_round INTEGER NOT NULL CHECK (response_round >= 0),
                            transport_round INTEGER NOT NULL CHECK (transport_round >= 0),
                            status VARCHAR(24) NOT NULL CHECK (status IN (
                                'started', 'succeeded', 'http_error', 'transport_error',
                                'timeout_unknown', 'response_invalid'
                            )),
                            consumer_outcome VARCHAR(16)
                                CHECK (consumer_outcome IN ('accepted', 'incomplete', 'unparsed')),
                            billing_certainty VARCHAR(16) NOT NULL CHECK (billing_certainty IN (
                                'known', 'unknown', 'not_billed'
                            )),
                            http_status INTEGER,
                            error_class VARCHAR(64),
                            provider_response_id VARCHAR(255),
                            model_version VARCHAR(128),
                            prompt_tokens BIGINT CHECK (prompt_tokens >= 0),
                            cached_content_tokens BIGINT CHECK (cached_content_tokens >= 0),
                            candidate_tokens BIGINT CHECK (candidate_tokens >= 0),
                            thought_tokens BIGINT CHECK (thought_tokens >= 0),
                            tool_prompt_tokens BIGINT CHECK (tool_prompt_tokens >= 0),
                            total_tokens BIGINT CHECK (total_tokens >= 0),
                            usage_metadata JSONB,
                            started_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
                            finished_at TIMESTAMP WITH TIME ZONE,
                            UNIQUE (generation_key, model_stage, response_round, transport_round)
                        );
                        CREATE INDEX idx_llm_attempts_analysis ON llm_attempts(user_analysis_id);
                        CREATE INDEX idx_llm_attempts_model_time ON llm_attempts(model, started_at);
                    "#;
                    transaction.batch_execute(migration_sql).await?;
                }
                10 => {
                    // Persist independently randomized message-copy assignment.
                    let migration_sql = r#"
                        ALTER TABLE campaign_recipients
                            ADD COLUMN copy_variant VARCHAR(1)
                                CHECK (copy_variant IN ('a', 'b', 'c')),
                            ADD COLUMN copy_assignment_bucket INTEGER
                                CHECK (copy_assignment_bucket IS NULL
                                    OR copy_assignment_bucket BETWEEN 0 AND 9999),
                            ADD COLUMN copy_version VARCHAR(64);

                        CREATE INDEX idx_campaign_recipients_copy
                            ON campaign_recipients
                                (campaign_id, copy_version, copy_variant, variant);
                    "#;
                    transaction.batch_execute(migration_sql).await?;
                }
                _ => {}
            }
            transaction
                .execute(
                    "INSERT INTO schema_migrations (version) VALUES ($1)",
                    &[&version],
                )
                .await?;
        }
        Ok(())
    }
}
