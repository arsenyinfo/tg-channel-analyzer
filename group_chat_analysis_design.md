# Group Chat Analysis Feature Design

## Overview

The group chat analysis feature allows the Telegram bot to analyze group conversations and provide personality insights about the most active members. The bot monitors group messages, performs AI-powered analysis when mentioned, and allows group members to access individual analysis results for credits.

## Key Parameters

- **N = 1000**: Maximum messages stored per group
- **K = 3-10**: Number of top active users analyzed (determined by LLM)
- **M = 50**: New message threshold for cache invalidation
- Cost: **1 credit** per analysis reveal (running analysis is free)

## Architecture

### 1. Message Collection Flow

```
Group Message → Bot.handle_message() → GroupHandler.handle_group_message()
                                           ↓
                                    Store in group_messages table
                                           ↓
                                    Update group_memberships
                                           ↓
                                    Check for bot mention
```

### 2. Analysis Trigger Flow

```
Bot Mention Detected → Check cached analysis
                          ↓
                    If cache valid (<50 new messages) → Post notification
                          ↓
                    If cache invalid → Perform new analysis
                                           ↓
                                    Get recent 1000 messages
                                           ↓
                                    Identify top K active users
                                           ↓
                                    Generate LLM analysis
                                           ↓
                                    Store in group_analyses
                                           ↓
                                    Post notification
```

### 3. Analysis Access Flow

```
User DMs Bot → Check common groups → Show available analyses
                  ↓
            Select group → Select analysis type → Select user
                                                       ↓
                                                Consume 1 credit
                                                       ↓
                                                Show analysis
```

## Database Schema (Migration 5)

### group_chats
- `chat_id` (BIGINT): Telegram chat ID
- `title` (VARCHAR): Group name
- `chat_type` (VARCHAR): Type of chat
- `member_count` (INTEGER): Number of members
- `updated_at` (TIMESTAMP): Last update time

### group_messages
- `id` (SERIAL): Primary key
- `chat_id` (BIGINT): Group chat ID
- `telegram_user_id` (BIGINT): User who sent message
- `username` (VARCHAR): Telegram username
- `first_name` (VARCHAR): User's first name
- `message_text` (TEXT): Message content
- `message_id` (BIGINT): Telegram message ID
- `timestamp` (TIMESTAMP): Message timestamp

### group_memberships
- `chat_id` (BIGINT): Group chat ID
- `telegram_user_id` (BIGINT): User ID
- `username` (VARCHAR): Current username
- `first_name` (VARCHAR): Current first name
- `message_count` (INTEGER): Total messages sent
- `last_message_at` (TIMESTAMP): Last activity

### group_analyses
- `id` (SERIAL): Primary key
- `chat_id` (BIGINT): Group chat ID
- `analysis_data` (JSONB): Analysis results (roast, professional, personal)
- `analyzed_users` (JSONB): Array of analyzed users
- `message_count_when_analyzed` (INTEGER): Messages analyzed
- `created_at` (TIMESTAMP): Analysis timestamp

### group_analysis_access
- `user_id` (INTEGER): User who accessed analysis
- `group_analysis_id` (INTEGER): Analysis accessed
- `accessed_at` (TIMESTAMP): Access timestamp

## Key Components

### GroupHandler (`handlers/group_handler.rs`)

Main handler for group messages with key methods:
- `handle_group_message()`: Process incoming group messages
- `store_group_message()`: Store messages and cleanup old ones
- `handle_bot_mention()`: Trigger analysis when bot is mentioned
- `perform_group_analysis()`: Execute LLM analysis
- `get_user_groups()`: Get groups where user is member
- `get_available_analyses()`: Get cached analyses for a group

### Group Analysis Prompts (`prompts/group_analysis.rs`)

Generates prompts for LLM with three analysis types:
- **Professional**: Work-related qualities, leadership, expertise
- **Personal**: Personality traits, social dynamics, emotional intelligence
- **Roast**: Humorous, sharp observations about quirks and habits

### Callback Handler Integration

Handles the multi-step flow for accessing analyses:
1. `handle_show_group_analysis_callback()`: Initial group selection
2. `handle_group_selection_callback()`: Select analysis type
3. `handle_group_analysis_type_callback()`: Select user to analyze
4. `handle_group_user_selection_callback()`: Deliver analysis and charge credit

## Key Features

### Message Storage
- Stores last 1000 messages per group
- Automatic cleanup of older messages
- Excludes bot messages from storage

### Smart Caching
- Reuses analysis if less than 50 new messages since last analysis
- Prevents redundant LLM calls
- Analysis stored forever in database

### Access Control
- Only group members can access analyses
- Credit-based access (1 credit per reveal)
- Tracks who accessed which analyses

### Multi-language Support
- Analysis generated in same language as group messages
- Language detection handled by LLM

### Performance Optimization
- Message count tracking for quick user activity assessment
- Indexed database queries for fast retrieval
- Concurrent analysis prevention via state management

## Session States

The feature uses several session states to track user flow:
- `GroupAnalysisSelectingGroup`: User selecting which group
- `GroupAnalysisSelectingType`: User selecting analysis type
- `GroupAnalysisSelectingUser`: User selecting which member to analyze

## Analysis Types

1. **Professional Analysis** (~1500-2000 chars)
   - Leadership dynamics
   - Technical expertise
   - Communication professionalism
   - Collaborative behaviors
   - Workplace suitability

2. **Personal Analysis** (~1500-2000 chars)
   - Social roles (organizer, entertainer, mediator)
   - Emotional intelligence
   - Conflict resolution styles
   - Personal values and beliefs
   - Relationship patterns

3. **Roast Analysis** (~1500-2000 chars)
   - Communication quirks
   - Contradictions and hypocrisies
   - Annoying/endearing traits
   - Group dynamics created
   - Meme-worthiness

## Usage Flow

1. **Setup**: Add bot to group as member
2. **Collection**: Bot automatically stores messages
3. **Trigger**: Mention bot with @botname
4. **Analysis**: Bot analyzes top active users
5. **Notification**: Bot posts "Analysis ready for @user1, @user2..."
6. **Access**: Users DM bot to view analyses (1 credit each)

## Edge Cases Handled

- Bot messages excluded from analysis
- Groups with insufficient messages (<10)
- Multiple simultaneous analysis requests (locked)
- Users not in common groups
- Missing usernames/names (fallback to user ID)
- Analysis type not available (e.g., non-professional chat)

## Future Considerations

- Opt-out mechanism for users who don't want to be analyzed
- Minimum member requirements for groups
- Cooldown between analyses (currently uses M=50 message threshold)
- Bulk analysis pricing options
- Export analysis results feature