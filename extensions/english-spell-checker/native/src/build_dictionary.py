import sys
from pathlib import Path

script_dir = Path("extensions/english-spell-checker/native/src")
raw_20k = script_dir / "google-20k.txt"
out_file = script_dir / "words.txt"

words = set()

if raw_20k.exists():
    with open(raw_20k, "r", encoding="utf-8") as f:
        for line in f:
            w = line.strip().lower()
            if w and w.isalpha():
                words.add(w)
elif out_file.exists():
    with open(out_file, "r", encoding="utf-8") as f:
        for line in f:
            w = line.strip().lower()
            if w:
                words.add(w)

# Add common contractions
contractions = [
    "don't", "doesn't", "didn't", "won't", "wouldn't", "can't", "cannot", "couldn't",
    "shouldn't", "mustn't", "isn't", "aren't", "wasn't", "weren't", "hasn't", "haven't",
    "hadn't", "it's", "that's", "what's", "there's", "here's", "let's", "they're",
    "we're", "you're", "i'm", "i've", "we've", "you've", "they've", "i'd", "we'd",
    "you'd", "they'd", "i'll", "we'll", "you'll", "they'll"
]
for c in contractions:
    words.add(c)

# Add technical and programming vocabulary
tech_words = [
    "api", "apis", "app", "apps", "arg", "args", "argument", "arguments", "array", "arrays",
    "ascii", "assert", "assertion", "assertions", "async", "atomic", "attribute", "attributes",
    "auth", "authentication", "auto", "await", "backend", "backends", "base64", "binary", "binaries",
    "bind", "binding", "bindings", "bitmap", "blob", "block", "blocks", "blocking", "bool", "boolean",
    "booleans", "buffer", "buffers", "byte", "bytes", "bytecode", "cache", "caches", "cached", "caching",
    "callback", "callbacks", "canvas", "capture", "channel", "channels", "char", "chars", "checkbox",
    "checksum", "cipher", "cli", "client", "clients", "clone", "cloned", "cloning", "closure", "closures",
    "cmd", "codec", "codecs", "colormap", "column", "columns", "command", "commands", "commit", "commits",
    "committed", "committing", "compiler", "compilers", "config", "configs", "configuration", "configurations",
    "const", "constant", "constants", "context", "contexts", "cookie", "cookies", "cors", "cpu", "crypto",
    "daemon", "data", "database", "databases", "datasource", "deadlock", "debug", "debugger", "debugging",
    "decode", "decoded", "decoder", "decoders", "decoding", "decompress", "decompression", "decrypt", "decryption",
    "default", "defaults", "delete", "deleted", "delimiter", "delimiters", "deploy", "deployment", "deprecated",
    "deprecation", "deref", "destructor", "deserialize", "deserialization", "diff", "digest", "directory",
    "directories", "dispatch", "dispatcher", "dispatching", "dns", "doc", "docs", "doctype", "document",
    "documents", "dom", "downloader", "dto", "dylib", "dynamic", "editor", "element", "elements", "emit",
    "emitter", "encode", "encoded", "encoder", "encoding", "encrypt", "encryption", "endpoint", "endpoints",
    "enum", "enums", "env", "environ", "environment", "epoch", "errno", "error", "errors", "escape",
    "escaped", "event", "events", "exception", "exceptions", "exec", "executable", "executables", "execute",
    "executed", "executing", "execution", "executor", "expr", "expression", "expressions", "failover",
    "fallback", "fifo", "filesystem", "filter", "filters", "filtered", "filtering", "flag", "flags",
    "float", "floating", "flush", "flushed", "fn", "font", "fonts", "footer", "fork", "format", "formats",
    "formatter", "formatting", "fps", "frame", "frames", "frontend", "frontends", "fs", "func", "function",
    "functions", "functional", "garbage", "gateway", "gcc", "generator", "generators", "generic", "generics",
    "getter", "getters", "gid", "git", "github", "gitlab", "gpu", "grep", "grid", "group", "groups",
    "guid", "gutter", "gzip", "handle", "handler", "handlers", "handshake", "hash", "hashes", "hashed",
    "hashing", "hashmap", "header", "headers", "heap", "hexdump", "hook", "hooks", "host", "hostname",
    "html", "http", "https", "icon", "icons", "id", "identifier", "identifiers", "idle", "iframe",
    "image", "images", "immutable", "impl", "implement", "implemented", "implementation", "implementations",
    "import", "imports", "imported", "importing", "index", "indexes", "indices", "indexed", "indexing",
    "inheritance", "init", "initial", "initialize", "initialized", "initializer", "initializing", "inline",
    "inlined", "input", "inputs", "instance", "instances", "instantiate", "instantiated", "int", "integer",
    "integers", "interface", "interfaces", "interpreter", "interrupt", "interval", "ioctl", "ipc",
    "ipv4", "ipv6", "iterable", "iterator", "iterators", "iter", "javascript", "json", "jsonp", "jvm",
    "jwt", "kernel", "keyboard", "keymap", "keystroke", "keyword", "keywords", "kill", "lambda", "latency",
    "layout", "lexer", "lib", "library", "libraries", "lifecycle", "lifetime", "lifetimes", "limit",
    "limiter", "lineno", "lint", "linter", "lints", "linting", "linux", "literal", "literals", "llvm",
    "load", "loader", "lock", "locked", "locking", "log", "logger", "logging", "login", "logout", "lookup",
    "loop", "lsp", "macos", "macro", "macros", "mailbox", "malloc", "manifest", "manifests", "mapper",
    "mapping", "mappings", "marshal", "marshaling", "match", "matches", "matching", "matrix", "matrices",
    "md5", "mem", "memcpy", "memory", "merge", "merged", "merging", "mesh", "metadata", "method", "methods",
    "microservice", "middleware", "minified", "minimap", "mkdir", "mmap", "mock", "mocks", "mod", "mode",
    "modifier", "modifiers", "module", "modules", "monad", "mount", "mounted", "mouse", "msg", "multi",
    "multicast", "multithreaded", "mutex", "mut", "mutable", "namespace", "namespaces", "native", "nested",
    "net", "network", "networking", "newline", "nil", "node", "nodes", "noop", "normalize", "normalized",
    "notification", "notifications", "null", "nullable", "num", "number", "numbers", "numeric", "oauth",
    "obj", "object", "objects", "offset", "offsets", "online", "opcode", "operand", "operands", "operation",
    "operations", "operator", "operators", "optimize", "optimized", "optimizer", "optimizing", "option",
    "options", "optional", "output", "outputs", "overflow", "overhead", "overlay", "override", "overridden",
    "packet", "packets", "padding", "pane", "panes", "param", "params", "parameter", "parameters", "parse",
    "parsed", "parser", "parsers", "parsing", "patch", "payload", "payloads", "peer", "peers", "pending",
    "permission", "permissions", "persist", "persistent", "persistence", "pipeline", "pipelines", "pixel",
    "pixels", "pkg", "placeholder", "plaintext", "platform", "platforms", "plugin", "plugins", "pointer",
    "pointers", "polyfill", "pool", "popup", "port", "ports", "pos", "position", "pr", "pragma", "precedence",
    "prefix", "prepend", "preprocess", "preprocessor", "prev", "previous", "proc", "process", "processes",
    "processed", "processing", "processor", "profile", "profiler", "profiling", "prompt", "proto", "protocol",
    "protocols", "prototype", "provider", "providers", "proxy", "ptr", "pub", "public", "publish",
    "published", "publisher", "push", "pushed", "query", "queries", "queue", "queues", "queued", "quote",
    "quoted", "radix", "random", "range", "ranges", "read", "readable", "reader", "readonly", "reboot",
    "recv", "redirect", "redirects", "redirection", "ref", "reference", "references", "referenced",
    "referencing", "refactor", "refactoring", "regex", "regexp", "register", "registered", "registry",
    "release", "releases", "remote", "remotes", "render", "rendered", "renderer", "renderers", "rendering",
    "repo", "repos", "repository", "repositories", "req", "request", "requests", "res", "reset", "resolve",
    "resolved", "resolver", "resolvers", "resolving", "resource", "resources", "resp", "response",
    "responses", "restart", "restore", "result", "results", "retry", "retries", "rgb", "rgba", "rmdir",
    "root", "route", "router", "routes", "routing", "rpc", "runtime", "runtimes", "rust", "safe", "sandbox",
    "scan", "scanner", "schema", "schemas", "scope", "scopes", "screenshot", "script", "scripts", "scroll",
    "scrollbar", "sdk", "sdks", "search", "section", "sections", "secure", "security", "segment", "selector",
    "selectors", "semantic", "semaphore", "semicolon", "sender", "separator", "separators", "seq",
    "sequence", "sequences", "serde", "serialize", "serialization", "server", "servers", "setter",
    "setters", "sha", "sha256", "shader", "shaders", "shell", "shortcut", "shutdown", "sig", "signal",
    "signals", "signature", "signatures", "sink", "size", "slice", "slices", "slot", "slots", "snapshot",
    "snapshots", "socket", "sockets", "source", "sources", "span", "spans", "spawn", "spec", "special",
    "specs", "specification", "split", "sql", "sqlite", "src", "ssh", "ssl", "stack", "standalone",
    "state", "statement", "statements", "static", "status", "stderr", "stdin", "stdio", "stdout", "stream",
    "streams", "string", "strings", "struct", "structs", "structure", "structures", "style", "styles",
    "stylesheet", "subagent", "subcommand", "submodule", "subscribe", "subscriber", "subscription",
    "substring", "suite", "summary", "suspend", "symbol", "symbols", "sync", "synchronize", "synchronous",
    "syntax", "syscall", "tar", "target", "targets", "task", "tasks", "tcp", "telemetry", "template",
    "templates", "temporary", "term", "terminal", "terminals", "test", "tests", "testing", "textbox",
    "textarea", "theme", "themes", "thread", "threads", "throughput", "timeout", "timeouts", "timestamp",
    "timestamps", "tls", "tmp", "token", "tokens", "tokenizer", "toml", "tooltip", "trace", "tracer",
    "tracing", "trait", "traits", "transaction", "transactions", "transit", "transition", "traverse",
    "tree", "trees", "trigger", "triggers", "try", "tuple", "tuples", "type", "types", "typedef", "typescript",
    "udp", "ui", "uint", "unary", "undefined", "underline", "unicode", "union", "unique", "unix", "unsafe",
    "unsigned", "unzip", "uri", "uris", "url", "urls", "user", "users", "utf", "utf8", "uuid", "uuids",
    "val", "validate", "validation", "validator", "var", "variable", "variables", "vec", "vector", "vectors",
    "vendor", "verbose", "version", "versions", "versioning", "view", "viewport", "views", "virtual",
    "visibility", "void", "vs", "vscode", "wasm", "watcher", "watchers", "webview", "websocket", "widget",
    "widgets", "width", "window", "windows", "worker", "workers", "workspace", "workspaces", "worktree",
    "worktrees", "wrap", "wrapper", "xml", "yaml", "yarn", "yield", "zip"
]
for t in tech_words:
    words.add(t)

# Comprehensive programming languages keywords
programming_keywords = [
    # Rust
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
    "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait",
    "true", "type", "unsafe", "use", "where", "while", "yield", "abstract", "become",
    "box", "do", "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "try",
    "bool", "char", "str", "usize", "isize", "u8", "u16", "u32", "u64", "u128",
    "i8", "i16", "i32", "i64", "i128", "f32", "f64", "println", "eprintln", "panic",
    "unreachable", "unimplemented", "todo", "dbg", "vec", "repr", "inline", "cfg", "derive",
    "allow", "warn", "deny", "forbid", "refcell", "rwlock", "mutex", "atomic",
    # Python
    "false", "none", "true", "and", "as", "assert", "async", "await", "break", "class",
    "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
    "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise",
    "return", "try", "while", "with", "yield", "match", "case", "len", "str", "int",
    "float", "bool", "list", "dict", "set", "tuple", "bytes", "bytearray", "range",
    "enumerate", "zip", "map", "filter", "iter", "next", "repr", "super", "print", "open",
    "isinstance", "issubclass", "hasattr", "getattr", "setattr", "delattr", "callable",
    "classmethod", "staticmethod", "property", "args", "kwargs", "self", "cls", "dunder",
    "init", "enter", "exit", "getitem", "setitem", "delitem",
    # JS / TS
    "abstract", "arguments", "as", "async", "await", "boolean", "break", "byte", "case",
    "catch", "char", "class", "const", "continue", "debugger", "default", "delete", "do",
    "double", "else", "enum", "eval", "export", "extends", "false", "final", "finally",
    "float", "for", "from", "function", "get", "goto", "if", "implements", "import", "in",
    "instanceof", "int", "interface", "is", "keyof", "let", "long", "native", "new", "null",
    "number", "of", "package", "private", "protected", "public", "readonly", "require",
    "return", "set", "short", "static", "string", "super", "switch", "symbol", "synchronized",
    "this", "throw", "throws", "transient", "true", "try", "type", "typeof", "undefined",
    "unique", "unknown", "var", "void", "volatile", "while", "with", "yield", "any", "never",
    "infer", "declare", "namespace", "module", "override", "satisfies", "bigint", "nan",
    "prototype", "constructor", "promisify", "promiselike",
    # Go
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough",
    "for", "func", "go", "goto", "if", "import", "interface", "map", "package", "range",
    "return", "select", "struct", "switch", "type", "var", "true", "false", "iota", "nil",
    "append", "cap", "close", "complex", "copy", "delete", "imag", "len", "make", "new",
    "panic", "print", "println", "real", "recover", "clear", "min", "max", "bool", "byte",
    "complex64", "complex128", "error", "float32", "float64", "int", "int8", "int16",
    "int32", "int64", "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64",
    "uintptr", "goroutine", "comparable",
    # C / C++
    "alignas", "alignof", "and", "and_eq", "asm", "atomic_cancel", "atomic_commit",
    "atomic_noexcept", "auto", "bitand", "bitor", "bool", "break", "case", "catch", "char",
    "char8_t", "char16_t", "char32_t", "class", "compl", "concept", "const", "consteval",
    "constexpr", "constinit", "const_cast", "continue", "co_await", "co_return", "co_yield",
    "decltype", "default", "delete", "do", "double", "dynamic_cast", "else", "enum",
    "explicit", "export", "extern", "false", "float", "for", "friend", "goto", "if", "inline",
    "int", "long", "mutable", "namespace", "new", "noexcept", "not", "not_eq", "nullptr",
    "operator", "or", "or_eq", "private", "protected", "public", "reflexpr", "register",
    "reinterpret_cast", "requires", "restrict", "return", "short", "signed", "sizeof",
    "static", "static_assert", "static_cast", "struct", "switch", "synchronized", "template",
    "this", "thread_local", "throw", "true", "try", "typedef", "typeid", "typename", "union",
    "unsigned", "using", "virtual", "void", "volatile", "wchar_t", "while", "xor", "xor_eq",
    "size_t", "ssize_t", "ptrdiff_t", "intptr_t", "uintptr_t", "intmax_t", "uintmax_t",
    "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t",
    "printf", "fprintf", "sprintf", "snprintf", "scanf", "sscanf", "malloc", "calloc",
    "realloc", "free", "memcpy", "memmove", "memset", "memcmp", "strlen", "strcpy",
    "strncpy", "strcat", "strcmp", "strncmp", "strdup", "strchr", "strstr", "strtok",
    "atoi", "atol", "atof", "strtol", "strtoul", "qsort", "bsearch", "abort", "exit",
    "argc", "argv", "stdin", "stdout", "stderr", "pragma", "include", "define", "undef",
    "ifdef", "ifndef", "endif", "cout", "cin", "cerr", "clog", "endl", "nullptr_t",
    "std", "vector", "string", "map", "set", "unordered_map", "unordered_set", "deque",
    "list", "array", "pair", "tuple", "unique_ptr", "shared_ptr", "weak_ptr", "make_shared",
    "make_unique", "ssize", "ptrdiff", "intptr", "uintptr", "intmax", "uintmax", "reinterpret",
    # Ruby
    "alias", "and", "begin", "break", "case", "class", "def", "defined", "do", "else",
    "elsif", "end", "ensure", "false", "for", "if", "in", "module", "next", "nil",
    "not", "or", "redo", "rescue", "retry", "return", "self", "super", "then", "true",
    "undef", "unless", "until", "when", "while", "yield", "attr_accessor", "attr_reader",
    "attr_writer", "lambda", "proc", "puts", "raise", "include", "extend", "prepend",
    # PHP
    "echo", "print", "die", "exit", "isset", "unset", "empty", "eval", "include", "require",
    "include_once", "require_once", "namespace", "use", "function", "fn", "class", "trait",
    "interface", "extends", "implements", "public", "protected", "private", "static", "final",
    "abstract", "global", "var", "const", "return", "try", "catch", "finally", "throw",
    "if", "else", "elseif", "endif", "while", "endwhile", "do", "for", "endfor", "foreach",
    "endforeach", "switch", "case", "default", "break", "continue", "match", "yield",
    "declare", "goto", "clone", "instanceof", "readonly", "enum",
    # Java & Kotlin
    "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class",
    "const", "continue", "default", "do", "double", "else", "enum", "extends", "final",
    "finally", "float", "for", "goto", "if", "implements", "import", "instanceof", "int",
    "interface", "long", "native", "new", "package", "private", "protected", "public",
    "return", "short", "static", "strictfp", "super", "switch", "synchronized", "this",
    "throw", "throws", "transient", "try", "void", "volatile", "while", "val", "var",
    "fun", "when", "sealed", "data", "companion", "override", "open", "suspend", "coroutine",
    "nullable", "nonnull", "record", "permits", "yield", "vararg", "inline", "noinline",
    "crossinline", "reified", "it", "init",
    # C#
    "abstract", "as", "base", "bool", "break", "byte", "case", "catch", "char", "checked",
    "class", "const", "continue", "decimal", "default", "delegate", "do", "double", "else",
    "enum", "event", "explicit", "extern", "false", "finally", "fixed", "float", "for",
    "foreach", "goto", "if", "implicit", "in", "int", "interface", "internal", "is", "lock",
    "long", "namespace", "new", "null", "object", "operator", "out", "override", "params",
    "private", "protected", "public", "readonly", "ref", "return", "sbyte", "sealed",
    "short", "sizeof", "stackalloc", "static", "string", "struct", "switch", "this", "throw",
    "true", "try", "typeof", "uint", "ulong", "unchecked", "unsafe", "ushort", "using",
    "virtual", "void", "volatile", "while", "yield", "record", "init", "nint", "nuint",
    "nameof", "when", "select", "where", "orderby", "group", "by", "into", "join", "equals",
    # Swift
    "associatedtype", "class", "deinit", "enum", "extension", "fileprivate", "func",
    "import", "init", "inout", "internal", "let", "open", "operator", "private", "protocol",
    "public", "rethrows", "static", "struct", "subscript", "typealias", "var", "break",
    "case", "continue", "default", "defer", "do", "else", "fallthrough", "for", "guard",
    "if", "in", "repeat", "return", "switch", "where", "while", "as", "any", "catch",
    "false", "is", "nil", "super", "self", "throw", "throws", "true", "try", "async",
    "await", "some", "mutating", "nonmutating", "convenience", "dynamic", "final",
    "indirect", "lazy", "optional", "override", "required", "unowned", "weak",
    # Shell & Bash
    "echo", "cat", "grep", "sed", "awk", "chmod", "chown", "sudo", "curl", "wget", "which",
    "export", "alias", "unalias", "source", "exec", "eval", "trap", "shift", "umask",
    "set", "unset", "test", "expr", "read", "printf", "elif", "fi", "then", "else",
    "esac", "case", "for", "while", "until", "do", "done", "in", "function", "select",
    "time", "coproc", "local", "readonly", "typeset", "declare",
    # SQL
    "select", "from", "where", "insert", "update", "delete", "join", "inner", "left",
    "right", "outer", "cross", "on", "group", "by", "order", "having", "limit", "offset",
    "create", "table", "alter", "drop", "index", "primary", "foreign", "key", "unique",
    "check", "cascade", "distinct", "count", "sum", "avg", "min", "max", "union",
    "between", "like", "null", "is", "in", "exists", "case", "when", "then", "else",
    "end", "cast", "as", "view", "trigger", "procedure", "transaction", "commit",
    "rollback", "varchar", "nvarchar", "blob", "clob", "truncate", "coalesce", "rowid",
    # Lua
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto",
    "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until",
    "while", "self", "ipairs", "pairs", "tostring", "tonumber", "pcall", "xpcall",
    # Zig
    "addrspace", "align", "allowzero", "and", "anyerror", "anyframe", "anytype", "asm",
    "async", "await", "break", "callconv", "catch", "comptime", "const", "continue",
    "defer", "else", "enum", "errdefer", "error", "export", "extern", "fn", "for",
    "if", "inline", "noalias", "noinline", "nosuspend", "opaque", "or", "orelse",
    "packed", "pub", "resume", "return", "linksection", "struct", "suspend", "switch",
    "test", "threadlocal", "try", "union", "unreachable", "usingnamespace", "var",
    "volatile", "while", "usize", "isize", "noreturn", "void",
    # Web / CSS / HTML / SVG
    "doctype", "html", "head", "body", "div", "span", "href", "src", "alt", "rel",
    "stylesheet", "tbody", "thead", "tfoot", "rgba", "hsla", "px", "em", "rem", "vh",
    "vw", "keyframes", "inherit", "initial", "unset", "none", "flex", "grid", "inline",
    "block", "margin", "padding", "border", "background", "color", "cursor", "pointer",
    "zindex", "opacity", "overflow", "justify", "align", "font", "sans", "serif",
    "monospace", "svg", "path", "stroke", "fill", "viewbox", "xmlns",
    # Dart / Scala / Elixir / Haskell
    "mixin", "typedef", "factory", "covariant", "late", "required", "sync",
    "defmodule", "defp", "defmacro", "defstruct", "defprotocol", "defimpl",
    "newtype", "deriving", "infix", "infixl", "infixr", "forall"
]
for k in programming_keywords:
    for sub in k.replace("-", "_").split("_"):
        s = sub.strip().lower()
        if s and s.isalpha():
            words.add(s)

sorted_words = sorted(list(words))
with open(out_file, "w", encoding="utf-8") as f:
    for w in sorted_words:
        f.write(w + "\n")

print(f"Generated {len(sorted_words)} words in {out_file}")
