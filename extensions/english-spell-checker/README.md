# English Spell Checker for Zed

A fast, fully self-contained English spell checker extension for the Zed editor that requires **zero external tools**, runtimes, or network downloads.

Unlike CSpell (which requires Node.js/npm) or Typos (which requires downloading external platform binaries from GitHub at runtime), this extension includes an embedded, high-performance pure Rust Language Server and curated English dictionary.

## Features

- **No External Dependencies**: No npm, Node.js, Python runtime, `aspell`, or `hunspell` needed.
- **Offline & Air-gapped Ready**: Embedded English vocabulary of 20,000+ words plus comprehensive software/technical terminology.
- **Smart Morphological Rules**: Understands plurals (`-s`, `-es`, `-ies`), past tense (`-ed`), gerunds (`-ing`), adverbs (`-ly`), agent nouns (`-er`), and prefixes (`un-`, `re-`, `dis-`, etc.), effectively recognizing over 100,000 inflected English word forms.
- **Fast Typo Detection & Quick Fixes**:
  - Damerau-Levenshtein edit distance calculations (< 1ms per document)
  - Curated fast-path for hundreds of frequent typing and code mistakes
  - Contextual typo recognition (e.g. flagging "dose not" as a typo for "does not")
  - Quick Fix code actions (`Ctrl+.` or `Cmd+.`) to replace misspelled words in one click
- **Code-Aware Tokenizer**:
  - Automatically splits `camelCase`, `PascalCase`, `snake_case`, and `kebab-case` identifiers
  - Ignores numbers, hex codes (`0x1a2b`, `#ffffff`), URLs, and common acronyms (`JSON`, `HTTP`, `WASM`, `HTML`, etc.)
  - Handles English apostrophes properly (`don't`, `it's`, `user's`)
- **Broad Language Support**: Enabled across Markdown, Plain Text, Rust, Python, JavaScript, TypeScript, Go, HTML, CSS, JSON, TOML, YAML, C, C++, Ruby, and Git Commit messages.

## Structure

```text
extensions/english-spell-checker/
├── extension.toml         # Extension manifest registering the LSP server
├── extension.wasm         # Compiled WebAssembly Zed extension module
├── Cargo.toml             # Cargo manifest for the extension crate
├── install.py             # Companion installer script
├── bin/
│   └── english-spell-checker.exe  # Standalone companion LSP server executable
├── src/
│   ├── english_spell_checker.rs   # Zed extension API integration
│   └── tools_path.rs              # Managed tool path resolution
└── native/                # Companion LSP server source code
    ├── Cargo.toml
    └── src/
        ├── dictionary.rs  # Embedded dictionary & typo suggestion engine
        ├── tokenizer.rs   # Code & prose word tokenizer
        ├── checker.rs     # Diagnostic & CodeAction generator
        ├── lsp.rs         # JSON-RPC / LSP protocol types
        ├── main.rs        # Stdio event loop
        └── words.txt      # Curated word list
```

## Build and Install

### 1. Build the Companion Binary
```sh
python extensions/english-spell-checker/install.py
```
This builds `english-spell-checker` and installs it to Zed's managed tools directory (`zedstorm-tools/extensions/english-spell-checker/bin`) as well as the local `bin/` folder.

### 2. Build the WASM Extension
```sh
cargo build --manifest-path extensions/english-spell-checker/Cargo.toml --target wasm32-wasip2 --target-dir target/ext
copy target/ext/wasm32-wasip2/debug/zed_english_spell_checker.wasm extensions/english-spell-checker/extension.wasm
```

### 3. Load as a Dev Extension in Zed
1. In Zed, open the command palette (`Ctrl+Shift+P` / `Cmd+Shift+P`).
2. Type and run `zed: install dev extension`.
3. Choose the `extensions/english-spell-checker` directory.

## Testing

Run unit tests for dictionary lookup, tokenizer, and diagnostics:
```sh
cargo test --manifest-path extensions/english-spell-checker/native/Cargo.toml --target-dir target/ext
```
