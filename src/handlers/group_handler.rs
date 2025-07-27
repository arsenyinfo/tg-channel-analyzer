use deadpool_postgres::Pool;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{ChatKind, ParseMode};
use chrono::{DateTime, Utc};

use crate::bot::BotContext;
use crate::prompts::group_analysis::generate_group_analysis_prompt;

#[derive(Debug)]
pub enum GroupManagerError {
    #[allow(dead_code)]
    GroupNotFound(i64),
    #[allow(dead_code)]
    UserNotMember(i64, i64),
    #[allow(dead_code)]
    InsufficientMessages(i64),
    DatabaseError(Box<dyn Error + Send + Sync>),
    #[allow(dead_code)]
    AnalysisInProgress(i64),
}

impl fmt::Display for GroupManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GroupManagerError::GroupNotFound(chat_id) => {
                write!(f, "Group with chat_id {} not found", chat_id)
            }
            GroupManagerError::UserNotMember(chat_id, user_id) => {
                write!(f, "User {} is not a member of group {}", user_id, chat_id)
            }
            GroupManagerError::InsufficientMessages(chat_id) => {
                write!(f, "Group {} has insufficient messages for analysis", chat_id)
            }
            GroupManagerError::DatabaseError(e) => write!(f, "Database error: {}", e),
            GroupManagerError::AnalysisInProgress(chat_id) => {
                write!(f, "Analysis already in progress for group {}", chat_id)
            }
        }
    }
}

impl Error for GroupManagerError {}

impl From<tokio_postgres::Error> for GroupManagerError {
    fn from(err: tokio_postgres::Error) -> Self {
        GroupManagerError::DatabaseError(Box::new(err))
    }
}

impl From<deadpool_postgres::PoolError> for GroupManagerError {
    fn from(err: deadpool_postgres::PoolError) -> Self {
        GroupManagerError::DatabaseError(Box::new(err))
    }
}

impl From<serde_json::Error> for GroupManagerError {
    fn from(err: serde_json::Error) -> Self {
        GroupManagerError::DatabaseError(Box::new(err))
    }
}

#[derive(Debug, Clone)]
pub struct GroupMessage {
    #[allow(dead_code)]
    pub id: Option<i32>,
    pub chat_id: i64,
    pub telegram_user_id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub message_text: String,
    pub message_id: Option<i64>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroupUser {
    pub telegram_user_id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub message_count: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserAnalysis {
    pub username: String,
    pub professional: String,
    pub personal: String,
    pub roast: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroupAnalysisData {
    pub roast: Option<String>,
    pub professional: Option<String>,
    pub personal: Option<String>,
    pub analyzed_users: Vec<GroupUser>,
    pub message_count: i32,
    pub analysis_timestamp: DateTime<Utc>,
}

#[derive(Clone)]
pub struct GroupHandler {
    pool: Arc<Pool>,
    max_messages_per_group: usize,
}

impl GroupHandler {
    pub fn new(pool: Arc<Pool>) -> Self {
        Self {
            pool,
            max_messages_per_group: 1000, // N = 1000 as per requirements
        }
    }

    pub async fn handle_group_message(
        &self,
        ctx: BotContext,
        msg: Message,
    ) -> ResponseResult<()> {
        let chat_id = msg.chat.id.0;
        let chat_title = match &msg.chat.kind {
            ChatKind::Public(chat) => chat.title.clone().unwrap_or_else(|| "Unknown".to_string()),
            _ => "Private Group".to_string(),
        };


        // store group metadata
        if let Err(e) = self.upsert_group_metadata(
            chat_id,
            Some(&chat_title),
            "group",
            None,
        ).await {
            warn!("Failed to update group metadata for {}: {}", chat_id, e);
        }

        // process text messages
        if let Some(text) = msg.text() {
            if let Some(from) = &msg.from {
                // skip bot messages
                if from.is_bot {
                    info!("Skipping bot message from user_id: {} in chat_id: {}", from.id.0, chat_id);
                    return Ok(());
                }

                info!("Processing text message from user_id: {} in chat_id: {}, text_preview: \"{}\"", 
                    from.id.0, chat_id, text.chars().take(50).collect::<String>());

                // store message in database
                let group_msg = GroupMessage {
                    id: None,
                    chat_id,
                    telegram_user_id: from.id.0 as i64,
                    username: from.username.clone(),
                    first_name: Some(from.first_name.clone()),
                    message_text: text.to_string(),
                    message_id: Some(msg.id.0 as i64),
                    timestamp: Utc::now(),
                };

                if let Err(e) = self.store_group_message(&group_msg).await {
                    error!("Failed to store group message: {}", e);
                }

                // update user membership
                if let Err(e) = self.update_user_membership(
                    chat_id,
                    from.id.0 as i64,
                    from.username.as_deref(),
                    Some(&from.first_name),
                ).await {
                    warn!("Failed to update user membership: {}", e);
                }

                // check if bot is mentioned (trigger analysis)
                if self.is_bot_mentioned(&ctx, text).await {
                    self.handle_bot_mention(ctx, msg, chat_id).await?;
                }
            }
        }

        Ok(())
    }

    // database operations
    async fn upsert_group_metadata(
        &self,
        chat_id: i64,
        title: Option<&str>,
        chat_type: &str,
        member_count: Option<i32>,
    ) -> Result<(), GroupManagerError> {
        let client = self.pool.get().await?;
        
        client
            .execute(
                "INSERT INTO group_chats (chat_id, title, chat_type, member_count, updated_at) 
                 VALUES ($1, $2, $3, $4, NOW()) 
                 ON CONFLICT (chat_id) 
                 DO UPDATE SET title = $2, chat_type = $3, member_count = $4, updated_at = NOW()",
                &[&chat_id, &title, &chat_type, &member_count],
            )
            .await?;

        Ok(())
    }

    async fn store_group_message(&self, message: &GroupMessage) -> Result<i32, GroupManagerError> {
        let client = self.pool.get().await?;
        
        info!("Storing message for chat_id: {}, user_id: {}, text_preview: \"{}\"", 
            message.chat_id, message.telegram_user_id, 
            message.message_text.chars().take(50).collect::<String>());
        
        // insert new message
        let message_id = client
            .query_one(
                "INSERT INTO group_messages (chat_id, telegram_user_id, username, first_name, message_text, message_id) 
                 VALUES ($1, $2, $3, $4, $5, $6) 
                 RETURNING id",
                &[
                    &message.chat_id,
                    &message.telegram_user_id,
                    &message.username,
                    &message.first_name,
                    &message.message_text,
                    &message.message_id,
                ],
            )
            .await?
            .get::<_, i32>(0);

        info!("Stored message with database id: {} for chat_id: {}", message_id, message.chat_id);

        // check total message count before cleanup
        let count_before = client
            .query_one(
                "SELECT COUNT(*) FROM group_messages WHERE chat_id = $1",
                &[&message.chat_id],
            )
            .await?
            .get::<_, i64>(0);

        info!("Total messages before cleanup for chat_id {}: {}", message.chat_id, count_before);

        // cleanup old messages, keeping only last N
        let deleted_rows = client
            .execute(
                "DELETE FROM group_messages 
                 WHERE chat_id = $1 
                 AND id NOT IN (
                     SELECT id FROM group_messages 
                     WHERE chat_id = $1 
                     ORDER BY timestamp DESC 
                     LIMIT $2
                 )",
                &[&message.chat_id, &(self.max_messages_per_group as i64)],
            )
            .await?;

        if deleted_rows > 0 {
            info!("Cleaned up {} old messages for chat_id: {}", deleted_rows, message.chat_id);
        }

        // check total message count after cleanup
        let count_after = client
            .query_one(
                "SELECT COUNT(*) FROM group_messages WHERE chat_id = $1",
                &[&message.chat_id],
            )
            .await?
            .get::<_, i64>(0);

        info!("Total messages after cleanup for chat_id {}: {}", message.chat_id, count_after);

        Ok(message_id)
    }

    async fn update_user_membership(
        &self,
        chat_id: i64,
        telegram_user_id: i64,
        username: Option<&str>,
        first_name: Option<&str>,
    ) -> Result<(), GroupManagerError> {
        let client = self.pool.get().await?;
        
        client
            .execute(
                "INSERT INTO group_memberships (chat_id, telegram_user_id, username, first_name, message_count, last_message_at) 
                 VALUES ($1, $2, $3, $4, 1, NOW()) 
                 ON CONFLICT (chat_id, telegram_user_id) 
                 DO UPDATE SET 
                     username = $3, 
                     first_name = $4, 
                     message_count = group_memberships.message_count + 1, 
                     last_message_at = NOW()",
                &[&chat_id, &telegram_user_id, &username, &first_name],
            )
            .await?;

        Ok(())
    }

    async fn is_bot_mentioned(&self, ctx: &BotContext, text: &str) -> bool {
        // get bot username
        match ctx.bot.get_me().await {
            Ok(bot_info) => {
                if let Some(ref username) = bot_info.username {
                    let bot_mention = format!("@{}", username);
                    text.contains(&bot_mention)
                } else {
                    warn!("Bot info retrieved but no username found");
                    false
                }
            },
            Err(e) => {
                error!("Failed to get bot info for mention detection: {}", e);
                false
            }
        }
    }

    async fn handle_bot_mention(
        &self,
        ctx: BotContext,
        msg: Message,
        chat_id: i64,
    ) -> ResponseResult<()> {
        // check if analysis already exists and is still valid
        match self.get_cached_analysis(chat_id).await {
            Ok(Some(analysis)) => {
                // check if cache is still valid (< M=50 new messages)
                let new_message_count = self.get_message_count_since(chat_id, analysis.analysis_timestamp).await
                    .unwrap_or(0);
                
                if new_message_count < 50 {
                    self.post_analysis_notification(ctx, msg, chat_id, &analysis.analyzed_users).await?;
                    return Ok(());
                }
            }
            _ => {} // no cached analysis or error fetching it
        }

        // get recent messages for analysis
        let messages = match self.get_recent_messages(chat_id, self.max_messages_per_group as i64).await {
            Ok(msgs) => msgs,
            Err(e) => {
                error!("Failed to get messages for group {}: {}", chat_id, e);
                ctx.bot.send_message(msg.chat.id, "❌ Failed to retrieve messages for analysis")
                    .await?;
                return Ok(());
            }
        };

        if messages.len() < 10 {
            ctx.bot.send_message(
                msg.chat.id,
                format!("❌ Not enough messages for analysis. Found {} messages (need at least 10). Please have members send more messages to the group first.", messages.len())
            ).await?;
            return Ok(());
        }

        // get top K active users
        let top_users = match self.get_top_active_users(chat_id, 10_i64).await {
            Ok(users) => users,
            Err(e) => {
                error!("Failed to get top users for group {}: {}", chat_id, e);
                ctx.bot.send_message(msg.chat.id, "❌ Failed to identify active users")
                    .await?;
                return Ok(());
            }
        };

        if top_users.is_empty() {
            ctx.bot.send_message(msg.chat.id, "❌ No active users found for analysis")
                .await?;
            return Ok(());
        }

        // send "analyzing..." message
        ctx.bot.send_message(
            msg.chat.id,
            format!("🔍 <b>Starting analysis...</b>\n\nAnalyzing {} messages from {} active members. This may take a moment.", 
                messages.len(), top_users.len())
        )
        .parse_mode(ParseMode::Html)
        .await?;

        // trigger actual LLM analysis
        let (analysis_data, per_user_analyses) = match self.perform_group_analysis(&messages, &top_users).await {
            Ok(result) => result,
            Err(e) => {
                error!("Failed to perform LLM analysis for group {}: {}", chat_id, e);
                ctx.bot.send_message(msg.chat.id, "❌ Analysis failed. Please try again later.")
                    .await?;
                return Ok(());
            }
        };

        // store analysis result
        if let Err(e) = self.store_group_analysis(chat_id, &analysis_data, &per_user_analyses).await {
            error!("Failed to store analysis for group {}: {}", chat_id, e);
            ctx.bot.send_message(msg.chat.id, "❌ Failed to store analysis results")
                .await?;
            return Ok(());
        }

        // post notification
        self.post_analysis_notification(ctx, msg, chat_id, &top_users).await?;
        
        Ok(())
    }

    async fn get_recent_messages(&self, chat_id: i64, limit: i64) -> Result<Vec<GroupMessage>, GroupManagerError> {
        let client = self.pool.get().await?;
        
        info!("Getting recent messages for chat_id: {}, limit: {}", chat_id, limit);
        
        let rows = client
            .query(
                "SELECT id, chat_id, telegram_user_id, username, first_name, message_text, message_id, timestamp 
                 FROM group_messages 
                 WHERE chat_id = $1 
                 ORDER BY timestamp DESC 
                 LIMIT $2",
                &[&chat_id, &limit],
            )
            .await?;

        info!("Retrieved {} rows from database for chat_id: {}", rows.len(), chat_id);

        let messages: Vec<GroupMessage> = rows
            .into_iter()
            .map(|row| GroupMessage {
                id: Some(row.get(0)),
                chat_id: row.get(1),
                telegram_user_id: row.get(2),
                username: row.get(3),
                first_name: row.get(4),
                message_text: row.get(5),
                message_id: row.get(6),
                timestamp: row.get(7),
            })
            .collect();

        info!("Converted {} messages for chat_id: {}", messages.len(), chat_id);
        for (i, msg) in messages.iter().take(3).enumerate() {
            info!("Message {}: user_id={}, text_preview=\"{}\"", 
                i + 1, msg.telegram_user_id, 
                msg.message_text.chars().take(50).collect::<String>());
        }

        Ok(messages)
    }

    async fn get_top_active_users(&self, chat_id: i64, limit: i64) -> Result<Vec<GroupUser>, GroupManagerError> {
        let client = self.pool.get().await?;
        
        let rows = client
            .query(
                "SELECT telegram_user_id, username, first_name, message_count 
                 FROM group_memberships 
                 WHERE chat_id = $1 
                 ORDER BY message_count DESC 
                 LIMIT $2",
                &[&chat_id, &limit],
            )
            .await?;

        let users: Vec<GroupUser> = rows
            .into_iter()
            .map(|row| GroupUser {
                telegram_user_id: row.get(0),
                username: row.get(1),
                first_name: row.get(2),
                message_count: row.get(3),
            })
            .collect();

        Ok(users)
    }

    async fn get_cached_analysis(&self, chat_id: i64) -> Result<Option<GroupAnalysisData>, GroupManagerError> {
        let client = self.pool.get().await?;
        
        let row = client
            .query_opt(
                "SELECT analysis_data, analyzed_users, message_count_when_analyzed, created_at 
                 FROM group_analyses 
                 WHERE chat_id = $1 
                 ORDER BY created_at DESC 
                 LIMIT 1",
                &[&chat_id],
            )
            .await?;

        if let Some(row) = row {
            let _analysis_data: serde_json::Value = row.get(0);
            let analyzed_users: serde_json::Value = row.get(1);
            let message_count: i32 = row.get(2);
            let created_at: DateTime<Utc> = row.get(3);

            // deserialize the stored analysis
            let users: Vec<GroupUser> = serde_json::from_value(analyzed_users)?;
            
            // the analysis_data now contains per-user analysis in new format
            // for backward compatibility, we'll return None for the combined fields
            let analysis = GroupAnalysisData {
                roast: None,
                professional: None,
                personal: None,
                analyzed_users: users,
                message_count,
                analysis_timestamp: created_at,
            };

            Ok(Some(analysis))
        } else {
            Ok(None)
        }
    }

    async fn get_message_count_since(&self, chat_id: i64, since: DateTime<Utc>) -> Result<i32, GroupManagerError> {
        let client = self.pool.get().await?;
        
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM group_messages WHERE chat_id = $1 AND timestamp > $2",
                &[&chat_id, &since],
            )
            .await?;

        Ok(row.get::<_, i64>(0) as i32)
    }

    async fn store_group_analysis(&self, chat_id: i64, analysis: &GroupAnalysisData, per_user_analyses: &HashMap<i64, UserAnalysis>) -> Result<i32, GroupManagerError> {
        let client = self.pool.get().await?;
        
        // store per-user analysis data in the new structure
        let analysis_json = serde_json::to_value(per_user_analyses)?;

        let analyzed_users_json = serde_json::to_value(&analysis.analyzed_users)?;

        let analysis_id = client
            .query_one(
                "INSERT INTO group_analyses (chat_id, analysis_data, analyzed_users, message_count_when_analyzed) 
                 VALUES ($1, $2, $3, $4) 
                 RETURNING id",
                &[&chat_id, &analysis_json, &analyzed_users_json, &analysis.message_count],
            )
            .await?
            .get::<_, i32>(0);

        Ok(analysis_id)
    }

    async fn post_analysis_notification(
        &self,
        ctx: BotContext,
        msg: Message,
        _chat_id: i64,
        analyzed_users: &[GroupUser],
    ) -> ResponseResult<()> {
        // create mentions for analyzed users
        let user_mentions: Vec<String> = analyzed_users
            .iter()
            .take(3) // limit mentions to avoid spam
            .map(|user| {
                if let Some(username) = &user.username {
                    format!("@{}", username)
                } else if let Some(first_name) = &user.first_name {
                    format!("{}", first_name)
                } else {
                    format!("User {}", user.telegram_user_id)
                }
            })
            .collect();

        let total_analyzed = analyzed_users.len();
        let mentions_text = if total_analyzed > 3 {
            format!("{} and {} others", user_mentions.join(", "), total_analyzed - 3)
        } else {
            user_mentions.join(", ")
        };

        let notification_msg = format!(
            "✅ <b>Group analysis ready!</b>\n\nAnalysis completed for: {}\n\n💡 <b>Message me privately to view results for 1 credit each</b>",
            mentions_text
        );

        ctx.bot.send_message(msg.chat.id, notification_msg)
            .parse_mode(ParseMode::Html)
            .await?;

        Ok(())
    }

    async fn perform_group_analysis(
        &self,
        messages: &[GroupMessage],
        top_users: &[GroupUser],
    ) -> Result<(GroupAnalysisData, HashMap<i64, UserAnalysis>), GroupManagerError> {
        // generate the analysis prompt
        let prompt = generate_group_analysis_prompt(messages, top_users)
            .map_err(|e| GroupManagerError::DatabaseError(e))?;

        // perform LLM analysis - get raw JSON response
        let json_response = self.query_group_analysis_json(&prompt).await
            .map_err(|e| GroupManagerError::DatabaseError(e))?;

        // parse JSON response to per-user analysis
        let per_user_analyses = self.parse_per_user_analysis(&json_response)
            .map_err(|e| GroupManagerError::DatabaseError(e))?;

        // convert to the expected storage format - store per-user data 
        let group_analysis = GroupAnalysisData {
            roast: None,  // will be populated from per_user_analyses when needed
            professional: None,
            personal: None,
            analyzed_users: top_users.to_vec(),
            message_count: messages.len() as i32,
            analysis_timestamp: Utc::now(),
        };

        Ok((group_analysis, per_user_analyses))
    }

    async fn query_group_analysis_json(
        &self,
        prompt: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        use crate::llm::query_llm;

        // try gemini-2.5-flash first
        match query_llm(prompt, "gemini-2.5-flash").await {
            Ok(response) => Ok(response.content),
            Err(e) => {
                warn!("Gemini Flash failed for group analysis: {}, trying fallback", e);
                // fallback to gemini-2.5-pro
                match query_llm(prompt, "gemini-2.5-pro").await {
                    Ok(response) => Ok(response.content),
                    Err(e) => {
                        error!("Gemini Pro fallback also failed for group analysis: {}", e);
                        Err(e)
                    }
                }
            }
        }
    }

    fn parse_per_user_analysis(
        &self,
        json_response: &str,
    ) -> Result<HashMap<i64, UserAnalysis>, Box<dyn std::error::Error + Send + Sync>> {
        // extract JSON from response if it contains extra text
        let json_start = json_response.find('{').ok_or("No JSON found in response")?;
        let json_end = json_response.rfind('}').ok_or("Invalid JSON in response")? + 1;
        let json_content = &json_response[json_start..json_end];

        // parse the JSON response
        let parsed: HashMap<String, serde_json::Value> = serde_json::from_str(json_content)?;
        
        let mut result = HashMap::new();
        
        for (user_key, user_data) in parsed {
            // extract user_id from key like "user_12345"
            if let Some(user_id_str) = user_key.strip_prefix("user_") {
                if let Ok(user_id) = user_id_str.parse::<i64>() {
                    if let Ok(analysis) = serde_json::from_value::<UserAnalysis>(user_data) {
                        result.insert(user_id, analysis);
                    } else {
                        warn!("Failed to parse user analysis for user_id: {}", user_id);
                    }
                } else {
                    warn!("Invalid user_id format in key: {}", user_key);
                }
            } else {
                warn!("Invalid user key format: {}", user_key);
            }
        }
        
        Ok(result)
    }

    // public methods for private message integration
    pub async fn get_user_groups(&self, telegram_user_id: i64) -> Result<Vec<i64>, GroupManagerError> {
        let client = self.pool.get().await?;
        
        let rows = client
            .query(
                "SELECT DISTINCT chat_id FROM group_memberships WHERE telegram_user_id = $1",
                &[&telegram_user_id],
            )
            .await?;

        let chat_ids: Vec<i64> = rows.into_iter().map(|row| row.get(0)).collect();
        Ok(chat_ids)
    }

    pub async fn get_available_analyses(&self, chat_id: i64) -> Result<Option<GroupAnalysisData>, GroupManagerError> {
        self.get_cached_analysis(chat_id).await
    }

    pub async fn get_available_analyses_with_id(&self, chat_id: i64) -> Result<Option<(GroupAnalysisData, i32)>, GroupManagerError> {
        let client = self.pool.get().await?;
        
        let row = client
            .query_opt(
                "SELECT id, analysis_data, analyzed_users, message_count_when_analyzed, created_at 
                 FROM group_analyses 
                 WHERE chat_id = $1 
                 ORDER BY created_at DESC 
                 LIMIT 1",
                &[&chat_id],
            )
            .await?;

        if let Some(row) = row {
            let analysis_id: i32 = row.get(0);
            let _analysis_data: serde_json::Value = row.get(1);
            let analyzed_users: serde_json::Value = row.get(2);
            let message_count: i32 = row.get(3);
            let created_at: DateTime<Utc> = row.get(4);

            // deserialize the stored analysis
            let users: Vec<GroupUser> = serde_json::from_value(analyzed_users)?;
            
            // the analysis_data now contains per-user analysis in new format
            // for backward compatibility, we'll return None for the combined fields
            let analysis = GroupAnalysisData {
                roast: None,
                professional: None,
                personal: None,
                analyzed_users: users,
                message_count,
                analysis_timestamp: created_at,
            };

            Ok(Some((analysis, analysis_id)))
        } else {
            Ok(None)
        }
    }

    pub async fn get_individual_user_analysis(
        &self,
        chat_id: i64,
        user_id: i64,
        analysis_type: &str,
    ) -> Result<Option<String>, GroupManagerError> {
        let client = self.pool.get().await?;
        
        let row = client
            .query_opt(
                "SELECT analysis_data FROM group_analyses 
                 WHERE chat_id = $1 
                 ORDER BY created_at DESC 
                 LIMIT 1",
                &[&chat_id],
            )
            .await?;

        if let Some(row) = row {
            let analysis_data: serde_json::Value = row.get(0);
            
            // parse the per-user analysis structure
            let user_key = format!("{}", user_id);
            if let Some(user_analysis) = analysis_data.get(&user_key) {
                if let Some(content) = user_analysis.get(analysis_type).and_then(|v| v.as_str()) {
                    return Ok(Some(content.to_string()));
                }
            }
        }

        Ok(None)
    }

    pub async fn get_group_name(&self, chat_id: i64) -> Result<Option<String>, GroupManagerError> {
        let client = self.pool.get().await?;
        
        let row = client
            .query_opt(
                "SELECT title FROM group_chats WHERE chat_id = $1",
                &[&chat_id],
            )
            .await?;

        Ok(row.map(|r| r.get::<_, Option<String>>(0)).flatten())
    }

}