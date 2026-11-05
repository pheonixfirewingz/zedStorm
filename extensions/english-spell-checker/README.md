# English Spell Checker for Zed

A fast, fully self-contained English spell checker extension for the Zed editor that requires **zero external tools**, runtimes, or network downloads.

Unlike CSpell (which requires Node.js/npm) or Typos (which requires downloading external platform binaries from GitHub at runtime), this extension includes an embedded, high-performance pure Rust Language Server and curated English dictionary.

## Features

- **No External Dependencies**: No npm, Node.js, Python runtime, `aspell`, or `hunspell` needed.
- **Offline & Air-gapped Ready**: Embedded English vocabulary of 20,000+ words plus comprehensive software/technical terminology.
- **Smart Morphological Rules**: Understands plurals (`-s`, `-es`, `-ies`), past tense (`-ed`), gerunds (`-ing`), adverbs (`-ly`), agent nouns (`-er`), and prefixes (`un-`, `re-`, `dis-`, etc.), effectively recognizing over 100,000 inflected English word forms.
- **WebStorm-style Inspection & GUI Flow**:
  - Typo diagnostics formatted WebStorm-style: `Typo: In word '<word>'` with direct suggestion
  - **Change to '<suggestion>'**: One-click quickfix to apply suggested spelling replacement
  - **Save '<word>' to dictionary**: Instantly saves the word to `dictionary.txt` located directly in the companion binary's folder (`<binary_dir>/dictionary.txt`), automatically clearing underlines across all open documents
  - **Save '<word>' to project**: Persists project-specific terminology into `.zed/dictionary.txt` within the project workspace root
  - **Ignore '<word>'**: Suppresses spelling diagnostics for the word during the current session
  - **Open dictionary...**: Opens the dictionary file in Zed for viewing or editing (via LSP `window/showDocument`)
- **Settings & Dictionary File Support**:
  - Automatically loads custom words from `dictionary.txt` placed alongside the binary on startup
  - Supports custom word lists (`custom_words`) and dictionary file paths (`dictionary`) in Zed's `settings.json`
- **Code-Aware Tokenizer & Keyword Respect**:
  - Full dictionary recognition for programming languages' keywords (Rust, Python, Go, C, C++, JavaScript, TypeScript, Ruby, PHP, Java, Kotlin, C#, Swift, SQL, Shell, Lua, Zig, etc.)
  - Automatically splits `camelCase`, `PascalCase`, `snake_case`, and `kebab-case` identifiers
  - Ignores numbers, hex codes (`0x1a2b`, `#ffffff`), URLs, and common acronyms (`JSON`, `HTTP`, `WASM`, `HTML`, etc.)
  - Handles English apostrophes properly (`don't`, `it's`, `user's`)
- **Broad Language Support**: Enabled across Markdown, Plain Text, Rust, Python, JavaScript, TypeScript, Go, HTML, CSS, JSON, TOML, YAML, C, C++, Ruby, Git Commit, PHP, Java, Kotlin, C#, Swift, SQL, Shell Script, Bash, Lua, and Zig.

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
        ├── storage.rs     # User & workspace dictionary persistence
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
