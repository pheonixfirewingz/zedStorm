use crate::lsp::{Diagnostic, Position, Range};

pub struct JsonChecker;

impl JsonChecker {
    pub fn new() -> Self {
        Self
    }

    pub fn check(&self, text: &str) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();

        if text.trim().is_empty() {
            return diagnostics;
        }

        // Validate using serde_json_lenient to support comments and standard json syntax
        if let Err(err) = serde_json_lenient::from_str::<serde_json_lenient::Value>(text) {
            let line = err.line().saturating_sub(1) as u32;
            let col = err.column().saturating_sub(1) as u32;

            let lines: Vec<&str> = text.lines().collect();
            let end_col = if (line as usize) < lines.len() {
                lines[line as usize].len() as u32
            } else {
                col + 1
            };

            diagnostics.push(Diagnostic {
                range: Range {
                    start: Position {
                        line,
                        character: col,
                    },
                    end: Position {
                        line,
                        character: end_col.max(col + 1),
                    },
                },
                severity: 1, // Error
                source: "json-lsp".to_string(),
                message: err.to_string(),
                code: Some("json-syntax".to_string()),
            });
        }

        diagnostics
    }

    pub fn format(&self, text: &str) -> Option<String> {
        let val: serde_json_lenient::Value = serde_json_lenient::from_str(text).ok()?;
        serde_json::to_string_pretty(&val).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_json() {
        let checker = JsonChecker::new();
        let json_text = r#"{"name": "ZedStorm", "version": 1}"#;
        let diags = checker.check(json_text);
        assert!(diags.is_empty(), "Expected no diagnostics, got: {:?}", diags);
    }

    #[test]
    fn test_json_with_comments() {
        let checker = JsonChecker::new();
        let jsonc = "{\n  // This is a comment\n  \"key\": \"value\"\n}";
        let diags = checker.check(jsonc);
        assert!(diags.is_empty(), "Expected no diagnostics for jsonc, got: {:?}", diags);
    }

    #[test]
    fn test_invalid_json() {
        let checker = JsonChecker::new();
        let invalid = r#"{"name": "missing quote}"#;
        let diags = checker.check(invalid);
        assert!(!diags.is_empty());
        assert_eq!(diags[0].code.as_deref(), Some("json-syntax"));
    }
}
