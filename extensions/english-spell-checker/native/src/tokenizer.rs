#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordToken {
    pub text: String,
    pub line: u32,
    pub start_col: u32,
    pub end_col: u32,
    pub is_forced_typo: bool,
}

pub fn tokenize_document(text: &str) -> Vec<WordToken> {
    let mut tokens = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        let line_num = line_idx as u32;
        tokenize_line(line, line_num, &mut tokens);
    }

    tokens
}

pub fn tokenize_line(line: &str, line_num: u32, out: &mut Vec<WordToken>) {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Skip URLs
        if let Some(remaining) = line.get(chars[i].0..) {
            if remaining.starts_with("http://") || remaining.starts_with("https://") || remaining.starts_with("file://") {
                // skip until whitespace
                while i < len && !chars[i].1.is_whitespace() {
                    i += 1;
                }
                continue;
            }
        }

        let ch = chars[i].1;

        // Start of a potential word token (ASCII alphabetic)
        if ch.is_ascii_alphabetic() {
            let start_byte = chars[i].0;

            // Consume characters belonging to this identifier/word
            let mut has_digit = false;
            let mut has_underscore = false;
            let mut has_hyphen = false;

            while i < len {
                let c = chars[i].1;
                if c.is_ascii_alphabetic() {
                    i += 1;
                } else if c == '\'' {
                    // Internal apostrophe (e.g. don't, user's)
                    if i + 1 < len && chars[i + 1].1.is_ascii_alphabetic() {
                        i += 2;
                    } else {
                        break;
                    }
                } else if c == '_' {
                    has_underscore = true;
                    break;
                } else if c == '-' {
                    has_hyphen = true;
                    break;
                } else if c.is_ascii_digit() {
                    has_digit = true;
                    i += 1;
                } else {
                    break;
                }
            }

            let end_byte = if i < len { chars[i].0 } else { line.len() };
            let raw_word = &line[start_byte..end_byte];

            // If token has digits (like sha256, utf8, v2, x86_64), skip
            if !has_digit && !raw_word.is_empty() {
                split_and_add_tokens(raw_word, line_num, utf16_col(line, start_byte), out);
            }

            if has_underscore || has_hyphen {
                i += 1; // skip the separator and continue next token
            }
        } else {
            i += 1;
        }
    }

    // Check contextual typos like "dose not" -> "does not"
    for idx in 0..out.len().saturating_sub(1) {
        if out[idx].text.to_ascii_lowercase() == "dose" {
            let next_word = out[idx + 1].text.to_ascii_lowercase();
            if next_word == "not" || next_word == "exist" || next_word == "matter" || next_word == "have" {
                out[idx].is_forced_typo = true;
            }
        }
    }
}

fn split_and_add_tokens(raw: &str, line_num: u32, base_utf16_col: u32, out: &mut Vec<WordToken>) {
    // If it's all uppercase with <= 4 chars (e.g. HTML, JSON, HTTP, URL, LSP), skip
    if raw.len() <= 4 && raw.chars().all(|c| c.is_ascii_uppercase()) {
        return;
    }

    // If it contains an apostrophe (e.g. don't), treat as a single token
    if raw.contains('\'') {
        if raw.len() > 2 {
            out.push(WordToken {
                text: raw.to_string(),
                line: line_num,
                start_col: base_utf16_col,
                end_col: base_utf16_col + raw.encode_utf16().count() as u32,
                is_forced_typo: false,
            });
        }
        return;
    }

    // CamelCase splitting
    let chars: Vec<(usize, char)> = raw.char_indices().collect();
    let mut word_start = 0;

    for k in 1..chars.len() {
        let prev = chars[k - 1].1;
        let curr = chars[k].1;

        let is_split = (prev.is_ascii_lowercase() && curr.is_ascii_uppercase())
            || (k + 1 < chars.len() && prev.is_ascii_uppercase() && curr.is_ascii_uppercase() && chars[k + 1].1.is_ascii_lowercase());

        if is_split {
            let sub = &raw[chars[word_start].0..chars[k].0];
            push_word_if_eligible(sub, line_num, base_utf16_col, raw, chars[word_start].0, out);
            word_start = k;
        }
    }

    let last_sub = &raw[chars[word_start].0..];
    push_word_if_eligible(last_sub, line_num, base_utf16_col, raw, chars[word_start].0, out);
}

fn push_word_if_eligible(
    word: &str,
    line_num: u32,
    base_utf16_col: u32,
    parent_raw: &str,
    byte_offset_in_parent: usize,
    out: &mut Vec<WordToken>,
) {
    if word.len() <= 2 {
        return;
    }
    if word.len() <= 4 && word.chars().all(|c| c.is_ascii_uppercase()) {
        return;
    }

    let start_col = base_utf16_col + parent_raw[..byte_offset_in_parent].encode_utf16().count() as u32;
    let end_col = start_col + word.encode_utf16().count() as u32;

    out.push(WordToken {
        text: word.to_string(),
        line: line_num,
        start_col,
        end_col,
        is_forced_typo: false,
    });
}

fn utf16_col(line: &str, byte_idx: usize) -> u32 {
    line[..byte_idx].encode_utf16().count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_plain_words() {
        let text = "make english spell checker extension that dose not need an external tool";
        let tokens = tokenize_document(text);
        let words: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert!(words.contains(&"make"));
        assert!(words.contains(&"english"));
        assert!(words.contains(&"spell"));
        assert!(words.contains(&"checker"));
        assert!(words.contains(&"extension"));
        assert!(words.contains(&"dose"));
        assert!(words.contains(&"need"));
        assert!(words.contains(&"external"));
        assert!(words.contains(&"tool"));

        let dose_tok = tokens.iter().find(|t| t.text == "dose").unwrap();
        assert!(dose_tok.is_forced_typo);
    }

    #[test]
    fn test_tokenize_camel_case() {
        let text = "let parseJsonBuffer = false;";
        let tokens = tokenize_document(text);
        let words: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert!(words.contains(&"parse"));
        assert!(words.contains(&"Buffer"));
    }

    #[test]
    fn test_tokenize_snake_case() {
        let text = "fn handle_spell_check_errors()";
        let tokens = tokenize_document(text);
        let words: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert!(words.contains(&"handle"));
        assert!(words.contains(&"spell"));
        assert!(words.contains(&"check"));
        assert!(words.contains(&"errors"));
    }
}
