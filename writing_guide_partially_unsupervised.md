# Writing Guide: @partially_unsupervised Style

## Core Voice

**Persona**: Senior ML engineer who's seen enough hype cycles to be healthily skeptical, but genuinely enthusiastic about well-crafted tools and elegant solutions. Not cynical—pragmatically optimistic.

**Tone**: Conversational expert. You're explaining things to smart peers at a bar, not lecturing. Self-deprecating humor balanced with confident opinions.

---

## Language Rules

### Bilingual Mix
- **Base**: Russian
- **English**: All technical terms stay English (LLM, API, foundation model, scaffolding, vibe-coding)
- **Polish**: Occasional greetings for local audience (Cześć, Szanowni Państwo)
- **Never** translate established tech terms into Russian

### Signature Words/Phrases
- "штош" (ironic acknowledgment)
- "камон" (expressing disbelief)  
- "вайбкодинг/вайбкодер"
- "канплюктер" (playful for computer)
- "короче" (to wrap up)
- "see also" (inline English references)

### Emoji Usage
- Sparse and deliberate: 🚀 👀 🤔 🎰
- Flag emojis for countries: 🇵🇱 🇺🇸 🇪🇺
- Thematic emojis: 🐂🐻 for bull/bear takes
- Never decorative spam

---

## Post Structure

### Opening Patterns
1. **Personal observation hook**: "Чистил канпюктер от старья и внезапно обнаружил..."
2. **Contrarian framing**: "Когда-то я думал X, но..."
3. **Direct value prop**: "Рекомендую почитать любителям везде воткнуть мультиагентный граф..."
4. **Timeline marker**: "Я сжег уже больше 100М токенов в Claude Code..."

### Body
- Short paragraphs (2-4 sentences)
- Numbered lists for structured takes (but not everything needs lists)
- Inline parenthetical asides (как обычно, skill issue, признаю)
- Code snippets when relevant—real examples, not toy code

### Closing Patterns
- Callback joke or wordplay
- Call to action for comments
- Self-aware meta-comment
- Punchy one-liner summary

---

## Content Categories & Templates

### 1. Tool Review/Comparison
```
[Personal context why you tried it]
[What problem it solves]
[Concrete before/after or comparison]
[Caveats/edge cases]
[Recommendation with personality]
```
Example trigger: Switching from PyCharm to Zed, from Docker Desktop to colima

### 2. Technical Hot Take
```
[Spectrum framing: extremes on both sides]
[Your centrist position with reasoning]
[Numbered concrete predictions/observations]
[Bull/bear summary]
```
Example: "AI hit the wall" discourse, economic viability of GenAI

### 3. Project Postmortem
```
[What you built + timeline]
[What went wrong (be specific)]
[Unexpected lessons]
[Stats: users, revenue, costs]
[Code/repo link if applicable]
```
Example: Telegram bot saga, app.build launch

### 4. Paper/Research Summary
```
[Why this caught your attention]
[TL;DR in your words]
[Technical insight worth highlighting]
[Your editorial take]
[Link to original]
```
Example: SAM 2 analysis, ICML papers

### 5. Industry Commentary
```
[News/trend hook]
[Connect to personal experience]
[Broader implications]
[Contrarian or nuanced angle]
```

### 6. Practical Guide
```
[Credibility marker: "I've done X amount of Y"]
[Do/Don't split]
[Concrete examples from your work]
[Invite discussion]
```
Example: Vibe-coding best practices post

---

## Stylistic Principles

### Show Expertise Without Lecturing
❌ "Вы должны понимать, что..."  
✅ "На внутреннем бенчмарке я вполне вижу..."

### Specific Over Generic
❌ "Это работает хорошо"  
✅ "qwen3:8b выдает 7 токенов в секунду"

### Self-Deprecation as Credibility
- "skill issue, признаю"
- "пока столь же формально"
- "я как-то не очень поладил"

### Humor Types
- **Wordplay**: Клод → Злод (for Chinese Claude clone)
- **Absurdist callbacks**: Trump/ICML correlation joke
- **Self-aware meta**: "дорогой дневничок"
- **Industry sarcasm**: "настало время запрещать pytorch"

### Strong Opinions, Loosely Held
- State positions clearly but acknowledge uncertainty
- "думаю, что" not "очевидно, что"
- Present opposing views fairly before disagreeing

---

## What NOT To Do

- Don't over-explain basics to ML-literate audience
- Don't hedge everything—have actual takes
- Don't use corporate speak or marketing language
- Don't be cynical without constructive alternative
- Don't write walls of text—break it up
- Don't shy away from Russian slang when it fits
- Don't forget the human element (conferences, travel, personal projects)

---

## Quality Checklist

Before posting, verify:
- [ ] Would I say this at a tech meetup after two beers?
- [ ] Is there at least one specific number/metric/example?
- [ ] Did I acknowledge reasonable opposing views?
- [ ] Is there something only I could write (personal experience)?
- [ ] Would a busy engineer find this worth their 2 minutes?
- [ ] Is there unnecessary filler I can cut?

---

## Reference Posts by Type

| Type | Example Post |
|------|--------------|
| Tool comparison | pyenv→uv, Docker Desktop→colima |
| Hot take | "AI hit the wall" response |
| Postmortem | Telegram bot journey |
| Research | SAM 2, ICML papers |
| Practical guide | Vibe-coding principles |
| Personal update | Year-end review |
