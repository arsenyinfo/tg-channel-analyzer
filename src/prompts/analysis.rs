use crate::analysis::MessageDict;

pub fn generate_analysis_prompt(
    messages: &[MessageDict],
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // create a version of messages without image URLs for LLM analysis
    let messages_for_llm: Vec<MessageDict> = messages
        .iter()
        .map(|msg| {
            MessageDict {
                date: msg.date.clone(),
                message: msg.message.clone(),
                images: None, // exclude images from LLM analysis
            }
        })
        .collect();

    let messages_json = serde_json::to_string_pretty(&messages_for_llm)?;

    Ok(format!(
        "You are an expert analyst tasked with creating a comprehensive personality profile based on Telegram channel messages. Analyze the writing style, topics discussed, opinions expressed, and behavioral patterns to understand the author's character.

CRITICAL REQUIREMENTS:
1. Write in the same language as the messages (detect automatically)
2. Each section must be approximately 2048 characters long
3. Use ONLY the provided XML tags exactly as shown
4. Base analysis solely on the message content provided
5. Do not make assumptions about gender, age, or location unless clearly evident

OUTPUT FORMAT (use these exact tags):

<professional>
Write a detailed professional assessment suitable for a hiring manager. Focus on:
- Technical skills and expertise demonstrated
- Communication style and professionalism
- Leadership qualities or lack thereof
- Work ethic and reliability indicators
- Potential red flags or concerns for employers
- Industry knowledge and thought leadership
- Team collaboration potential

Tone: Formal, objective, balanced - highlight both strengths and weaknesses
Length: ~2048 characters
</professional>

<personal>
Write a psychological personality analysis for a general audience. Focus on:
- Core personality traits and characteristics
- Emotional intelligence and social skills
- Decision-making patterns and cognitive style
- Values, beliefs, and motivations
- Relationship patterns and social behavior
- Stress responses and coping mechanisms
- Growth mindset vs fixed mindset indicators

Tone: Insightful, empathetic, professional psychological assessment
Length: ~2048 characters
</personal>

<roast>
Write a sharp, witty critique as if from a close friend who knows them well. Focus on:
- Quirks, habits, and annoying tendencies
- Contradictions in their behavior or beliefs
- Pretentious or hypocritical moments
- Social media behavior and online persona
- Pet peeves others might have about them
- Blind spots and areas of self-delusion

Tone: Brutally honest, sharp humor, keeping in mind the cultural context (e.g. Eastern European directness)
Length: ~2048 characters
Note: Adjust harshness based on cultural context - Eastern Europeans typically appreciate more direct criticism
</roast>

ANALYSIS GUIDELINES:
- Look for patterns across multiple messages, not isolated incidents
- Consider context and nuance, not just surface-level content
- Identify both explicit statements and implied attitudes
- Note communication style: formal vs casual, technical vs accessible
- Observe emotional regulation and reaction patterns
- Consider the audience they're writing for and how they adapt their voice

Messages to analyze:
{}",
        messages_json
    ))
}
