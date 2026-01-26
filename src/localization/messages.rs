/// supported languages for the bot UI
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    En,
    Ru,
}

impl Lang {
    /// creates Lang from Telegram's language_code (e.g., "ru", "en", "uk")
    pub fn from_code(code: Option<&str>) -> Self {
        match code {
            Some("ru") => Lang::Ru,
            _ => Lang::En,
        }
    }
}

// =============================================================================
// Error messages
// =============================================================================

impl Lang {
    pub fn error_account_access(&self) -> &'static str {
        match self {
            Lang::En => {
                "❌ Sorry, there was an error accessing your account. Please try again later."
            }
            Lang::Ru => {
                "❌ Извините, произошла ошибка при доступе к вашему аккаунту. Попробуйте позже."
            }
        }
    }

    pub fn error_processing_request(&self) -> &'static str {
        match self {
            Lang::En => "❌ Error processing user request. Please try again later.",
            Lang::Ru => "❌ Ошибка обработки запроса. Попробуйте позже.",
        }
    }

    pub fn error_check_credits(&self) -> &'static str {
        match self {
            Lang::En => "❌ Failed to check credits. Please try again.",
            Lang::Ru => "❌ Не удалось проверить кредиты. Попробуйте снова.",
        }
    }

    pub fn error_start_analysis(&self) -> &'static str {
        match self {
            Lang::En => "❌ Failed to start analysis. Please try again.",
            Lang::Ru => "❌ Не удалось начать анализ. Попробуйте снова.",
        }
    }

    pub fn error_user_not_found(&self) -> &'static str {
        match self {
            Lang::En => "❌ User not found. Please try again.",
            Lang::Ru => "❌ Пользователь не найден. Попробуйте снова.",
        }
    }

    pub fn error_insufficient_credits(&self) -> &'static str {
        match self {
            Lang::En => "❌ Insufficient credits. Please purchase more credits to continue.",
            Lang::Ru => "❌ Недостаточно кредитов. Пожалуйста, купите кредиты для продолжения.",
        }
    }

    pub fn error_system(&self) -> &'static str {
        match self {
            Lang::En => "❌ Analysis failed due to a system error. Please try again later.",
            Lang::Ru => "❌ Анализ не удался из-за системной ошибки. Попробуйте позже.",
        }
    }

    pub fn error_payment_processing(&self) -> &'static str {
        match self {
            Lang::En => "❌ Error processing payment. Please contact support.",
            Lang::Ru => "❌ Ошибка обработки платежа. Свяжитесь с поддержкой.",
        }
    }

    pub fn error_payment_credits(&self) -> &'static str {
        match self {
            Lang::En => "⚠️ Payment received but failed to add credits. Please contact support with your payment ID.",
            Lang::Ru => "⚠️ Платёж получен, но не удалось добавить кредиты. Свяжитесь с поддержкой, указав ID платежа.",
        }
    }

    pub fn error_invalid_channel(&self) -> &'static str {
        match self {
            Lang::En => "❓ Please send a valid channel username starting with '@' (e.g., @channelname)\n\nUse /start to see the full instructions.",
            Lang::Ru => "❓ Отправьте корректное имя канала, начинающееся с '@' (например, @channelname)\n\nИспользуйте /start для просмотра инструкций.",
        }
    }

    pub fn error_analysis_prepare(&self, channel_name: &str) -> String {
        match self {
            Lang::En => format!(
                "❌ <b>Analysis Error</b>\n\n\
                Failed to prepare analysis for channel {}. This could happen if:\n\
                • The channel is private/restricted\n\
                • The channel doesn't exist\n\
                • There are network connectivity issues\n\n\
                No credits were consumed for this request.",
                channel_name
            ),
            Lang::Ru => format!(
                "❌ <b>Ошибка анализа</b>\n\n\
                Не удалось подготовить анализ для канала {}. Возможные причины:\n\
                • Канал приватный/ограниченный\n\
                • Канал не существует\n\
                • Проблемы с сетью\n\n\
                Кредиты не были списаны.",
                channel_name
            ),
        }
    }

    pub fn error_no_messages(&self) -> &'static str {
        match self {
            Lang::En => {
                "❌ <b>Analysis Error</b>\n\n\
                No messages found in the channel. This could happen if:\n\
                • The channel is private/restricted\n\
                • The channel has no recent messages\n\
                • There are network connectivity issues\n\n\
                No credits were consumed for this request."
            }
            Lang::Ru => {
                "❌ <b>Ошибка анализа</b>\n\n\
                В канале не найдено сообщений. Возможные причины:\n\
                • Канал приватный/ограниченный\n\
                • В канале нет недавних сообщений\n\
                • Проблемы с сетью\n\n\
                Кредиты не были списаны."
            }
        }
    }

    pub fn error_prompt_generation(&self) -> &'static str {
        match self {
            Lang::En => "❌ <b>Analysis Error</b>\n\nFailed to generate analysis prompt. No credits were consumed.",
            Lang::Ru => "❌ <b>Ошибка анализа</b>\n\nНе удалось сгенерировать промпт. Кредиты не были списаны.",
        }
    }

    pub fn error_ai_service(&self) -> &'static str {
        match self {
            Lang::En => "❌ <b>Analysis Error</b>\n\nFailed to complete analysis due to AI service issues. Please try again later.\n\nNo credits were consumed for this request.",
            Lang::Ru => "❌ <b>Ошибка анализа</b>\n\nНе удалось завершить анализ из-за проблем с AI-сервисом. Попробуйте позже.\n\nКредиты не были списаны.",
        }
    }

    pub fn error_no_analysis_content(&self, analysis_type: &str) -> String {
        match self {
            Lang::En => format!(
                "❌ No {} analysis content was generated. Please try again.",
                analysis_type
            ),
            Lang::Ru => format!(
                "❌ Не удалось сгенерировать {} анализ. Попробуйте снова.",
                self.analysis_type_name(analysis_type)
            ),
        }
    }
}

// =============================================================================
// Welcome / Start messages
// =============================================================================

impl Lang {
    pub fn welcome_no_credits(
        &self,
        user_id: i32,
        single_price: u32,
        bulk_price: u32,
        bulk_discount: u32,
        referral_info: &str,
    ) -> String {
        match self {
            Lang::En => format!(
                "🤖 <b><a href=\"https://t.me/ScratchAuthorEgoBot?start={user_id}\">@ScratchAuthorEgoBot</a> - Channel Analyzer</b>\n\n\
                Welcome! I can analyze Telegram channels and provide insights.\n\n\
                📋 <b>How to use:</b>\n\
                • Send me a channel username (e.g., <code>@channelname</code>)\n\
                • I'll validate the channel and show analysis options\n\
                • Choose your preferred analysis type\n\
                • Get detailed results in seconds!\n\n\
                ⚠️ <b>Note:</b> Only text content is analyzed. Channels with mostly images or videos may not work well.\n\n\
                ⚡ <b>Analysis Types:</b>\n\
                • 💼 Professional: Expert assessment for hiring\n\
                • 🧠 Personal: Psychological profile insights\n\
                • 🔥 Roast: Fun, brutally honest critique\n\n\
                💰 <b>Pricing:</b>\n\
                • 1 analysis: {single_price} ⭐ stars\n\
                • 10 analyses: {bulk_price} ⭐ stars (save {bulk_discount} stars!)\n\n\
                🎁 <b>Referral Program:</b> {referral_info}\n\
                Share your link: <code>https://t.me/ScratchAuthorEgoBot?start={user_id}</code>\n\
                • Get credits at milestones: 1, 5, 10, 20, 30...\n\
                • Get 1 credit for each paid referral\n\n\
                Choose a package below or just send me a channel name to get started!"
            ),
            Lang::Ru => format!(
                "🤖 <b><a href=\"https://t.me/ScratchAuthorEgoBot?start={user_id}\">@ScratchAuthorEgoBot</a> - Анализатор каналов</b>\n\n\
                Добро пожаловать! Я анализирую Telegram-каналы и предоставляю инсайты.\n\n\
                📋 <b>Как использовать:</b>\n\
                • Отправьте имя канала (например, <code>@channelname</code>)\n\
                • Я проверю канал и покажу варианты анализа\n\
                • Выберите тип анализа\n\
                • Получите результаты за секунды!\n\n\
                ⚠️ <b>Важно:</b> Анализируется только текст. Каналы с фото/видео могут не подойти.\n\n\
                ⚡ <b>Типы анализа:</b>\n\
                • 💼 Профессиональный: оценка для найма\n\
                • 🧠 Личностный: психологический профиль\n\
                • 🔥 Роаст: весёлая, честная критика\n\n\
                💰 <b>Цены:</b>\n\
                • 1 анализ: {single_price} ⭐ звёзд\n\
                • 10 анализов: {bulk_price} ⭐ звёзд (экономия {bulk_discount} звёзд!)\n\n\
                🎁 <b>Реферальная программа:</b> {referral_info}\n\
                Ваша ссылка: <code>https://t.me/ScratchAuthorEgoBot?start={user_id}</code>\n\
                • Кредиты на этапах: 1, 5, 10, 20, 30...\n\
                • 1 кредит за каждого оплатившего реферала\n\n\
                Выберите пакет ниже или отправьте имя канала!"
            ),
        }
    }

    pub fn welcome_with_credits(&self, user_id: i32, referral_section: &str) -> String {
        match self {
            Lang::En => format!(
                "🤖 <b><a href=\"https://t.me/ScratchAuthorEgoBot?start={user_id}\">@ScratchAuthorEgoBot</a> - Channel Analyzer</b>\n\n\
                Welcome back! I can analyze Telegram channels and provide insights.\n\n\
                📋 <b>How to use:</b>\n\
                • Send me a channel username (e.g., <code>@channelname</code>)\n\
                • I'll validate the channel and show analysis options\n\
                • Choose your preferred analysis type\n\
                • Get detailed results in seconds!\n\n\
                ⚠️ <b>Note:</b> Only text content is analyzed. Channels with mostly images or videos may not work well.\n\n\
                ⚡ <b>Analysis Types:</b>\n\
                • 💼 Professional: Expert assessment for hiring\n\
                • 🧠 Personal: Psychological profile insights\n\
                • 🔥 Roast: Fun, brutally honest critique\n\n\
                {referral_section}\n\n\
                Just send me a channel name to get started!"
            ),
            Lang::Ru => format!(
                "🤖 <b><a href=\"https://t.me/ScratchAuthorEgoBot?start={user_id}\">@ScratchAuthorEgoBot</a> - Анализатор каналов</b>\n\n\
                С возвращением! Я анализирую Telegram-каналы и предоставляю инсайты.\n\n\
                📋 <b>Как использовать:</b>\n\
                • Отправьте имя канала (например, <code>@channelname</code>)\n\
                • Я проверю канал и покажу варианты анализа\n\
                • Выберите тип анализа\n\
                • Получите результаты за секунды!\n\n\
                ⚠️ <b>Важно:</b> Анализируется только текст. Каналы с фото/видео могут не подойти.\n\n\
                ⚡ <b>Типы анализа:</b>\n\
                • 💼 Профессиональный: оценка для найма\n\
                • 🧠 Личностный: психологический профиль\n\
                • 🔥 Роаст: весёлая, честная критика\n\n\
                {referral_section}\n\n\
                Отправьте имя канала, чтобы начать!"
            ),
        }
    }

    pub fn referral_info_has_referrals(&self, count: i32) -> String {
        match self {
            Lang::En => format!("You have {} referrals! 🎉", count),
            Lang::Ru => format!("У вас {} рефералов! 🎉", count),
        }
    }

    pub fn referral_info_no_referrals(&self) -> &'static str {
        match self {
            Lang::En => "Start earning free credits by referring friends!",
            Lang::Ru => "Приглашайте друзей и получайте бесплатные кредиты!",
        }
    }

    pub fn referral_section_with_referrals(
        &self,
        credits: i32,
        total_analyses: i32,
        referrals: i32,
        paid_referrals: i32,
        referrals_to_next: i32,
        user_id: i32,
    ) -> String {
        match self {
            Lang::En => format!(
                "💳 <b>Your Status:</b>\n\
                • Credits remaining: <b>{credits}</b>\n\
                • Total analyses performed: <b>{total_analyses}</b>\n\
                • Referrals: <b>{referrals}</b> (Paid: <b>{paid_referrals}</b>)\n\
                • Next milestone reward in <b>{referrals_to_next}</b> referrals\n\n\
                🎁 <b>Referral Program:</b>\n\
                Share your link: <code>https://t.me/ScratchAuthorEgoBot?start={user_id}</code>\n\
                • Get credits at milestones: 1, 5, 10, 20, 30...\n\
                • Get 1 credit for each paid referral\n\n\
                Great job on your {referrals} referrals! 🎉"
            ),
            Lang::Ru => format!(
                "💳 <b>Ваш статус:</b>\n\
                • Осталось кредитов: <b>{credits}</b>\n\
                • Всего анализов: <b>{total_analyses}</b>\n\
                • Рефералов: <b>{referrals}</b> (Оплативших: <b>{paid_referrals}</b>)\n\
                • До следующей награды: <b>{referrals_to_next}</b> рефералов\n\n\
                🎁 <b>Реферальная программа:</b>\n\
                Ваша ссылка: <code>https://t.me/ScratchAuthorEgoBot?start={user_id}</code>\n\
                • Кредиты на этапах: 1, 5, 10, 20, 30...\n\
                • 1 кредит за каждого оплатившего реферала\n\n\
                Отлично, у вас уже {referrals} рефералов! 🎉"
            ),
        }
    }

    pub fn referral_section_no_referrals(
        &self,
        credits: i32,
        total_analyses: i32,
        user_id: i32,
    ) -> String {
        match self {
            Lang::En => format!(
                "💳 <b>Your Status:</b>\n\
                • Credits remaining: <b>{credits}</b>\n\
                • Total analyses performed: <b>{total_analyses}</b>\n\n\
                🎁 <b>Referral Program:</b>\n\
                Share your link: <code>https://t.me/ScratchAuthorEgoBot?start={user_id}</code>\n\
                • Get credits at milestones: 1, 5, 10, 20, 30...\n\
                • Get 1 credit for each paid referral"
            ),
            Lang::Ru => format!(
                "💳 <b>Ваш статус:</b>\n\
                • Осталось кредитов: <b>{credits}</b>\n\
                • Всего анализов: <b>{total_analyses}</b>\n\n\
                🎁 <b>Реферальная программа:</b>\n\
                Ваша ссылка: <code>https://t.me/ScratchAuthorEgoBot?start={user_id}</code>\n\
                • Кредиты на этапах: 1, 5, 10, 20, 30...\n\
                • 1 кредит за каждого оплатившего реферала"
            ),
        }
    }
}

// =============================================================================
// Referral notifications
// =============================================================================

impl Lang {
    pub fn referral_milestone_with_credits(
        &self,
        referral_count: i32,
        credits_awarded: i32,
        referrer_user_id: i32,
    ) -> String {
        match self {
            Lang::En => format!(
                "🎉 <b>Referral Milestone!</b>\n\n\
                Congratulations! You've reached <b>{referral_count}</b> referrals and earned <b>{credits_awarded}</b> credit(s)!\n\n\
                Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">your referral link</a>"
            ),
            Lang::Ru => format!(
                "🎉 <b>Реферальный рубеж!</b>\n\n\
                Поздравляем! Вы достигли <b>{referral_count}</b> рефералов и получили <b>{credits_awarded}</b> кредит(ов)!\n\n\
                Продолжайте делиться: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">вашей реферальной ссылкой</a>"
            ),
        }
    }

    pub fn referral_milestone_no_credits(
        &self,
        referral_count: i32,
        referrer_user_id: i32,
    ) -> String {
        match self {
            Lang::En => format!(
                "🎊 <b>Referral Milestone!</b>\n\n\
                Congratulations! You've reached <b>{referral_count}</b> referrals!\n\n\
                Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">your referral link</a>"
            ),
            Lang::Ru => format!(
                "🎊 <b>Реферальный рубеж!</b>\n\n\
                Поздравляем! Вы достигли <b>{referral_count}</b> рефералов!\n\n\
                Продолжайте делиться: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">вашей реферальной ссылкой</a>"
            ),
        }
    }

    pub fn referral_reward(
        &self,
        credits_awarded: i32,
        referral_count: i32,
        referrer_user_id: i32,
    ) -> String {
        match self {
            Lang::En => format!(
                "🎉 <b>Referral Reward!</b>\n\n\
                You've earned <b>{credits_awarded}</b> credit(s) for reaching <b>{referral_count}</b> referrals!\n\n\
                Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">your referral link</a>"
            ),
            Lang::Ru => format!(
                "🎉 <b>Реферальная награда!</b>\n\n\
                Вы получили <b>{credits_awarded}</b> кредит(ов) за <b>{referral_count}</b> рефералов!\n\n\
                Продолжайте делиться: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">вашей реферальной ссылкой</a>"
            ),
        }
    }

    pub fn referral_paid_and_milestone(
        &self,
        total_credits: i32,
        referral_count: i32,
        paid_rewards: i32,
        milestone_rewards: i32,
        referrer_user_id: i32,
    ) -> String {
        match self {
            Lang::En => format!(
                "🎉 <b>Referral Rewards!</b>\n\n\
                You've earned <b>{total_credits}</b> credits (Total referrals: <b>{referral_count}</b>):\n\
                • {paid_rewards} credit(s) for paid referral\n\
                • {milestone_rewards} credit(s) for milestone bonus\n\n\
                Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">your referral link</a>"
            ),
            Lang::Ru => format!(
                "🎉 <b>Реферальные награды!</b>\n\n\
                Вы получили <b>{total_credits}</b> кредитов (Всего рефералов: <b>{referral_count}</b>):\n\
                • {paid_rewards} кредит(ов) за оплатившего реферала\n\
                • {milestone_rewards} кредит(ов) за рубеж\n\n\
                Продолжайте делиться: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">вашей реферальной ссылкой</a>"
            ),
        }
    }

    pub fn referral_paid_only(
        &self,
        paid_rewards: i32,
        referral_count: i32,
        referrer_user_id: i32,
    ) -> String {
        match self {
            Lang::En => format!(
                "🎉 <b>Referral Reward!</b>\n\n\
                You've earned <b>{paid_rewards}</b> credit(s) for a paid referral! (Total referrals: <b>{referral_count}</b>)\n\n\
                Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">your referral link</a>"
            ),
            Lang::Ru => format!(
                "🎉 <b>Реферальная награда!</b>\n\n\
                Вы получили <b>{paid_rewards}</b> кредит(ов) за оплатившего реферала! (Всего рефералов: <b>{referral_count}</b>)\n\n\
                Продолжайте делиться: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">вашей реферальной ссылкой</a>"
            ),
        }
    }

    pub fn referral_milestone_only(
        &self,
        milestone_rewards: i32,
        referral_count: i32,
        referrer_user_id: i32,
    ) -> String {
        match self {
            Lang::En => format!(
                "🎉 <b>Milestone Reward!</b>\n\n\
                You've earned <b>{milestone_rewards}</b> credit(s) for reaching <b>{referral_count}</b> referrals!\n\n\
                Keep sharing: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">your referral link</a>"
            ),
            Lang::Ru => format!(
                "🎉 <b>Награда за рубеж!</b>\n\n\
                Вы получили <b>{milestone_rewards}</b> кредит(ов) за <b>{referral_count}</b> рефералов!\n\n\
                Продолжайте делиться: <a href=\"https://t.me/ScratchAuthorEgoBot?start={referrer_user_id}\">вашей реферальной ссылкой</a>"
            ),
        }
    }
}

// =============================================================================
// Credits & payments
// =============================================================================

impl Lang {
    pub fn no_credits_available(
        &self,
        single_price: u32,
        bulk_price: u32,
        bulk_discount: u32,
        credits: i32,
        total_analyses: i32,
    ) -> String {
        match self {
            Lang::En => format!(
                "❌ <b>No Analysis Credits Available</b>\n\n\
                You have used all your free analysis credits.\n\n\
                💰 <b>Purchase More Credits:</b>\n\
                • 1 analysis for {single_price} ⭐ stars\n\
                • 10 analyses for {bulk_price} ⭐ stars (save {bulk_discount} stars!)\n\n\
                📊 <b>Your Stats:</b>\n\
                • Credits remaining: <code>{credits}</code>\n\
                • Total analyses performed: <code>{total_analyses}</code>\n\n\
                Choose a package below to continue analyzing channels!"
            ),
            Lang::Ru => format!(
                "❌ <b>Нет кредитов для анализа</b>\n\n\
                Вы использовали все бесплатные кредиты.\n\n\
                💰 <b>Купить кредиты:</b>\n\
                • 1 анализ за {single_price} ⭐ звёзд\n\
                • 10 анализов за {bulk_price} ⭐ звёзд (экономия {bulk_discount} звёзд!)\n\n\
                📊 <b>Ваша статистика:</b>\n\
                • Осталось кредитов: <code>{credits}</code>\n\
                • Всего анализов: <code>{total_analyses}</code>\n\n\
                Выберите пакет ниже!"
            ),
        }
    }

    pub fn no_credits_short(&self) -> &'static str {
        match self {
            Lang::En => "❌ No analysis credits available.\n\nYou need credits to analyze channels. Choose a package below:",
            Lang::Ru => "❌ Нет кредитов для анализа.\n\nДля анализа каналов нужны кредиты. Выберите пакет ниже:",
        }
    }

    pub fn payment_success(&self, user_id: i32, credits: i32, new_balance: i32) -> String {
        match self {
            Lang::En => format!(
                "🎉 <b>Payment Successful!</b> - <a href=\"https://t.me/ScratchAuthorEgoBot?start={user_id}\">@ScratchAuthorEgoBot</a>\n\n\
                ✅ Added {credits} credits to your account\n\
                💳 New balance: {new_balance} credits\n\n\
                You can now analyze channels by sending me a channel username like <code>@channelname</code>"
            ),
            Lang::Ru => format!(
                "🎉 <b>Платёж успешен!</b> - <a href=\"https://t.me/ScratchAuthorEgoBot?start={user_id}\">@ScratchAuthorEgoBot</a>\n\n\
                ✅ Добавлено {credits} кредитов на ваш счёт\n\
                💳 Новый баланс: {new_balance} кредитов\n\n\
                Теперь вы можете анализировать каналы, отправив имя канала, например <code>@channelname</code>"
            ),
        }
    }

    pub fn credits_label(&self, credits: i32) -> String {
        match self {
            Lang::En => format!("{} credits", credits),
            Lang::Ru => format!("{} кредитов", credits),
        }
    }
}

// =============================================================================
// Buttons
// =============================================================================

impl Lang {
    pub fn btn_buy_single(&self, amount: i32, price: u32) -> String {
        match self {
            Lang::En => format!("💎 Buy {} Credit ({} ⭐)", amount, price),
            Lang::Ru => format!("💎 Купить {} кредит ({} ⭐)", amount, price),
        }
    }

    pub fn btn_buy_bulk(&self, amount: i32, price: u32) -> String {
        match self {
            Lang::En => format!("💎 Buy {} Credits ({} ⭐)", amount, price),
            Lang::Ru => format!("💎 Купить {} кредитов ({} ⭐)", amount, price),
        }
    }

    pub fn btn_professional_analysis(&self) -> &'static str {
        match self {
            Lang::En => "💼 Professional Analysis",
            Lang::Ru => "💼 Профессиональный анализ",
        }
    }

    pub fn btn_personal_analysis(&self) -> &'static str {
        match self {
            Lang::En => "🧠 Personal Analysis",
            Lang::Ru => "🧠 Личностный анализ",
        }
    }

    pub fn btn_roast_analysis(&self) -> &'static str {
        match self {
            Lang::En => "🔥 Roast Analysis",
            Lang::Ru => "🔥 Роаст-анализ",
        }
    }
}

// =============================================================================
// Invoice descriptions
// =============================================================================

impl Lang {
    pub fn invoice_single_title(&self) -> &'static str {
        match self {
            Lang::En => "1 Channel Analysis",
            Lang::Ru => "1 анализ канала",
        }
    }

    pub fn invoice_single_description(&self) -> &'static str {
        match self {
            Lang::En => "Get 1 analysis credit to analyze any Telegram channel",
            Lang::Ru => "Получите 1 кредит для анализа любого Telegram-канала",
        }
    }

    pub fn invoice_bulk_title(&self) -> &'static str {
        match self {
            Lang::En => "10 Channel Analyses",
            Lang::Ru => "10 анализов каналов",
        }
    }

    pub fn invoice_bulk_description(&self, discount: u32) -> String {
        match self {
            Lang::En => format!(
                "Get 10 analysis credits to analyze any Telegram channels ({} stars discount!)",
                discount
            ),
            Lang::Ru => format!(
                "Получите 10 кредитов для анализа Telegram-каналов (скидка {} звёзд!)",
                discount
            ),
        }
    }
}

// =============================================================================
// Analysis flow
// =============================================================================

impl Lang {
    pub fn analysis_starting(&self, credits_after: i32) -> String {
        match self {
            Lang::En => format!(
                "🔍 Starting analysis...\n\n\
                💳 Credits remaining after analysis: <code>{credits_after}</code>"
            ),
            Lang::Ru => format!(
                "🔍 Начинаю анализ...\n\n\
                💳 Останется кредитов после анализа: <code>{credits_after}</code>"
            ),
        }
    }

    pub fn analysis_select_type(&self, channel_name: &str) -> String {
        match self {
            Lang::En => format!(
                "🎯 <b>Channel:</b> <code>{channel_name}</code>\n\n\
                Please choose the type of analysis you'd like to perform:\n\n\
                ⚠️ <b>Note:</b> Only text content is analyzed. Channels consisting mostly of images or videos may not yield accurate results."
            ),
            Lang::Ru => format!(
                "🎯 <b>Канал:</b> <code>{channel_name}</code>\n\n\
                Выберите тип анализа:\n\n\
                ⚠️ <b>Важно:</b> Анализируется только текст. Каналы с фото/видео могут не дать точных результатов."
            ),
        }
    }

    pub fn analysis_in_progress(&self, analysis_type: &str) -> String {
        let emoji = self.analysis_emoji(analysis_type);
        match self {
            Lang::En => format!(
                "Starting {} {} analysis... This may take a few minutes.",
                emoji, analysis_type
            ),
            Lang::Ru => format!(
                "Начинаю {} {} анализ... Это может занять несколько минут.",
                emoji,
                self.analysis_type_name(analysis_type)
            ),
        }
    }

    pub fn analysis_complete(
        &self,
        analysis_type: &str,
        user_id: i32,
        remaining_credits: i32,
    ) -> String {
        let type_capitalized = self.analysis_type_capitalized(analysis_type);
        match self {
            Lang::En => format!(
                "✅ <b>{type_capitalized} Analysis Complete!</b> by <a href=\"https://t.me/ScratchAuthorEgoBot?start={user_id}\">@ScratchAuthorEgoBot</a>\n\n\
                📊 Your results are ready.\n\
                💳 Credits remaining: <code>{remaining_credits}</code>"
            ),
            Lang::Ru => format!(
                "✅ <b>{type_capitalized} анализ завершён!</b> от <a href=\"https://t.me/ScratchAuthorEgoBot?start={user_id}\">@ScratchAuthorEgoBot</a>\n\n\
                📊 Результаты готовы.\n\
                💳 Осталось кредитов: <code>{remaining_credits}</code>"
            ),
        }
    }

    pub fn analysis_result_header(&self, channel_name: &str, user_id: i32) -> String {
        match self {
            Lang::En => format!(
                "📊 <b>Channel Analysis Results</b> by <a href=\"https://t.me/ScratchAuthorEgoBot?start={user_id}\">@ScratchAuthorEgoBot</a>\n\n\
                🎯 <b>Channel:</b> <code>{channel_name}</code>\n\n"
            ),
            Lang::Ru => format!(
                "📊 <b>Результаты анализа канала</b> от <a href=\"https://t.me/ScratchAuthorEgoBot?start={user_id}\">@ScratchAuthorEgoBot</a>\n\n\
                🎯 <b>Канал:</b> <code>{channel_name}</code>\n\n"
            ),
        }
    }

    pub fn analysis_type_header(&self, analysis_type: &str) -> String {
        let emoji = self.analysis_emoji(analysis_type);
        let type_capitalized = self.analysis_type_capitalized(analysis_type);
        match self {
            Lang::En => format!("{} <b>{} Analysis:</b>\n\n", emoji, type_capitalized),
            Lang::Ru => format!("{} <b>{} анализ:</b>\n\n", emoji, type_capitalized),
        }
    }

    pub fn analysis_part_indicator(&self, part: usize, total: usize) -> String {
        match self {
            Lang::En => format!("\n\n<i>📄 Part {} of {}</i>", part, total),
            Lang::Ru => format!("\n\n<i>📄 Часть {} из {}</i>", part, total),
        }
    }

    fn analysis_emoji(&self, analysis_type: &str) -> &'static str {
        match analysis_type {
            "professional" => "💼",
            "personal" => "🧠",
            "roast" => "🔥",
            _ => "🔍",
        }
    }

    fn analysis_type_capitalized(&self, analysis_type: &str) -> String {
        match self {
            Lang::En => {
                analysis_type
                    .chars()
                    .next()
                    .unwrap()
                    .to_uppercase()
                    .collect::<String>()
                    + &analysis_type[1..]
            }
            Lang::Ru => match analysis_type {
                "professional" => "Профессиональный".to_string(),
                "personal" => "Личностный".to_string(),
                "roast" => "Роаст".to_string(),
                _ => analysis_type.to_string(),
            },
        }
    }

    fn analysis_type_name(&self, analysis_type: &str) -> &'static str {
        match self {
            Lang::En => match analysis_type {
                "professional" => "professional",
                "personal" => "personal",
                "roast" => "roast",
                _ => "analysis",
            },
            Lang::Ru => match analysis_type {
                "professional" => "профессиональный",
                "personal" => "личностный",
                "roast" => "роаст",
                _ => "анализ",
            },
        }
    }
}
