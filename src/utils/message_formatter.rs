use comrak::{markdown_to_html, ComrakOptions};
use html_escape;

pub struct MessageFormatter;

impl MessageFormatter {
    pub fn escape_html(text: &str) -> String {
        // use proper HTML escaping library
        html_escape::encode_text(text).to_string()
    }

    pub fn markdown_to_html_safe(text: &str) -> String {
        // convert markdown to HTML with Telegram-compatible options
        let mut options = ComrakOptions::default();
        options.extension.strikethrough = true;
        options.extension.autolink = true;
        options.render.hardbreaks = true;
        options.render.unsafe_ = false;

        let html = markdown_to_html(text, &options);

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
            .replace("<ol>", "")
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

    /// splits a message into chunks that fit within Telegram's 4096 UTF-16 code unit limit
    pub fn split_message_into_chunks(text: &str, max_length: usize) -> Vec<String> {
        if Self::count_utf16_code_units(text) <= max_length {
            return vec![text.to_string()];
        }

        let mut chunks = Vec::new();
        let mut current_chunk = String::new();

        // split by lines to avoid breaking in the middle of formatting
        for line in text.lines() {
            let line_with_newline = format!("{}\n", line);

            // if adding this line would exceed the limit, finalize current chunk
            if Self::count_utf16_code_units(&current_chunk)
                + Self::count_utf16_code_units(&line_with_newline)
                > max_length
            {
                if !current_chunk.is_empty() {
                    chunks.push(current_chunk.trim_end().to_string());
                    current_chunk.clear();
                }

                // if single line is too long, split it at word boundaries
                if Self::count_utf16_code_units(&line_with_newline) > max_length {
                    let words: Vec<&str> = line.split_whitespace().collect();
                    let mut word_chunk = String::new();

                    for word in words {
                        let word_with_space = format!("{} ", word);
                        if Self::count_utf16_code_units(&word_chunk)
                            + Self::count_utf16_code_units(&word_with_space)
                            > max_length
                        {
                            if !word_chunk.is_empty() {
                                chunks.push(word_chunk.trim_end().to_string());
                                word_chunk.clear();
                            }
                        }
                        word_chunk.push_str(&word_with_space);
                    }

                    if !word_chunk.is_empty() {
                        current_chunk = word_chunk.trim_end().to_string();
                    }
                } else {
                    current_chunk.push_str(&line_with_newline);
                }
            } else {
                current_chunk.push_str(&line_with_newline);
            }
        }

        if !current_chunk.is_empty() {
            chunks.push(current_chunk.trim_end().to_string());
        }

        chunks
    }
}
