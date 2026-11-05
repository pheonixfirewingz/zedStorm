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

    pub fn add_custom_word(&mut self, word: &str) {
        self.dictionary.add_custom_word(word);
    }

    pub fn ignore_word(&mut self, word: &str) {
        self.dictionary.ignore_word(word);
    }

    pub fn recheck_all_documents(&mut self) -> Vec<(String, Vec<Diagnostic>)> {
        let doc_uris: Vec<String> = self.documents.keys().cloned().collect();
        let mut results = Vec::new();
        for uri in doc_uris {
            if let Some((text, _)) = self.documents.get(&uri).cloned() {
                let diagnostics = self.check_document(&uri, &text);
                results.push((uri, diagnostics));
            }
        }
        results
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
                    format!("Typo: In word '{}'. Did you mean '{}'?", token.text, first)
                } else {
                    format!("Typo: In word '{}'.", token.text)
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
        use crate::lsp::Command;
        let mut actions = Vec::new();
        let Some((_, items)) = self.documents.get(uri) else {
            return actions;
        };

        for item in items {
            if ranges_overlap(&item.range, cursor_range) {
                // Quick fix suggestions (like WebStorm: Change to '<suggestion>')
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
                        command: None,
                    });
                }

                // WebStorm-style Dictionary options:
                // 1. Save to dictionary (persists to file in the companion binary location)
                actions.push(CodeAction {
                    title: format!("Save '{}' to dictionary", item.word),
                    kind: "quickfix".to_string(),
                    is_preferred: Some(false),
                    edit: None,
                    command: Some(Command {
                        title: format!("Save '{}' to dictionary", item.word),
                        command: "english-spell-checker.saveToDictionary".to_string(),
                        arguments: Some(vec![serde_json::Value::String(item.word.clone())]),
                    }),
                });

                // 2. Save to project dictionary (.zed/dictionary.txt)
                actions.push(CodeAction {
                    title: format!("Save '{}' to project", item.word),
                    kind: "quickfix".to_string(),
                    is_preferred: Some(false),
                    edit: None,
                    command: Some(Command {
                        title: format!("Save '{}' to project", item.word),
                        command: "english-spell-checker.saveToProject".to_string(),
                        arguments: Some(vec![
                            serde_json::Value::String(item.word.clone()),
                            serde_json::Value::String(uri.to_string()),
                        ]),
                    }),
                });

                // 3. Ignore word for session
                actions.push(CodeAction {
                    title: format!("Ignore '{}'", item.word),
                    kind: "quickfix".to_string(),
                    is_preferred: Some(false),
                    edit: None,
                    command: Some(Command {
                        title: format!("Ignore '{}'", item.word),
                        command: "english-spell-checker.ignoreWord".to_string(),
                        arguments: Some(vec![serde_json::Value::String(item.word.clone())]),
                    }),
                });

                // 4. Open dictionary file in editor (GUI flow to view/manage dictionary words)
                actions.push(CodeAction {
                    title: "Open dictionary...".to_string(),
                    kind: "quickfix".to_string(),
                    is_preferred: Some(false),
                    edit: None,
                    command: Some(Command {
                        title: "Open dictionary...".to_string(),
                        command: "english-spell-checker.openDictionary".to_string(),
                        arguments: None,
                    }),
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

    #[test]
    fn test_checker_respects_programming_language_keywords() {
        let mut checker = SpellChecker::new();

        // Rust: usize, dyn, println, dbg, pub, etc.
        let rust_code = "pub fn compute(data: &[u8], len: usize, item: &dyn Any) -> usize {\n    println!(\"len: {}\", len);\n    dbg!(item);\n    len\n}";
        let diags = checker.check_document("file:///main.rs", rust_code);
        assert_eq!(diags.len(), 0, "Rust keywords should not trigger spelling diagnostics");

        // Python: elif, nonlocal, kwargs, isinstance, repr, etc.
        let py_code = "def process(*args, **kwargs):\n    if not args:\n        pass\n    elif not isinstance(kwargs, dict):\n        return repr(kwargs)\n";
        let diags = checker.check_document("file:///app.py", py_code);
        assert_eq!(diags.len(), 0, "Python keywords should not trigger spelling diagnostics");

        // C++: constexpr, nullptr, decltype, noexcept
        let cpp_code = "constexpr auto ptr = nullptr;\nvoid run() noexcept {\n    decltype(ptr) x = nullptr;\n}";
        let diags = checker.check_document("file:///main.cpp", cpp_code);
        assert_eq!(diags.len(), 0, "C++ keywords should not trigger spelling diagnostics");

        // Go: defer, iota, fallthrough, rune, goroutine
        let go_code = "func cleanup() {\n    defer file.Close()\n    const offset = iota\n}";
        let diags = checker.check_document("file:///server.go", go_code);
        assert_eq!(diags.len(), 0, "Go keywords should not trigger spelling diagnostics");

        // TypeScript: typeof, instanceof, keyof
        let ts_code = "if (typeof value === 'string' || value instanceof CustomError) {\n    type PropKeys = keyof CustomError;\n}";
        let diags = checker.check_document("file:///index.ts", ts_code);
        assert_eq!(diags.len(), 0, "TypeScript keywords should not trigger spelling diagnostics");

        // Ruby: elsif, redo, retry
        let ruby_code = "if condition\n    redo\nelsif alternative\n    retry\nend";
        let diags = checker.check_document("file:///script.rb", ruby_code);
        assert_eq!(diags.len(), 0, "Ruby keywords should not trigger spelling diagnostics");
    }

    #[test]
    fn test_code_actions_provide_dictionary_options() {
        let mut checker = SpellChecker::new();
        let diags = checker.check_document("file:///test.rs", "fn mycustomidentifier() {}");
        assert_eq!(diags.len(), 1);

        let actions = checker.get_code_actions("file:///test.rs", &diags[0].range);
        let titles: Vec<&str> = actions.iter().map(|a| a.title.as_str()).collect();

        assert!(titles.iter().any(|t| t == &"Save 'mycustomidentifier' to dictionary"));
        assert!(titles.iter().any(|t| t == &"Save 'mycustomidentifier' to project"));
        assert!(titles.iter().any(|t| t == &"Ignore 'mycustomidentifier'"));
        assert!(titles.iter().any(|t| t == &"Open dictionary..."));

        // Check command payloads
        let user_action = actions.iter().find(|a| a.title.contains("to dictionary")).unwrap();
        let cmd = user_action.command.as_ref().unwrap();
        assert_eq!(cmd.command, "english-spell-checker.saveToDictionary");
        assert_eq!(cmd.arguments.as_ref().unwrap()[0], "mycustomidentifier");

        let ws_action = actions.iter().find(|a| a.title.contains("to project")).unwrap();
        let ws_cmd = ws_action.command.as_ref().unwrap();
        assert_eq!(ws_cmd.command, "english-spell-checker.saveToProject");
        assert_eq!(ws_cmd.arguments.as_ref().unwrap()[0], "mycustomidentifier");
        assert_eq!(ws_cmd.arguments.as_ref().unwrap()[1], "file:///test.rs");

        let ignore_action = actions.iter().find(|a| a.title.contains("Ignore")).unwrap();
        let ign_cmd = ignore_action.command.as_ref().unwrap();
        assert_eq!(ign_cmd.command, "english-spell-checker.ignoreWord");
        assert_eq!(ign_cmd.arguments.as_ref().unwrap()[0], "mycustomidentifier");

        let open_action = actions.iter().find(|a| a.title == "Open dictionary...").unwrap();
        let open_cmd = open_action.command.as_ref().unwrap();
        assert_eq!(open_cmd.command, "english-spell-checker.openDictionary");
    }

    #[test]
    fn test_add_custom_word_removes_diagnostics() {
        let mut checker = SpellChecker::new();
        let diags = checker.check_document("file:///test.rs", "let mycustomterm = 42;");
        assert_eq!(diags.len(), 1);

        checker.add_custom_word("mycustomterm");
        let updated = checker.recheck_all_documents();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].1.len(), 0, "Diagnostic should be cleared after adding word to dictionary");
    }

    #[test]
    fn test_ignore_word_removes_diagnostics() {
        let mut checker = SpellChecker::new();
        let diags = checker.check_document("file:///test.rs", "let sessionterm = 42;");
        assert_eq!(diags.len(), 1);

        checker.ignore_word("sessionterm");
        let updated = checker.recheck_all_documents();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].1.len(), 0, "Diagnostic should be cleared after ignoring word for session");
    }
}
