use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionState {
    Idle,
    // Channel analysis flow
    ChannelAnalysisAwaitingInput,
    ChannelAnalysisSelectingType {
        channel_name: String,
    },
    // Group analysis flow
    GroupAnalysisSelectingGroup,
    GroupAnalysisSelectingType {
        chat_id: i64,
        group_name: String,
    },
    GroupAnalysisSelectingUser {
        chat_id: i64,
        group_name: String,
        analysis_type: String,
        available_users: Vec<crate::handlers::group_handler::GroupUser>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSession {
    pub user_id: i64,
    pub state: SessionState,
    pub last_updated: chrono::DateTime<chrono::Utc>,
}

pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<i64, UserSession>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn get_session(&self, user_id: i64) -> SessionState {
        let sessions = self.sessions.lock().await;
        sessions
            .get(&user_id)
            .map(|session| session.state.clone())
            .unwrap_or(SessionState::Idle)
    }

    pub async fn set_session(&self, user_id: i64, state: SessionState) {
        let mut sessions = self.sessions.lock().await;
        sessions.insert(
            user_id,
            UserSession {
                user_id,
                state,
                last_updated: chrono::Utc::now(),
            },
        );
    }

    pub async fn clear_session(&self, user_id: i64) {
        let mut sessions = self.sessions.lock().await;
        sessions.remove(&user_id);
    }

    // cleanup old sessions (older than 1 hour)
    #[allow(dead_code)]
    pub async fn cleanup_old_sessions(&self) {
        let mut sessions = self.sessions.lock().await;
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(1);
        sessions.retain(|_, session| session.last_updated > cutoff);
    }
}