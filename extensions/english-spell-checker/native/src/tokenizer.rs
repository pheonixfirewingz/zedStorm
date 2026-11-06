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

    for (line_index, line) in text.lines().enumerate() {
        let line_num = line_index as u32;
        tokenize_line(line, line_num, &mut tokens);
    }

    // Contextual typos like "dose not" -> "does not"
    for index in 1..tokens.len() {
        if tokens[index - 1].text.eq_ignore_ascii_case("dose")
            && ["not", "exist", "matter", "have"]
                .iter()
                .any(|next| tokens[index].text.eq_ignore_ascii_case(next))
        {
            tokens[index - 1].is_forced_typo = true;
        }
    }

    tokens
}

pub fn tokenize_line(line: &str, line_num: u32, out: &mut Vec<WordToken>) {
    // Tokens are ASCII, and every byte of a multi-byte UTF-8 character is >= 0x80, so scanning
    // bytes never matches inside one and avoids a per-character index allocation.
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut index = 0;
    // UTF-16 column of `counted_byte`, advanced incrementally so long lines stay linear.
    let mut counted_byte = 0;
    let mut utf16_column = 0u32;

    while index < len {
        if bytes[index] == b'h' || bytes[index] == b'f' {
            let remaining = &line[index..];
            if remaining.starts_with("http://")
                || remaining.starts_with("https://")
                || remaining.starts_with("file://")
            {
                index += remaining
                    .find(char::is_whitespace)
                    .unwrap_or(remaining.len());
                continue;
            }
        }

        if bytes[index].is_ascii_alphabetic() {
            let start_byte = index;
            let mut has_digit = false;
            let mut has_underscore = false;
            let mut has_hyphen = false;

            while index < len {
                let byte = bytes[index];
                if byte.is_ascii_alphabetic() {
                    index += 1;
                } else if byte == b'\'' {
                    // Internal apostrophe (e.g. don't, user's)
                    if bytes.get(index + 1).is_some_and(u8::is_ascii_alphabetic) {
                        index += 2;
                    } else {
                        break;
                    }
                } else if byte == b'_' {
                    has_underscore = true;
                    break;
                } else if byte == b'-' {
                    has_hyphen = true;
                    break;
                } else if byte.is_ascii_digit() {
                    has_digit = true;
                    index += 1;
                } else {
                    break;
                }
            }

            let raw_word = &line[start_byte..index];

            // If token has digits (like sha256, utf8, v2, x86_64), skip
            if !has_digit && !raw_word.is_empty() {
                for &byte in &bytes[counted_byte..start_byte] {
                    utf16_column += match byte {
                        0x80..=0xBF => 0,
                        0xF0..=0xFF => 2,
                        _ => 1,
                    };
                }
                counted_byte = start_byte;
                split_and_add_tokens(raw_word, line_num, utf16_column, out);
            }

            if has_underscore || has_hyphen {
                index += 1;
            }
        } else {
            index += 1;
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
                end_col: base_utf16_col + raw.len() as u32,
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
            push_word_if_eligible(sub, line_num, base_utf16_col, chars[word_start].0, out);
            word_start = k;
        }
    }

    let last_sub = &raw[chars[word_start].0..];
    push_word_if_eligible(last_sub, line_num, base_utf16_col, chars[word_start].0, out);
}

fn push_word_if_eligible(
    word: &str,
    line_num: u32,
    base_utf16_col: u32,
    byte_offset_in_parent: usize,
    out: &mut Vec<WordToken>,
) {
    if word.len() <= 2 {
        return;
    }
    if word.len() <= 4 && word.chars().all(|c| c.is_ascii_uppercase()) {
        return;
    }

    // Tokens only contain ASCII, so byte offsets equal UTF-16 offsets.
    let start_col = base_utf16_col + byte_offset_in_parent as u32;
    let end_col = start_col + word.len() as u32;

    out.push(WordToken {
        text: word.to_string(),
        line: line_num,
        start_col,
        end_col,
        is_forced_typo: false,
    });
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
