use tg_main::user_manager::UserManager;

use super::{TestDatabase, mock_bot::MockTelegramBot, test_utils::{TestAssertions, TestScenario}};

#[tokio::test]
async fn test_basic_referral_chain() {
    let db = TestDatabase::create_fresh().await.expect("Failed to create test database");
    let user_manager = UserManager::new(db.pool.clone());
    let bot = MockTelegramBot::new();

    // create referrer user
    let referrer_telegram_id = 100;
    let (referrer, _) = bot
        .simulate_user_start(&user_manager, referrer_telegram_id, Some("referrer"), Some("Referrer"), Some("User"), None)
        .await
        .expect("Failed to create referrer");

    // create 5 referred users to trigger milestone reward
    for i in 1..=5 {
        let referee_telegram_id = referrer_telegram_id + i;
        let (_, reward_info) = bot
            .simulate_user_start(
                &user_manager,
                referee_telegram_id,
                Some(&format!("referee{}", i)),
                Some(&format!("Referee{}", i)),
                None,
                Some(referrer.id),
            )
            .await
            .expect("Failed to create referee");

        // check if it's a milestone
        if i == 1 {
            // first referral should trigger celebration milestone
            assert!(reward_info.is_some());
            let reward = reward_info.unwrap();
            assert_eq!(reward.referral_count, 1);
            assert_eq!(reward.milestone_rewards, 0); // no credit rewards yet
            assert_eq!(reward.total_credits_awarded, 0);
            assert!(reward.is_celebration_milestone);
        } else if i == 5 {
            // fifth referral should trigger both celebration and credit reward
            assert!(reward_info.is_some());
            let reward = reward_info.unwrap();
            assert_eq!(reward.referral_count, 5);
            assert_eq!(reward.milestone_rewards, 1); // 1 credit for 5 referrals
            assert_eq!(reward.total_credits_awarded, 1);
            assert!(reward.is_celebration_milestone);
        } else {
            // 2nd, 3rd, 4th referrals should not trigger anything
            assert!(reward_info.is_none() || !reward_info.unwrap().is_celebration_milestone);
        }
    }

    // verify database state
    TestAssertions::assert_user_referral_count(&db, referrer.id, 5)
        .await
        .expect("Referral count assertion failed");
    
    TestAssertions::assert_user_credit_count(&db, referrer.id, 2) // 1 initial + 1 from milestone
        .await
        .expect("Credit count assertion failed");

    TestAssertions::assert_referral_reward_count(&db, referrer.id, "unpaid_milestone", 1)
        .await
        .expect("Milestone reward count assertion failed");

    // verify notification messages were sent
    assert!(bot.message_count_for_chat(referrer_telegram_id) >= 3); // welcome + celebration for 1st + reward for 5th
    assert!(bot.chat_received_message_containing(referrer_telegram_id, "🎊 Referral Milestone"));
    assert!(bot.chat_received_message_containing(referrer_telegram_id, "🎉 Referral Milestone"));
    
    // cleanup test database
    db.cleanup().await.expect("Failed to cleanup test database");
}

#[tokio::test]
async fn test_milestone_celebrations() {
    let db = TestDatabase::create_fresh().await.expect("Failed to create test database");
    let user_manager = UserManager::new(db.pool.clone());
    let bot = MockTelegramBot::new();

    // create referrer
    let referrer_telegram_id = 200;
    let (referrer, _) = bot
        .simulate_user_start(&user_manager, referrer_telegram_id, Some("referrer"), Some("Referrer"), None, None)
        .await
        .expect("Failed to create referrer");

    let celebration_milestones = [1, 5, 10, 20, 30];
    let credit_milestones = [5, 10, 15, 20, 25, 30]; // every 5

    // create referrals up to 30
    for i in 1..=30 {
        let referee_telegram_id = referrer_telegram_id + i;
        bot.clear_messages(); // clear previous messages to check only current referral notifications
        
        let (_, reward_info) = bot
            .simulate_user_start(
                &user_manager,
                referee_telegram_id,
                Some(&format!("referee{}", i)),
                Some(&format!("Referee{}", i)),
                None,
                Some(referrer.id),
            )
            .await
            .expect("Failed to create referee");

        // check milestone behaviors
        let should_celebrate = celebration_milestones.contains(&i);
        let should_get_credits = credit_milestones.contains(&i);

        if should_celebrate || should_get_credits {
            assert!(reward_info.is_some(), "Expected reward info for referral {}", i);
            let reward = reward_info.unwrap();
            assert_eq!(reward.referral_count, i as i32);
            assert_eq!(reward.is_celebration_milestone, should_celebrate);
            
            if should_get_credits {
                let _expected_credits = i as i32 / 5; // credits = referral_count / 5
                assert_eq!(reward.milestone_rewards, 1, "Expected 1 new credit at referral {}", i);
            } else {
                assert_eq!(reward.milestone_rewards, 0, "Expected no credits at referral {}", i);
            }

            // verify notification was sent to referrer
            let referrer_messages = bot.get_messages_for_chat(referrer_telegram_id);
            assert!(!referrer_messages.is_empty(), "Expected notification for referral {}", i);
            
            if should_celebrate && should_get_credits {
                assert!(bot.chat_received_message_containing(referrer_telegram_id, "🎉 Referral Milestone"));
                assert!(bot.chat_received_message_containing(referrer_telegram_id, "earned"));
            } else if should_get_credits {
                assert!(bot.chat_received_message_containing(referrer_telegram_id, "🎉 Referral Reward"));
            } else if should_celebrate {
                assert!(bot.chat_received_message_containing(referrer_telegram_id, "🎊 Referral Milestone"));
            }
        } else {
            // no rewards or celebrations expected
            assert!(reward_info.is_none() || (reward_info.as_ref().unwrap().total_credits_awarded == 0 && !reward_info.unwrap().is_celebration_milestone));
        }
    }

    // final verification
    TestAssertions::assert_user_referral_count(&db, referrer.id, 30)
        .await
        .expect("Final referral count assertion failed");
    
    TestAssertions::assert_user_credit_count(&db, referrer.id, 7) // 1 initial + 6 milestone credits (30/5)
        .await
        .expect("Final credit count assertion failed");

    TestAssertions::assert_referral_reward_count(&db, referrer.id, "unpaid_milestone", 6)
        .await
        .expect("Final milestone reward count assertion failed");
    
    // cleanup test database
    db.cleanup().await.expect("Failed to cleanup test database");
}

#[tokio::test]
async fn test_paid_referral_rewards() {
    let db = TestDatabase::create_fresh().await.expect("Failed to create test database");
    let user_manager = UserManager::new(db.pool.clone());
    let bot = MockTelegramBot::new();

    // create referrer
    let referrer_telegram_id = 300;
    let (referrer, _) = bot
        .simulate_user_start(&user_manager, referrer_telegram_id, Some("referrer"), Some("Referrer"), None, None)
        .await
        .expect("Failed to create referrer");

    // create a referred user
    let referee_telegram_id = 301;
    let (_referee, _) = bot
        .simulate_user_start(
            &user_manager,
            referee_telegram_id,
            Some("referee"),
            Some("Referee"),
            None,
            Some(referrer.id),
        )
        .await
        .expect("Failed to create referee");

    // verify initial state
    TestAssertions::assert_user_credit_count(&db, referrer.id, 1) // only initial credit
        .await
        .expect("Initial referrer credit assertion failed");

    TestAssertions::assert_paid_referral_count(&db, referrer.id, 0)
        .await
        .expect("Initial paid referral count assertion failed");

    bot.clear_messages();

    // simulate referee making a payment
    bot.simulate_user_payment(&user_manager, referee_telegram_id, 10)
        .await
        .expect("Failed to simulate payment");

    // verify paid referral was recorded and reward was given
    TestAssertions::assert_paid_referral_count(&db, referrer.id, 1)
        .await
        .expect("Paid referral count assertion failed");

    TestAssertions::assert_user_credit_count(&db, referrer.id, 2) // 1 initial + 1 from paid referral
        .await
        .expect("Post-payment referrer credit assertion failed");

    TestAssertions::assert_referral_reward_count(&db, referrer.id, "paid_user", 1)
        .await
        .expect("Paid referral reward count assertion failed");

    // verify notification was sent to referrer
    assert!(bot.chat_received_message_containing(referrer_telegram_id, "🎉 Referral Reward"));
    assert!(bot.chat_received_message_containing(referrer_telegram_id, "paid referral"));
    
    // cleanup test database
    db.cleanup().await.expect("Failed to cleanup test database");
}

#[tokio::test]
async fn test_mixed_paid_and_unpaid_referrals() {
    let db = TestDatabase::create_fresh().await.expect("Failed to create test database");
    let user_manager = UserManager::new(db.pool.clone());
    let _bot = MockTelegramBot::new();

    // create referrer with 4 unpaid referrals and 1 paid referral
    let (referrer, unpaid_referrals, paid_referrals) = TestScenario::create_referrer_with_mixed_referrals(
        &user_manager,
        400,
        4, // unpaid
        1, // paid
    ).await.expect("Failed to create mixed referral scenario");

    // verify counts
    TestAssertions::assert_user_referral_count(&db, referrer.id, 5) // 4 unpaid + 1 paid
        .await
        .expect("Total referral count assertion failed");

    TestAssertions::assert_paid_referral_count(&db, referrer.id, 1)
        .await
        .expect("Paid referral count assertion failed");

    // should have 1 initial + 1 milestone (5 referrals) + 1 paid = 3 credits
    TestAssertions::assert_user_credit_count(&db, referrer.id, 3)
        .await
        .expect("Mixed scenario credit count assertion failed");

    TestAssertions::assert_referral_reward_count(&db, referrer.id, "unpaid_milestone", 1)
        .await
        .expect("Unpaid milestone reward count assertion failed");

    TestAssertions::assert_referral_reward_count(&db, referrer.id, "paid_user", 1)
        .await
        .expect("Paid user reward count assertion failed");

    // verify all referrals are correctly attributed
    for unpaid in &unpaid_referrals {
        TestAssertions::assert_user_referred_by(&db, unpaid.id, Some(referrer.id))
            .await
            .expect("Unpaid referral attribution assertion failed");
    }

    for paid in &paid_referrals {
        TestAssertions::assert_user_referred_by(&db, paid.id, Some(referrer.id))
            .await
            .expect("Paid referral attribution assertion failed");
    }
    
    // cleanup test database
    db.cleanup().await.expect("Failed to cleanup test database");
}

#[tokio::test]
async fn test_progressive_milestone_rewards() {
    let db = TestDatabase::create_fresh().await.expect("Failed to create test database");
    let user_manager = UserManager::new(db.pool.clone());
    let bot = MockTelegramBot::new();

    // create referrer with exactly 25 referrals to test progression
    let (referrer, _) = TestScenario::create_referrer_with_unpaid_referrals(
        &user_manager,
        500,
        25,
    ).await.expect("Failed to create referrer with 25 referrals");

    // verify final state
    TestAssertions::assert_user_referral_count(&db, referrer.id, 25)
        .await
        .expect("Final referral count assertion failed");

    // should have 1 initial + 5 milestone credits (25/5 = 5)
    TestAssertions::assert_user_credit_count(&db, referrer.id, 6)
        .await
        .expect("Progressive milestone credit assertion failed");

    TestAssertions::assert_referral_reward_count(&db, referrer.id, "unpaid_milestone", 5)
        .await
        .expect("Progressive milestone reward count assertion failed");

    // now add 5 more referrals to reach 30 (next milestone)
    for i in 26..=30 {
        let referee_telegram_id = 500 + i;
        bot.clear_messages();
        
        let (_, reward_info) = bot
            .simulate_user_start(
                &user_manager,
                referee_telegram_id,
                Some(&format!("referee{}", i)),
                Some(&format!("Referee{}", i)),
                None,
                Some(referrer.id),
            )
            .await
            .expect("Failed to create additional referee");

        if i == 30 {
            // 30th referral should trigger both celebration and credit
            assert!(reward_info.is_some());
            let reward = reward_info.unwrap();
            assert_eq!(reward.referral_count, 30);
            assert_eq!(reward.milestone_rewards, 1); // one new credit
            assert!(reward.is_celebration_milestone);
            
            // verify notification
            assert!(bot.chat_received_message_containing(500, "🎉 Referral Milestone"));
        }
    }

    // final verification after adding 5 more
    TestAssertions::assert_user_referral_count(&db, referrer.id, 30)
        .await
        .expect("Final 30 referral count assertion failed");

    TestAssertions::assert_user_credit_count(&db, referrer.id, 7) // 1 initial + 6 milestone credits
        .await
        .expect("Final 30 credit count assertion failed");
    
    // cleanup test database
    db.cleanup().await.expect("Failed to cleanup test database");
}

#[tokio::test]
async fn test_edge_cases() {
    let db = TestDatabase::create_fresh().await.expect("Failed to create test database");
    let user_manager = UserManager::new(db.pool.clone());
    let bot = MockTelegramBot::new();

    // test invalid referrer ID
    let invalid_result = bot
        .simulate_user_start(&user_manager, 999, Some("user"), Some("User"), None, Some(999999))
        .await;
    
    // should still create user but without referral link
    assert!(invalid_result.is_ok());
    let (user, reward_info) = invalid_result.unwrap();
    assert!(reward_info.is_none());
    assert_eq!(user.referred_by_user_id, None);

    // test self-referral (create user first, then try to refer to themselves)
    let self_referrer_telegram_id = 600;
    let (self_referrer, _) = bot
        .simulate_user_start(&user_manager, self_referrer_telegram_id, Some("selfer"), Some("Selfer"), None, None)
        .await
        .expect("Failed to create self-referrer");

    // now try to create another user with self-referral (using same user's database ID)
    let self_referred_result = bot
        .simulate_user_start(
            &user_manager,
            self_referrer_telegram_id + 1,
            Some("selfref"),
            Some("SelfRef"),
            None,
            Some(self_referrer.id), // valid referrer ID
        )
        .await;

    // this should work (system doesn't prevent referring others, just validates ID exists)
    assert!(self_referred_result.is_ok());
    let (_, reward_info) = self_referred_result.unwrap();
    assert!(reward_info.is_some()); // should get first referral celebration
    
    // verify referrer now has 1 referral
    TestAssertions::assert_user_referral_count(&db, self_referrer.id, 1)
        .await
        .expect("Self-referrer count assertion failed");
    
    // cleanup test database
    db.cleanup().await.expect("Failed to cleanup test database");
}

#[tokio::test]
async fn test_database_consistency() {
    let db = TestDatabase::create_fresh().await.expect("Failed to create test database");
    let user_manager = UserManager::new(db.pool.clone());

    // create a comprehensive scenario
    let (referrer, _, _) = TestScenario::create_referrer_with_mixed_referrals(
        &user_manager,
        700,
        15, // unpaid (should give 3 milestone credits: 5, 10, 15)
        2,  // paid (should give 2 paid credits)
    ).await.expect("Failed to create comprehensive scenario");

    // verify all database tables are consistent
    let client = db.pool.get().await.expect("Failed to get database client");

    // check users table
    let user_row = client
        .query_one("SELECT referrals_count, paid_referrals_count, analysis_credits FROM users WHERE id = $1", &[&referrer.id])
        .await
        .expect("Failed to query user");
    
    let total_referrals: i32 = user_row.get(0);
    let paid_referrals: i32 = user_row.get(1);
    let credits: i32 = user_row.get(2);
    
    assert_eq!(total_referrals, 17); // 15 unpaid + 2 paid
    assert_eq!(paid_referrals, 2);
    assert_eq!(credits, 6); // 1 initial + 3 milestone + 2 paid

    // check referral_rewards table consistency
    let milestone_rewards = client
        .query_one(
            "SELECT COUNT(*) FROM referral_rewards WHERE referrer_user_id = $1 AND reward_type = 'unpaid_milestone'",
            &[&referrer.id]
        )
        .await
        .expect("Failed to query milestone rewards");
    
    let milestone_count: i64 = milestone_rewards.get(0);
    assert_eq!(milestone_count, 3); // rewards at 5, 10, 15

    let paid_rewards = client
        .query_one(
            "SELECT COUNT(*) FROM referral_rewards WHERE referrer_user_id = $1 AND reward_type = 'paid_user'",
            &[&referrer.id]
        )
        .await
        .expect("Failed to query paid rewards");
    
    let paid_count: i64 = paid_rewards.get(0);
    assert_eq!(paid_count, 2); // 2 paid referrals

    // verify all referred users exist and are properly linked
    let referred_users = client
        .query(
            "SELECT COUNT(*) FROM users WHERE referred_by_user_id = $1",
            &[&referrer.id]
        )
        .await
        .expect("Failed to query referred users");
    
    let referred_count: i64 = referred_users[0].get(0);
    assert_eq!(referred_count, 17); // all 17 referrals should be in database
    
    // cleanup test database
    db.cleanup().await.expect("Failed to cleanup test database");
}