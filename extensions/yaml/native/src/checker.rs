use crate::lsp::{CodeAction, Diagnostic, Position, Range, TextEdit, WorkspaceEdit};
use std::collections::HashMap;

pub struct YamlChecker;

impl YamlChecker {
    pub fn new() -> Self {
        Self
    }

    pub fn check(&self, text: &str) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        let lines: Vec<&str> = text.lines().collect();

        // 1. Check for tab characters in indentation (forbidden in YAML)
        for (line_idx, line) in lines.iter().enumerate() {
            let leading_tabs = line.chars().take_while(|c| *c == '\t').count();
            if leading_tabs > 0 {
                diagnostics.push(Diagnostic {
                    range: Range {
                        start: Position {
                            line: line_idx as u32,
                            character: 0,
                        },
                        end: Position {
                            line: line_idx as u32,
                            character: leading_tabs as u32,
                        },
                    },
                    severity: 1, // Error
                    source: "yaml-lsp".to_string(),
                    message: "Tab characters are not allowed for indentation in YAML. Use spaces instead.".to_string(),
                    code: Some("yaml-no-tabs".to_string()),
                });
            }
        }

        // 2. Validate YAML syntax using serde_yaml_ng
        if let Err(err) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text) {
            let (line, col) = if let Some(loc) = err.location() {
                // loc.line() is 1-indexed, convert to 0-indexed
                let l = loc.line().saturating_sub(1) as u32;
                let c = loc.column().saturating_sub(1) as u32;
                (l, c)
            } else {
                (0, 0)
            };

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
                source: "yaml-lsp".to_string(),
                message: err.to_string(),
                code: Some("yaml-syntax".to_string()),
            });
        }

        diagnostics
    }

    pub fn code_actions(&self, uri: &str, text: &str, range: &Range) -> Vec<CodeAction> {
        let mut actions = Vec::new();
        let lines: Vec<&str> = text.lines().collect();

        let line_idx = range.start.line as usize;
        if line_idx < lines.len() {
            let line = lines[line_idx];
            let leading_tabs = line.chars().take_while(|c| *c == '\t').count();
            if leading_tabs > 0 {
                let spaces = "  ".repeat(leading_tabs);
                let mut changes = HashMap::new();
                changes.insert(
                    uri.to_string(),
                    vec![TextEdit {
                        range: Range {
                            start: Position {
                                line: line_idx as u32,
                                character: 0,
                            },
                            end: Position {
                                line: line_idx as u32,
                                character: leading_tabs as u32,
                            },
                        },
                        new_text: spaces,
                    }],
                );

                actions.push(CodeAction {
                    title: "Convert tabs to spaces".to_string(),
                    kind: "quickfix".to_string(),
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                    }),
                });
            }
        }

        actions
    }

    pub fn format(&self, text: &str) -> Option<String> {
        // Attempt formatting via parse + serialize
        let val: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).ok()?;
        serde_yaml_ng::to_string(&val).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_yaml() {
        let checker = YamlChecker::new();
        let yaml = "name: ZedStorm\nversion: 1.0\nitems:\n  - one\n  - two\n";
        let diags = checker.check(yaml);
        assert!(diags.is_empty(), "Expected no diagnostics for valid YAML, got: {:?}", diags);
    }

    #[test]
    fn test_detect_tabs() {
        let checker = YamlChecker::new();
        let yaml = "name: ZedStorm\n\tinvalid_tab: true\n";
        let diags = checker.check(yaml);
        assert!(!diags.is_empty());
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("yaml-no-tabs")));
    }

    #[test]
    fn test_syntax_error() {
        let checker = YamlChecker::new();
        let yaml = "name: [unclosed bracket\n";
        let diags = checker.check(yaml);
        assert!(!diags.is_empty());
        assert!(diags.iter().any(|d| d.code.as_deref() == Some("yaml-syntax")));
    }
}
