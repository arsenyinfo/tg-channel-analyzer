use deadpool_postgres::Pool;
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum UserManagerError {
    InsufficientCredits(i64), // telegram_user_id
    UserNotFound(i64),        // telegram_user_id
    DatabaseError(Box<dyn Error + Send + Sync>),
}

impl fmt::Display for UserManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UserManagerError::InsufficientCredits(user_id) => {
                write!(f, "User {} has insufficient credits", user_id)
            }
            UserManagerError::UserNotFound(user_id) => {
                write!(f, "User {} not found", user_id)
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
}

pub struct UserManager {
    pool: Pool,
}

impl UserManager {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// gets existing user or creates new user with default credits
    pub async fn get_or_create_user(
        &self,
        telegram_user_id: i64,
        username: Option<&str>,
        first_name: Option<&str>,
        last_name: Option<&str>,
    ) -> Result<User, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;

        // try to get existing user first
        if let Some(row) = client
            .query_opt(
                "SELECT id, telegram_user_id, username, first_name, last_name, analysis_credits, total_analyses_performed 
                 FROM users WHERE telegram_user_id = $1",
                &[&telegram_user_id],
            )
            .await?
        {
            let user = User {
                id: row.get(0),
                telegram_user_id: row.get(1),
                username: row.get(2),
                first_name: row.get(3),
                last_name: row.get(4),
                analysis_credits: row.get(5),
                total_analyses_performed: row.get(6),
            };
            info!("Found existing user: {} (credits: {})", telegram_user_id, user.analysis_credits);
            return Ok(user);
        }

        // create new user with default credits
        let row = client
            .query_one(
                "INSERT INTO users (telegram_user_id, username, first_name, last_name, analysis_credits, total_analyses_performed) 
                 VALUES ($1, $2, $3, $4, 1, 0) 
                 RETURNING id, telegram_user_id, username, first_name, last_name, analysis_credits, total_analyses_performed",
                &[&telegram_user_id, &username, &first_name, &last_name],
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
        };

        info!(
            "Created new user: {} with {} credits",
            telegram_user_id, user.analysis_credits
        );
        Ok(user)
    }

    /// consumes one credit and records the analysis
    pub async fn consume_credit(
        &self,
        telegram_user_id: i64,
        channel_name: &str,
    ) -> Result<i32, UserManagerError> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;

        // check and update credits atomically
        let row = transaction
            .query_opt(
                "UPDATE users SET analysis_credits = analysis_credits - 1, total_analyses_performed = total_analyses_performed + 1, updated_at = NOW() 
                 WHERE telegram_user_id = $1 AND analysis_credits > 0 
                 RETURNING id, analysis_credits",
                &[&telegram_user_id],
            )
            .await?;

        let (user_id, remaining_credits) = match row {
            Some(row) => (row.get::<_, i32>(0), row.get::<_, i32>(1)),
            None => {
                // check if user exists to provide more specific error before rolling back
                let user_exists = transaction
                    .query_opt(
                        "SELECT 1 FROM users WHERE telegram_user_id = $1",
                        &[&telegram_user_id],
                    )
                    .await?
                    .is_some();
                
                transaction.rollback().await?;
                
                return if user_exists {
                    Err(UserManagerError::InsufficientCredits(telegram_user_id))
                } else {
                    Err(UserManagerError::UserNotFound(telegram_user_id))
                };
            }
        };

        // record the analysis in audit trail
        transaction
            .execute(
                "INSERT INTO user_analyses (user_id, channel_name, credits_used) VALUES ($1, $2, 1)",
                &[&user_id, &channel_name],
            )
            .await?;

        transaction.commit().await?;

        info!(
            "User {} consumed 1 credit for channel {}, remaining: {}",
            telegram_user_id, channel_name, remaining_credits
        );
        Ok(remaining_credits)
    }

    /// adds credits to user (for future payment integration)
    pub async fn add_credits(
        &self,
        telegram_user_id: i64,
        credits_to_add: i32,
    ) -> Result<i32, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;

        let row = client
            .query_opt(
                "UPDATE users SET analysis_credits = analysis_credits + $2, updated_at = NOW() 
                 WHERE telegram_user_id = $1 
                 RETURNING analysis_credits",
                &[&telegram_user_id, &credits_to_add],
            )
            .await?;

        match row {
            Some(row) => {
                let new_balance: i32 = row.get(0);
                info!(
                    "Added {} credits to user {}, new balance: {}",
                    credits_to_add, telegram_user_id, new_balance
                );
                Ok(new_balance)
            }
            None => {
                error!("User {} not found when adding credits", telegram_user_id);
                Err("User not found".into())
            }
        }
    }
}
