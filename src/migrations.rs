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

        // check if migrations table exists and create if not
        let needs_init = client
            .query_opt(
                "SELECT 1 FROM pg_tables WHERE schemaname = 'public' AND tablename = 'schema_migrations'",
                &[],
            )
            .await?
            .is_none();

        if needs_init {
            // first time setup - create everything in a single transaction
            let transaction = client.transaction().await?;
            Self::initial_setup(&transaction).await?;
            transaction.commit().await?;
            info!("Initial database setup completed");
        }
        
        // check if we need to run any new migrations (always check, even after initial setup)
        let current_version = Self::get_current_version(&mut client).await?;
        if current_version < Self::latest_version() {
            let transaction = client.transaction().await?;
            Self::run_pending_migrations(&transaction, current_version).await?;
            transaction.commit().await?;
            info!("Database migrations completed");
        } else {
            info!("Database schema is up to date");
        }

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

    async fn get_current_version(
        client: &deadpool_postgres::Object,
    ) -> Result<i32, Box<dyn std::error::Error + Send + Sync>> {
        let row = client
            .query_one("SELECT MAX(version) FROM schema_migrations", &[])
            .await?;
        Ok(row.get::<_, Option<i32>>(0).unwrap_or(0))
    }

    fn latest_version() -> i32 {
        3 // increment this when adding new migrations
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
