use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use teloxide::{types::Message, Bot, RequestError};
use tg_main::{handlers::payment_handler::PaymentHandler, user_manager::UserManager};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::TestDatabase;

#[tokio::test]
async fn failed_payment_notification_still_rewards_referrer_once() {
    let db = TestDatabase::create_fresh().await.unwrap();
    let user_manager = Arc::new(UserManager::new(db.pool.clone()));
    let (referrer, _) = user_manager
        .get_or_create_user(100, None, None, None, None, None)
        .await
        .unwrap();
    let (purchaser, _) = user_manager
        .get_or_create_user(101, None, None, None, Some(referrer.id), None)
        .await
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_url = format!("http://{}/", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let server_requests = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0, "request ended before its body was received");
                request.extend_from_slice(&buffer[..count]);
                if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&request[..header_end]).unwrap();
                    let body_length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if request.len() >= header_end + 4 + body_length {
                        break;
                    }
                }
            }
            server_requests.fetch_add(1, Ordering::SeqCst);
            let body = r#"{"ok":false,"error_code":403,"description":"Forbidden: bot was blocked by the user"}"#;
            let response = format!(
                "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let bot = Arc::new(Bot::new("123:test-token").set_api_url(api_url.parse().unwrap()));
    let message: Message = serde_json::from_value(serde_json::json!({
        "message_id": 1,
        "date": 1_700_000_000,
        "from": {"id": 101, "is_bot": false, "first_name": "Purchaser"},
        "chat": {"id": 101, "type": "private", "first_name": "Purchaser"},
        "successful_payment": {
            "currency": "XTR",
            "total_amount": 100,
            "invoice_payload": "credits_1",
            "telegram_payment_charge_id": "notification-failure-charge",
            "provider_payment_charge_id": ""
        }
    }))
    .unwrap();
    let payment = message.successful_payment().unwrap().clone();
    let handler = PaymentHandler::new(user_manager);
    let first_result = handler
        .handle_successful_payment(bot.clone(), message.clone(), payment.clone())
        .await;

    let client = db.pool.get().await.unwrap();
    let state_query = "SELECT purchaser.analysis_credits, referrer.analysis_credits,
        referrer.paid_referrals_count, purchaser.first_payment_rewarded,
        (SELECT COUNT(*) FROM referral_rewards WHERE referee_user_id = $1 AND reward_type = 'paid_user'),
        (SELECT COUNT(*) FROM payments WHERE user_id = $1)
        FROM users purchaser, users referrer WHERE purchaser.id = $1 AND referrer.id = $2";
    let first_state = client
        .query_one(state_query, &[&purchaser.id, &referrer.id])
        .await
        .unwrap();
    let duplicate_result = handler
        .handle_successful_payment(bot, message, payment)
        .await;
    let duplicate_state = client
        .query_one(state_query, &[&purchaser.id, &referrer.id])
        .await
        .unwrap();
    server.abort();
    drop(client);
    db.cleanup().await.unwrap();

    assert!(matches!(first_result, Err(RequestError::Api(_))));
    assert!(duplicate_result.is_ok());
    for state in [first_state, duplicate_state] {
        assert_eq!(
            state.get::<_, i32>(0),
            2,
            "purchased credit is applied once"
        );
        assert_eq!(state.get::<_, i32>(1), 3, "paid referral earns one credit");
        assert_eq!(state.get::<_, i32>(2), 1);
        assert!(state.get::<_, bool>(3));
        assert_eq!(state.get::<_, i64>(4), 1);
        assert_eq!(state.get::<_, i64>(5), 1);
    }
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}
