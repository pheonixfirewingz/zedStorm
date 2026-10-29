use std::collections::HashMap;

use crate::dictionary::Dictionary;
use crate::lsp::{CodeAction, Diagnostic, Position, Range, TextEdit, WorkspaceEdit};
use crate::tokenizer::tokenize_document;

#[derive(Debug, Clone)]
pub struct DiagnosticItem {
    pub range: Range,
    pub word: String,
    pub suggestions: Vec<String>,
}

pub struct SpellChecker {
    dictionary: Dictionary,
    documents: HashMap<String, (String, Vec<DiagnosticItem>)>,
}

impl Default for SpellChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl SpellChecker {
    pub fn new() -> Self {
        Self {
            dictionary: Dictionary::new(),
            documents: HashMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn add_custom_word(&mut self, word: &str) {
        self.dictionary.add_custom_word(word);
    }

    #[allow(dead_code)]
    pub fn ignore_word(&mut self, word: &str) {
        self.dictionary.ignore_word(word);
    }

    pub fn close_document(&mut self, uri: &str) {
        self.documents.remove(uri);
    }

    pub fn check_document(&mut self, uri: &str, text: &str) -> Vec<Diagnostic> {
        let tokens = tokenize_document(text);
        let mut diagnostics = Vec::new();
        let mut items = Vec::new();

        for token in tokens {
            let is_typo = token.is_forced_typo || !self.dictionary.is_valid(&token.text);

            if is_typo {
                let suggestions = self.dictionary.suggest(&token.text);
                let range = Range {
                    start: Position {
                        line: token.line,
                        character: token.start_col,
                    },
                    end: Position {
                        line: token.line,
                        character: token.end_col,
                    },
                };

                let message = if let Some(first) = suggestions.first() {
                    format!("Possible spelling mistake: '{}'. Did you mean '{}'?", token.text, first)
                } else {
                    format!("Possible spelling mistake: '{}'.", token.text)
                };

                diagnostics.push(Diagnostic {
                    range: range.clone(),
                    severity: 2, // Warning
                    source: "english-spell-checker".to_string(),
                    message,
                    data: Some(suggestions.clone()),
                });

                items.push(DiagnosticItem {
                    range,
                    word: token.text,
                    suggestions,
                });
            }
        }

        self.documents.insert(uri.to_string(), (text.to_string(), items));
        diagnostics
    }

    pub fn get_code_actions(&self, uri: &str, cursor_range: &Range) -> Vec<CodeAction> {
        let mut actions = Vec::new();
        let Some((_, items)) = self.documents.get(uri) else {
            return actions;
        };

        for item in items {
            if ranges_overlap(&item.range, cursor_range) {
                // Quick fix suggestions
                for (idx, suggestion) in item.suggestions.iter().enumerate() {
                    let mut changes = HashMap::new();
                    changes.insert(
                        uri.to_string(),
                        vec![TextEdit {
                            range: item.range.clone(),
                            new_text: suggestion.clone(),
                        }],
                    );

                    actions.push(CodeAction {
                        title: format!("Change to '{}'", suggestion),
                        kind: "quickfix".to_string(),
                        is_preferred: Some(idx == 0),
                        edit: Some(WorkspaceEdit { changes }),
                    });
                }

                // Add to dictionary / ignore options
                actions.push(CodeAction {
                    title: format!("Add '{}' to dictionary", item.word),
                    kind: "quickfix".to_string(),
                    is_preferred: Some(false),
                    edit: None,
                });

                actions.push(CodeAction {
                    title: format!("Ignore '{}' for this session", item.word),
                    kind: "quickfix".to_string(),
                    is_preferred: Some(false),
                    edit: None,
                });
            }
        }

        actions
    }
}

fn ranges_overlap(r1: &Range, r2: &Range) -> bool {
    if r1.end.line < r2.start.line || r2.end.line < r1.start.line {
        return false;
    }
    if r1.end.line == r2.start.line && r1.end.character < r2.start.character {
        return false;
    }
    if r2.end.line == r1.start.line && r2.end.character < r1.start.character {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checker_detects_misspelling() {
        let mut checker = SpellChecker::new();
        let diags = checker.check_document("file:///test.rs", "fn recieve_data() {}");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range.start.line, 0);
        assert!(diags[0].message.contains("recieve"));

        let actions = checker.get_code_actions("file:///test.rs", &diags[0].range);
        assert!(!actions.is_empty());
        assert_eq!(actions[0].title, "Change to 'receive'");
    }

    #[test]
    fn test_checker_detects_dose_not() {
        let mut checker = SpellChecker::new();
        let diags = checker.check_document("file:///test.md", "make an extension that dose not need an external tool");
        let dose_diag = diags.iter().find(|d| d.message.contains("dose"));
        assert!(dose_diag.is_some());
    }
}
