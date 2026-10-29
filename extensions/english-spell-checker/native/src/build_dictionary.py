import sys
from pathlib import Path

script_dir = Path("extensions/english-spell-checker/native/src")
raw_20k = script_dir / "google-20k.txt"

words = set()

if raw_20k.exists():
    with open(raw_20k, "r", encoding="utf-8") as f:
        for line in f:
            w = line.strip().lower()
            if w and w.isalpha():
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

sorted_words = sorted(list(words))
out_file = script_dir / "words.txt"
with open(out_file, "w", encoding="utf-8") as f:
    for w in sorted_words:
        f.write(w + "\n")

print(f"Generated {len(sorted_words)} words in {out_file}")
