use tg_main::user_manager::{UserManager, User};
use super::TestDatabase;

/// helper struct for creating test users with predictable IDs
pub struct TestUserBuilder {
    telegram_user_id: i64,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
}

impl TestUserBuilder {
    pub fn new(telegram_user_id: i64) -> Self {
        Self {
            telegram_user_id,
            username: None,
            first_name: None,
            last_name: None,
        }
    }

    pub fn username(mut self, username: &str) -> Self {
        self.username = Some(username.to_string());
        self
    }

    pub fn first_name(mut self, first_name: &str) -> Self {
        self.first_name = Some(first_name.to_string());
        self
    }

    pub fn last_name(mut self, last_name: &str) -> Self {
        self.last_name = Some(last_name.to_string());
        self
    }

    pub async fn create(
        &self,
        user_manager: &UserManager,
        referrer_user_id: Option<i32>,
    ) -> Result<User, Box<dyn std::error::Error + Send + Sync>> {
        let (user, _) = user_manager
            .get_or_create_user(
                self.telegram_user_id,
                self.username.as_deref(),
                self.first_name.as_deref(),
                self.last_name.as_deref(),
                referrer_user_id,
            )
            .await?;
        Ok(user)
    }
}

/// utility functions for test assertions
pub struct TestAssertions;

impl TestAssertions {
    /// verifies that a user has the expected number of referrals
    pub async fn assert_user_referral_count(
        db: &TestDatabase,
        user_id: i32,
        expected_count: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = db.pool.get().await?;
        let row = client
            .query_one(
                "SELECT referrals_count FROM users WHERE id = $1",
                &[&user_id],
            )
            .await?;
        
        let actual_count: i32 = row.get(0);
        assert_eq!(
            actual_count, expected_count,
            "Expected user {} to have {} referrals, but found {}",
            user_id, expected_count, actual_count
        );
        Ok(())
    }

    /// verifies that a user has the expected number of analysis credits
    pub async fn assert_user_credit_count(
        db: &TestDatabase,
        user_id: i32,
        expected_credits: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = db.pool.get().await?;
        let row = client
            .query_one(
                "SELECT analysis_credits FROM users WHERE id = $1",
                &[&user_id],
            )
            .await?;
        
        let actual_credits: i32 = row.get(0);
        assert_eq!(
            actual_credits, expected_credits,
            "Expected user {} to have {} credits, but found {}",
            user_id, expected_credits, actual_credits
        );
        Ok(())
    }

    /// verifies the number of referral rewards records for a user
    pub async fn assert_referral_reward_count(
        db: &TestDatabase,
        referrer_user_id: i32,
        reward_type: &str,
        expected_count: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = db.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM referral_rewards WHERE referrer_user_id = $1 AND reward_type = $2",
                &[&referrer_user_id, &reward_type],
            )
            .await?;
        
        let actual_count: i64 = row.get(0);
        assert_eq!(
            actual_count as i32, expected_count,
            "Expected user {} to have {} {} rewards, but found {}",
            referrer_user_id, expected_count, reward_type, actual_count
        );
        Ok(())
    }

    /// verifies that a user was referred by another user
    pub async fn assert_user_referred_by(
        db: &TestDatabase,
        user_id: i32,
        expected_referrer_id: Option<i32>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = db.pool.get().await?;
        let row = client
            .query_one(
                "SELECT referred_by_user_id FROM users WHERE id = $1",
                &[&user_id],
            )
            .await?;
        
        let actual_referrer_id: Option<i32> = row.get(0);
        assert_eq!(
            actual_referrer_id, expected_referrer_id,
            "Expected user {} to be referred by {:?}, but found {:?}",
            user_id, expected_referrer_id, actual_referrer_id
        );
        Ok(())
    }

    /// verifies that paid referrals count is correct
    pub async fn assert_paid_referral_count(
        db: &TestDatabase,
        user_id: i32,
        expected_count: i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = db.pool.get().await?;
        let row = client
            .query_one(
                "SELECT paid_referrals_count FROM users WHERE id = $1",
                &[&user_id],
            )
            .await?;
        
        let actual_count: i32 = row.get(0);
        assert_eq!(
            actual_count, expected_count,
            "Expected user {} to have {} paid referrals, but found {}",
            user_id, expected_count, actual_count
        );
        Ok(())
    }
}

/// helper for creating test scenarios
pub struct TestScenario;

impl TestScenario {
    /// creates a referrer and a specified number of unpaid referrals
    pub async fn create_referrer_with_unpaid_referrals(
        user_manager: &UserManager,
        referrer_telegram_id: i64,
        num_referrals: usize,
    ) -> Result<(User, Vec<User>), Box<dyn std::error::Error + Send + Sync>> {
        // create referrer
        let referrer = TestUserBuilder::new(referrer_telegram_id)
            .username("referrer")
            .first_name("Referrer")
            .last_name("User")
            .create(user_manager, None)
            .await?;

        let mut referrals = Vec::new();

        // create referrals
        for i in 0..num_referrals {
            let referral_telegram_id = referrer_telegram_id + 1000 + i as i64;
            let referral = TestUserBuilder::new(referral_telegram_id)
                .username(&format!("referral_{}", i))
                .first_name(&format!("Referral{}", i))
                .create(user_manager, Some(referrer.id))
                .await?;
            referrals.push(referral);
        }

        Ok((referrer, referrals))
    }

    /// creates a referrer with mixed paid and unpaid referrals
    pub async fn create_referrer_with_mixed_referrals(
        user_manager: &UserManager,
        referrer_telegram_id: i64,
        num_unpaid: usize,
        num_paid: usize,
    ) -> Result<(User, Vec<User>, Vec<User>), Box<dyn std::error::Error + Send + Sync>> {
        // create referrer
        let referrer = TestUserBuilder::new(referrer_telegram_id)
            .username("referrer")
            .first_name("Referrer")
            .last_name("User")
            .create(user_manager, None)
            .await?;

        let mut unpaid_referrals = Vec::new();
        let mut paid_referrals = Vec::new();

        // create unpaid referrals
        for i in 0..num_unpaid {
            let referral_telegram_id = referrer_telegram_id + 1000 + i as i64;
            let referral = TestUserBuilder::new(referral_telegram_id)
                .username(&format!("unpaid_referral_{}", i))
                .first_name(&format!("UnpaidReferral{}", i))
                .create(user_manager, Some(referrer.id))
                .await?;
            unpaid_referrals.push(referral);
        }

        // create paid referrals (these will make payments)
        for i in 0..num_paid {
            let referral_telegram_id = referrer_telegram_id + 2000 + i as i64;
            let referral = TestUserBuilder::new(referral_telegram_id)
                .username(&format!("paid_referral_{}", i))
                .first_name(&format!("PaidReferral{}", i))
                .create(user_manager, Some(referrer.id))
                .await?;
            
            // simulate payment by this referral
            user_manager.add_credits(referral.telegram_user_id, 1).await?;
            user_manager.record_paid_referral(referral.telegram_user_id).await?;
            
            paid_referrals.push(referral);
        }

        Ok((referrer, unpaid_referrals, paid_referrals))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::TestDatabase;

    #[tokio::test]
    async fn test_user_builder() {
        let db = TestDatabase::create_fresh().await.expect("Failed to create test database");
        let user_manager = UserManager::new(db.pool.clone());

        let user = TestUserBuilder::new(12345)
            .username("testuser")
            .first_name("Test")
            .last_name("User")
            .create(&user_manager, None)
            .await
            .expect("Failed to create user");

        assert_eq!(user.telegram_user_id, 12345);
        assert_eq!(user.username, Some("testuser".to_string()));
        assert_eq!(user.first_name, Some("Test".to_string()));
        assert_eq!(user.last_name, Some("User".to_string()));
        
        // cleanup test database
        db.cleanup().await.expect("Failed to cleanup test database");
    }

    #[tokio::test]
    async fn test_assertions() {
        let db = TestDatabase::create_fresh().await.expect("Failed to create test database");
        let user_manager = UserManager::new(db.pool.clone());

        let user = TestUserBuilder::new(12345)
            .create(&user_manager, None)
            .await
            .expect("Failed to create user");

        // test credit assertion
        TestAssertions::assert_user_credit_count(&db, user.id, 1)
            .await
            .expect("Credit count assertion failed");

        // test referral count assertion
        TestAssertions::assert_user_referral_count(&db, user.id, 0)
            .await
            .expect("Referral count assertion failed");
            
        // cleanup test database
        db.cleanup().await.expect("Failed to cleanup test database");
    }
}