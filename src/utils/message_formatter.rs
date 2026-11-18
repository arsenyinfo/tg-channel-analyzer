use comrak::{markdown_to_html, ComrakOptions};
use html_escape;
use regex::Regex;

pub struct MessageFormatter;

impl MessageFormatter {
    pub fn escape_html(text: &str) -> String {
        // use proper HTML escaping library
        html_escape::encode_text(text).to_string()
    }

    pub fn markdown_to_html_safe(text: &str) -> String {
        // convert markdown to HTML with Telegram-compatible options.
        // autolink is disabled and anchors are stripped below so attacker-controlled channel
        // content (via the LLM analysis output) cannot surface clickable phishing links.
        let mut options = ComrakOptions::default();
        options.extension.strikethrough = true;
        options.extension.autolink = false;
        options.render.hardbreaks = true;
        options.render.unsafe_ = false;

        let html = markdown_to_html(text, &options);

        // strip anchor tags from markdown links, keeping only their visible text
        // (comrak escapes attribute contents, so a literal '>' never appears inside the tag)
        let anchor_open = Regex::new(r"<a\b[^>]*>").unwrap();
        let html = anchor_open.replace_all(&html, "").into_owned();
        let html = html.replace("</a>", "");

        // Telegram cannot render images; retain comrak's already-escaped alternative text.
        let image = Regex::new(r#"<img\b[^>]*\salt="([^"]*)"[^>]*>"#).unwrap();
        let html = image.replace_all(&html, "$1").into_owned();

        // Ordered lists starting above one include a start attribute.
        let ordered_list_open = Regex::new(r"<ol\b[^>]*>").unwrap();
        let html = ordered_list_open.replace_all(&html, "").into_owned();

        // defang URL schemes and www so Telegram clients do not auto-linkify bare URLs the
        // model may still emit. covers scheme:// and www. forms (the clickable phishing vectors);
        // a bare domain with no scheme/www is a documented residual.
        let html = html.replace("://", "[://]");
        let html = html.replace("www.", "www[.]");

        // telegram HTML mode only supports: b, i, u, s, code, pre, a
        // replace unsupported tags with supported ones or remove them
        let html = html
            .replace("<p>", "")
            .replace("</p>", "\n\n")
            .replace("<h1>", "<b>")
            .replace("</h1>", "</b>\n\n")
            .replace("<h2>", "<b>")
            .replace("</h2>", "</b>\n\n")
            .replace("<h3>", "<b>")
            .replace("</h3>", "</b>\n")
            .replace("<h4>", "<b>")
            .replace("</h4>", "</b>\n")
            .replace("<h5>", "<b>")
            .replace("</h5>", "</b>\n")
            .replace("<h6>", "<b>")
            .replace("</h6>", "</b>\n")
            .replace("<strong>", "<b>")
            .replace("</strong>", "</b>")
            .replace("<em>", "<i>")
            .replace("</em>", "</i>")
            .replace("<del>", "<s>")
            .replace("</del>", "</s>")
            // remove list tags and convert to plain text with bullets
            .replace("<ul>", "")
            .replace("</ul>", "\n")
            .replace("</ol>", "\n")
            .replace("<li>", "• ")
            .replace("</li>", "\n")
            // remove other unsupported tags
            .replace("<div>", "")
            .replace("</div>", "\n")
            .replace("<span>", "")
            .replace("</span>", "")
            .replace("<br>", "\n")
            .replace("<br/>", "\n")
            .replace("<br />", "\n")
            .replace("<hr>", "\n───────────\n")
            .replace("<hr/>", "\n───────────\n")
            .replace("<hr />", "\n───────────\n");

        // clean up excessive whitespace
        let lines: Vec<&str> = html.lines().collect();
        let mut result = Vec::new();
        let mut empty_line_count = 0;

        for line in lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                empty_line_count += 1;
                // allow max 1 consecutive empty line (single blank line between paragraphs)
                if empty_line_count <= 1 {
                    result.push("");
                }
            } else {
                empty_line_count = 0;
                result.push(trimmed);
            }
        }

        result.join("\n").trim().to_string()
    }

    /// counts UTF-16 code units as Telegram does for message length limits
    pub fn count_utf16_code_units(text: &str) -> usize {
        text.encode_utf16().count()
    }

    /// hard-splits a single token into pieces each within `max_length` UTF-16 code units,
    /// breaking only at char boundaries
    fn hard_split_word(word: &str, max_length: usize) -> Vec<String> {
        let mut pieces = Vec::new();
        let mut current = String::new();
        let mut current_len = 0;
        for ch in word.chars() {
            let ch_len = ch.len_utf16();
            if current_len + ch_len > max_length && !current.is_empty() {
                pieces.push(std::mem::take(&mut current));
                current_len = 0;
            }
            current.push(ch);
            current_len += ch_len;
        }
        if !current.is_empty() {
            pieces.push(current);
        }
        pieces
    }

    /// Splits generated HTML by rendered UTF-16 length, keeping entities intact and
    /// closing/reopening active formatting so each chunk is independently valid.
    pub fn split_message_into_chunks(text: &str, max_length: usize) -> Vec<String> {
        if Self::count_utf16_code_units(text) <= max_length {
            return vec![text.to_string()];
        }

        // Words stay together when they fit; long words split only at character boundaries.
        // Input is the escaped HTML produced by markdown_to_html_safe.
        let tokens =
            Regex::new(r"(?s)<[^>]*>|&(?:#[xX][0-9a-fA-F]+|#[0-9]+|[A-Za-z]+);|[^<&\s]+|\s|[<&]")
                .unwrap();
        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut visible_length = 0;
        let mut open_tags: Vec<(&str, &str)> = Vec::new();

        for matched in tokens.find_iter(text) {
            let token = matched.as_str();
            if token.starts_with("<!--") {
                continue;
            }
            if let Some(tag) = token.strip_prefix('<') {
                if token.starts_with("</") {
                    open_tags.pop();
                } else {
                    let name = tag
                        .split(|c: char| c.is_whitespace() || c == '>')
                        .next()
                        .unwrap();
                    open_tags.push((name, token));
                }
                current.push_str(token);
                continue;
            }

            let is_entity = token.starts_with('&') && token.ends_with(';');
            let pieces = if !is_entity && Self::count_utf16_code_units(token) > max_length {
                Self::hard_split_word(token, max_length)
            } else {
                vec![token.to_string()]
            };
            for piece in pieces {
                let length =
                    Self::count_utf16_code_units(&html_escape::decode_html_entities(&piece));
                if visible_length > 0 && visible_length + length > max_length {
                    for (name, _) in open_tags.iter().rev() {
                        current.push_str(&format!("</{name}>"));
                    }
                    chunks.push(std::mem::take(&mut current));
                    for (_, opening) in &open_tags {
                        current.push_str(opening);
                    }
                    visible_length = 0;
                }
                current.push_str(&piece);
                visible_length += length;
            }
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        chunks
    }
}

#[cfg(test)]
mod tests {
    use super::MessageFormatter;

    #[test]
    fn ordered_lists_with_start_attributes_render_as_bullets() {
        let html = MessageFormatter::markdown_to_html_safe("3. First\n4. **Second**");
        assert!(!html.contains("<ol"));
        assert!(!html.contains("</ol>"));
        assert!(html.contains("• First"));
        assert!(html.contains("• <b>Second</b>"));
    }

    #[test]
    fn markdown_images_preserve_escaped_alt_text_without_image_tags() {
        let html = MessageFormatter::markdown_to_html_safe(
            "![<tag> & \"quoted\"](https://example.com/image.png \"title\")",
        );
        assert!(!html.contains("<img"));
        assert!(!html.contains("example.com"));
        assert!(!html.contains("<tag>"));
        assert_eq!(
            html_escape::decode_html_entities(&html),
            "<tag> & \"quoted\""
        );
    }

    #[test]
    fn image_alt_text_keeps_existing_link_defanging() {
        let html = MessageFormatter::markdown_to_html_safe(
            "[![https://example.com www.example.com](https://image.example/p.png)](https://link.example)",
        );
        assert!(!html.contains("<img"));
        assert!(!html.contains("<a"));
        assert!(!html.contains("image.example"));
        assert!(!html.contains("link.example"));
        assert!(html.contains("https[://]example.com"));
        assert!(html.contains("www[.]example.com"));
    }
    fn rendered(html: &str) -> String {
        // Parse fragments independently, so assertions compare delivered text rather than markup.
        scraper::Html::parse_fragment(html)
            .root_element()
            .text()
            .collect()
    }

    fn assert_chunks(html: &str, limit: usize) {
        let chunks = MessageFormatter::split_message_into_chunks(html, limit);
        let tag = regex::Regex::new(r"</?([a-z]+)[^>]*>").unwrap();
        for chunk in &chunks {
            let mut stack = Vec::new();
            for caps in tag.captures_iter(chunk) {
                let name = caps.get(1).unwrap().as_str();
                if caps.get(0).unwrap().as_str().starts_with("</") {
                    assert_eq!(stack.pop(), Some(name), "unbalanced HTML: {chunk}");
                } else {
                    stack.push(name);
                }
            }
            assert!(stack.is_empty(), "unclosed formatting in {chunk}");
            let visible = rendered(chunk);
            assert!(MessageFormatter::count_utf16_code_units(&visible) <= limit);
            assert!(!visible.is_empty(), "empty rendered chunk: {chunk}");
        }
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| rendered(chunk))
                .collect::<String>(),
            rendered(html)
        );
    }

    #[test]
    fn html_chunks_keep_nested_formatting_balanced() {
        for html in [
            "<b>abcdefghijklmno</b>",
            "<b>ab<i>cd efghijkl</i>mn</b>",
            "<pre><code class=\"language-rust\">let x = 1;\nlet y = 2;</code></pre>",
        ] {
            for limit in [5, 10, 16] {
                assert_chunks(html, limit);
            }
        }
    }

    #[test]
    fn html_chunks_preserve_entities_unicode_and_line_breaks() {
        for html in [
            "1234567&amp;xy",
            "ab&lt;cd&gt;ef&#x1F600;gh&#128512;ij",
            "<b>😀😀😀😀</b>",
            "abcdefghijkl\nZ",
            "one two\nthree four",
        ] {
            for limit in [2, 5, 10] {
                assert_chunks(html, limit);
            }
        }
    }
}
