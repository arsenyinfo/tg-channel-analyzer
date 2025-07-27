use crate::handlers::group_handler::{GroupMessage, GroupUser};

pub fn generate_group_analysis_prompt(
    messages: &[GroupMessage],
    top_users: &[GroupUser],
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // create a simplified message format for LLM analysis
    let messages_for_llm: Vec<serde_json::Value> = messages.iter().map(|msg| {
        serde_json::json!({
            "timestamp": msg.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            "username": msg.username.as_deref().unwrap_or("unknown"),
            "first_name": msg.first_name.as_deref().unwrap_or("User"),
            "text": msg.message_text
        })
    }).collect();
    
    let top_users_for_llm: Vec<serde_json::Value> = top_users.iter().map(|user| {
        serde_json::json!({
            "username": user.username.as_deref().unwrap_or("unknown"),
            "first_name": user.first_name.as_deref().unwrap_or("User"),
            "message_count": user.message_count
        })
    }).collect();
    
    let messages_json = serde_json::to_string_pretty(&messages_for_llm)?;
    let users_json = serde_json::to_string_pretty(&top_users_for_llm)?;

    Ok(format!(
        "You are an expert group dynamics analyst tasked with analyzing a Telegram group chat and creating individual personality profiles for the most active members.

CRITICAL REQUIREMENTS:
1. Write in the same language as the messages (detect automatically)
2. Focus ONLY on the top active users provided in the user list
3. Each individual user analysis should be approximately 1500-2000 characters
4. Use ONLY the provided JSON structure exactly as shown
5. Base analysis solely on the message content and user activity provided
6. Select 3-8 users from the top active list to analyze (pick the most interesting/active ones)
7. Return VALID JSON only - no extra text before or after

TOP ACTIVE USERS TO POTENTIALLY ANALYZE:
{}

OUTPUT FORMAT - RETURN ONLY THIS JSON STRUCTURE:
{{
  \"user_12345\": {{
    \"username\": \"actual_username_or_name\",
    \"professional\": \"Professional analysis of this specific user's work-related qualities, leadership dynamics, technical expertise, communication professionalism, collaborative behaviors, industry insights, and team suitability. Focus on: Leadership emergence, knowledge sharing, communication clarity, collaboration vs competition, professional development discussions, team fit, and any concerning behaviors. (~1500-2000 chars)\",
    \"personal\": \"Personal analysis of this specific user's personality traits and social dynamics. Focus on: Social role (organizer/entertainer/mediator), emotional intelligence, empathy in interactions, conflict resolution style, humor style, support behaviors, personal values expressed, relationship patterns, and social connectivity. (~1500-2000 chars)\",
    \"roast\": \"Witty, sharp observations about this specific user as a close friend would make. Focus on: Communication quirks, contradictions between beliefs and actions, annoying/endearing traits, self-presentation vs reality, group dynamics they create, meme potential, predictable unpredictability. Keep playful, not mean-spirited. (~1500-2000 chars)\"
  }},
  \"user_67890\": {{
    \"username\": \"another_username_or_name\",
    \"professional\": \"...\",
    \"personal\": \"...\",
    \"roast\": \"...\"
  }}
}}

ANALYSIS GUIDELINES:
- Replace \"user_12345\" with actual telegram_user_id from the user list
- Use the actual username or first_name as the \"username\" field value
- Each user gets individual, detailed analysis in all three categories
- Prioritize users with meaningful participation over just message count
- Look for unique communication patterns that distinguish each user
- Consider group dynamics: who responds to whom, who initiates topics
- Note leadership styles, conflict resolution, and social contributions
- Identify both explicit personality traits and behavioral patterns
- Consider cultural context and language used in the group

Recent Group Messages:
{}",
        users_json,
        messages_json
    ))
}