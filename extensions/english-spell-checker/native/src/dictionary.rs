use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const WORDS_TXT: &str = include_str!("words.txt");

static DICTIONARY: OnceLock<HashSet<&'static str>> = OnceLock::new();
static COMMON_TYPOS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

pub struct Dictionary {
    custom_words: HashSet<String>,
    ignored_words: HashSet<String>,
}

impl Default for Dictionary {
    fn default() -> Self {
        Self::new()
    }
}

impl Dictionary {
    pub fn new() -> Self {
        DICTIONARY.get_or_init(|| {
            let mut set = HashSet::with_capacity(30000);
            for line in WORDS_TXT.lines() {
                let w = line.trim();
                if !w.is_empty() {
                    set.insert(w);
                }
            }
            for kw in PROGRAMMING_KEYWORDS {
                set.insert(*kw);
            }
            set
        });

        COMMON_TYPOS.get_or_init(|| {
            let pairs: &[(&'static str, &'static str)] = &[
                ("dose", "does"),
                ("teh", "the"),
                ("recieve", "receive"),
                ("recieved", "received"),
                ("recieving", "receiving"),
                ("seperate", "separate"),
                ("seperated", "separated"),
                ("seperates", "separates"),
                ("lenght", "length"),
                ("widht", "width"),
                ("heigth", "height"),
                ("calback", "callback"),
                ("calbacks", "callbacks"),
                ("extention", "extension"),
                ("extentions", "extensions"),
                ("funtion", "function"),
                ("funtions", "functions"),
                ("functon", "function"),
                ("functons", "functions"),
                ("paramater", "parameter"),
                ("paramaters", "parameters"),
                ("wierd", "weird"),
                ("untill", "until"),
                ("alot", "a lot"),
                ("comit", "commit"),
                ("comited", "committed"),
                ("comiting", "committing"),
                ("occured", "occurred"),
                ("occuring", "occurring"),
                ("definately", "definitely"),
                ("truely", "truly"),
                ("goverment", "government"),
                ("enviroment", "environment"),
                ("neccessary", "necessary"),
                ("writting", "writing"),
                ("begining", "beginning"),
                ("succesful", "successful"),
                ("arguement", "argument"),
                ("arguements", "arguments"),
                ("tommorow", "tomorrow"),
                ("tommorrow", "tomorrow"),
                ("existance", "existence"),
                ("accomodate", "accommodate"),
                ("acheive", "achieve"),
                ("adress", "address"),
                ("adresses", "addresses"),
                ("allign", "align"),
                ("alligned", "aligned"),
                ("allready", "already"),
                ("apparant", "apparent"),
                ("appearence", "appearance"),
                ("availabe", "available"),
                ("beacuse", "because"),
                ("beleive", "believe"),
                ("booelean", "boolean"),
                ("boundry", "boundary"),
                ("breif", "brief"),
                ("charater", "character"),
                ("charaters", "characters"),
                ("colum", "column"),
                ("colums", "columns"),
                ("comming", "coming"),
                ("compatability", "compatibility"),
                ("completly", "completely"),
                ("connet", "connect"),
                ("connetion", "connection"),
                ("currunt", "current"),
                ("databse", "database"),
                ("debuging", "debugging"),
                ("decission", "decision"),
                ("defaut", "default"),
                ("defintion", "definition"),
                ("dependancy", "dependency"),
                ("descripiton", "description"),
                ("destory", "destroy"),
                ("diffrent", "different"),
                ("documantation", "documentation"),
                ("elemnt", "element"),
                ("elemnts", "elements"),
                ("embarass", "embarrass"),
                ("evant", "event"),
                ("familar", "familiar"),
                ("foward", "forward"),
                ("gurantee", "guarantee"),
                ("happend", "happened"),
                ("heirarchy", "hierarchy"),
                ("impliment", "implement"),
                ("implimented", "implemented"),
                ("independant", "independent"),
                ("inital", "initial"),
                ("initalize", "initialize"),
                ("initalized", "initialized"),
                ("interupt", "interrupt"),
                ("libary", "library"),
                ("lisence", "license"),
                ("maintainance", "maintenance"),
                ("managment", "management"),
                ("mesage", "message"),
                ("mesages", "messages"),
                ("minumum", "minimum"),
                ("mispell", "misspell"),
                ("mispelled", "misspelled"),
                ("noticable", "noticeable"),
                ("oppurtunity", "opportunity"),
                ("optmize", "optimize"),
                ("packge", "package"),
                ("peice", "piece"),
                ("performace", "performance"),
                ("permisson", "permission"),
                ("platfrom", "platform"),
                ("prefered", "preferred"),
                ("priviledge", "privilege"),
                ("proceedure", "procedure"),
                ("publically", "publicly"),
                ("reduntant", "redundant"),
                ("refrence", "reference"),
                ("regester", "register"),
                ("relavent", "relevant"),
                ("relese", "release"),
                ("reponsive", "responsive"),
                ("requirment", "requirement"),
                ("requirments", "requirements"),
                ("responce", "response"),
                ("respository", "repository"),
                ("retreive", "retrieve"),
                ("similiar", "similar"),
                ("structur", "structure"),
                ("suprise", "surprise"),
                ("synchonous", "synchronous"),
                ("thier", "their"),
                ("threshhold", "threshold"),
                ("transation", "transaction"),
                ("unfortunatly", "unfortunately"),
                ("varible", "variable"),
                ("varibles", "variables"),
                ("verfiy", "verify"),
                ("visiblity", "visibility"),
                ("wich", "which"),
                ("yeild", "yield"),
                ("defualt", "default"),
                ("strcut", "struct"),
                ("sturct", "struct"),
                ("cosnt", "const"),
                ("cosntexpr", "constexpr"),
                ("nullprt", "nullptr"),
                ("nullpter", "nullptr"),
                ("retrun", "return"),
                ("retun", "return"),
                ("breka", "break"),
                ("contunue", "continue"),
                ("contineu", "continue"),
                ("pubilc", "public"),
                ("statci", "static"),
                ("asyn", "async"),
                ("awiat", "await"),
                ("improt", "import"),
                ("exprot", "export"),
                ("tyepof", "typeof"),
                ("isntanceof", "instanceof"),
                ("eliff", "elif"),
                ("deffer", "defer"),
                ("deffered", "deferred"),
                ("elsiff", "elsif"),
                ("prntln", "println"),
                ("printl", "println"),
            ];
            let mut map = HashMap::with_capacity(pairs.len());
            for (typo, fix) in pairs {
                map.insert(*typo, *fix);
            }
            map
        });

        Self {
            custom_words: HashSet::new(),
            ignored_words: HashSet::new(),
        }
    }

    #[allow(dead_code)]
    pub fn add_custom_word(&mut self, word: &str) {
        self.custom_words.insert(word.to_ascii_lowercase());
    }

    #[allow(dead_code)]
    pub fn ignore_word(&mut self, word: &str) {
        self.ignored_words.insert(word.to_ascii_lowercase());
    }

    pub fn is_valid(&self, word: &str) -> bool {
        let lower = word.to_ascii_lowercase();
        if self.custom_words.contains(&lower) || self.ignored_words.contains(&lower) {
            return true;
        }

        // Known typos take precedence and are always marked invalid
        if let Some(typos) = COMMON_TYPOS.get() {
            if typos.contains_key(lower.as_str()) {
                return false;
            }
        }

        // Check base dictionary
        let dict = DICTIONARY.get().unwrap();
        if dict.contains(lower.as_str()) {
            return true;
        }

        // Possessive apostrophe (user's -> user)
        if let Some(stem) = lower.strip_suffix("'s") {
            if dict.contains(stem) || self.custom_words.contains(stem) {
                return true;
            }
        }

        // Inflection stemming
        let len = lower.len();
        if len >= 4 && lower.ends_with('s') {
            let stem = &lower[..len - 1];
            if dict.contains(stem) {
                return true;
            }
            if len >= 5 && lower.ends_with("es") {
                let stem2 = &lower[..len - 2];
                if dict.contains(stem2) {
                    return true;
                }
            }
            if len >= 6 && lower.ends_with("ies") {
                let mut stem_y = lower[..len - 3].to_string();
                stem_y.push('y');
                if dict.contains(stem_y.as_str()) {
                    return true;
                }
            }
        }

        if len >= 4 && lower.ends_with("ed") {
            let stem = &lower[..len - 2];
            if dict.contains(stem) {
                return true;
            }
            let stem_d = &lower[..len - 1]; // e.g. create -> created
            if dict.contains(stem_d) {
                return true;
            }
            if len >= 5 && lower.ends_with("ied") {
                let mut stem_y = lower[..len - 3].to_string();
                stem_y.push('y');
                if dict.contains(stem_y.as_str()) {
                    return true;
                }
            }
        }

        if len >= 5 && lower.ends_with("ing") {
            let stem = &lower[..len - 3];
            if dict.contains(stem) {
                return true;
            }
            // e.g. make -> making
            let mut stem_e = lower[..len - 3].to_string();
            stem_e.push('e');
            if dict.contains(stem_e.as_str()) {
                return true;
            }
            // e.g. running -> run
            if stem.len() >= 3 {
                let bytes = stem.as_bytes();
                if bytes[bytes.len() - 1] == bytes[bytes.len() - 2] {
                    let un_doubled = &stem[..stem.len() - 1];
                    if dict.contains(un_doubled) {
                        return true;
                    }
                }
            }
        }

        if len >= 5 && lower.ends_with("ly") {
            let stem = &lower[..len - 2];
            if dict.contains(stem) {
                return true;
            }
        }

        if len >= 4 && lower.ends_with("er") {
            let stem = &lower[..len - 2];
            if dict.contains(stem) {
                return true;
            }
            let stem_r = &lower[..len - 1];
            if dict.contains(stem_r) {
                return true;
            }
        }

        if len >= 6 && lower.ends_with("able") {
            let stem = &lower[..len - 4];
            if dict.contains(stem) {
                return true;
            }
            let mut stem_e = lower[..len - 4].to_string();
            stem_e.push('e');
            if dict.contains(stem_e.as_str()) {
                return true;
            }
        }

        // Prefixes
        for prefix in &["un", "re", "dis", "non", "pre", "mis"] {
            if lower.starts_with(prefix) && len > prefix.len() + 2 {
                let stem = &lower[prefix.len()..];
                if dict.contains(stem) {
                    return true;
                }
            }
        }

        false
    }

    pub fn suggest(&self, word: &str) -> Vec<String> {
        let lower = word.to_ascii_lowercase();
        let is_capitalized = word.chars().next().is_some_and(|c| c.is_uppercase());
        let mut results = Vec::new();

        // 1. Check known typo map
        if let Some(typos) = COMMON_TYPOS.get() {
            if let Some(fix) = typos.get(lower.as_str()) {
                results.push(apply_casing(fix, is_capitalized));
            }
        }

        // 2. Search base dictionary using Damerau-Levenshtein distance <= 2
        let dict = DICTIONARY.get().unwrap();
        let mut candidates: Vec<(&'static str, usize)> = Vec::new();

        for candidate in dict.iter() {
            let diff = candidate.len().abs_diff(lower.len());
            if diff > 2 {
                continue;
            }

            let share_first = candidate.as_bytes().first() == lower.as_bytes().first();
            let dist = damerau_levenshtein(&lower, candidate);
            if dist <= 2 {
                let score = dist * 2 + if share_first { 0 } else { 1 };
                candidates.push((candidate, score));
            }
        }

        candidates.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.len().abs_diff(lower.len()).cmp(&b.0.len().abs_diff(lower.len()))));

        for (cand, _) in candidates {
            let formatted = apply_casing(cand, is_capitalized);
            if !results.contains(&formatted) {
                results.push(formatted);
                if results.len() >= 5 {
                    break;
                }
            }
        }

        results
    }
}

fn apply_casing(word: &str, capitalize: bool) -> String {
    if !capitalize {
        return word.to_string();
    }
    let mut chars = word.chars();
    match chars.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

pub fn damerau_levenshtein(s1: &str, s2: &str) -> usize {
    let b1 = s1.as_bytes();
    let b2 = s2.as_bytes();
    let n = b1.len();
    let m = b2.len();

    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut d = vec![vec![0usize; m + 1]; n + 1];

    for i in 0..=n {
        d[i][0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }

    for i in 1..=n {
        for j in 1..=m {
            let cost = if b1[i - 1] == b2[j - 1] { 0 } else { 1 };
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);

            if i > 1 && j > 1 && b1[i - 1] == b2[j - 2] && b1[i - 2] == b2[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }

    d[n][m]
}

pub const PROGRAMMING_KEYWORDS: &[&str] = &[
    "abort", "abstract", "accessor", "addrspace", "alias", "align", "alignas", "alignof",
    "allow", "allowzero", "and", "any", "anyerror", "anyframe", "anytype", "append",
    "argc", "args", "arguments", "argv", "array", "as", "asm", "assert", "associatedtype",
    "async", "atomic", "atof", "atoi", "atol", "attr", "auto", "await", "awk", "base",
    "become", "begin", "bigint", "bitand", "bitor", "blob", "bool", "boolean", "box",
    "boxed", "break", "bsearch", "byte", "bytearray", "bytes", "callconv", "callable",
    "calloc", "case", "cast", "catch", "cerr", "cfg", "chan", "channel", "char",
    "checked", "chmod", "chown", "cin", "class", "classmethod", "clear", "clob", "clog",
    "clone", "close", "cls", "coalesce", "companion", "comparable", "compl", "complex",
    "comptime", "concept", "const", "consteval", "constexpr", "constinit", "constructor",
    "continue", "convenience", "coproc", "coroutine", "cout", "covariant", "crate",
    "crossinline", "data", "dbg", "decimal", "declare", "decltype", "defer", "defined",
    "deinit", "del", "delattr", "delegate", "delete", "delitem", "deny", "deque", "derive",
    "deriving", "dict", "die", "distinct", "div", "do", "doc", "doctype", "double",
    "dunder", "dynamic", "dyn", "echo", "elif", "else", "elseif", "elsif", "empty",
    "end", "endl", "endfor", "endforeach", "endif", "endwhile", "ensure", "enum",
    "enumerate", "eprint", "eprintln", "errdefer", "error", "esac", "eval", "event",
    "except", "exec", "exit", "explicit", "export", "expr", "extend", "extends", "extension",
    "extern", "factory", "fallthrough", "false", "fileprivate", "filter", "final", "finally",
    "fixed", "float", "fn", "for", "forbid", "foreach", "foreign", "format", "from",
    "func", "function", "fun", "get", "getattr", "getitem", "global", "goto", "goroutine",
    "guard", "hasattr", "having", "head", "href", "hsla", "html", "ifdef", "ifndef",
    "imag", "implicit", "implements", "impl", "import", "in", "include", "indirect",
    "infer", "infix", "infixl", "infixr", "inherit", "init", "initial", "inline", "inout",
    "input", "insert", "instanceof", "int", "interface", "internal", "intmax", "intptr",
    "iota", "ipairs", "is", "isinstance", "isize", "isset", "issubclass", "it", "iter",
    "join", "keyframes", "keyof", "kwargs", "lambda", "late", "lazy", "len", "let",
    "limit", "linksection", "list", "local", "lock", "long", "loop", "macro", "make",
    "malloc", "map", "match", "max", "memcmp", "memcpy", "memmove", "memset", "min",
    "mixin", "mod", "module", "monospace", "move", "mutable", "mutating", "mut", "nameof",
    "namespace", "nan", "native", "never", "new", "newtype", "next", "nil", "nint",
    "noalias", "noexcept", "noinline", "none", "nonlocal", "nonmutating", "nonnull",
    "noreturn", "nosuspend", "not", "nuint", "null", "nullable", "nullptr",
    "number", "nvarchar", "object", "offset", "open", "operator", "optional", "order",
    "orelse", "out", "outer", "override", "package", "pair", "pairs", "panic", "params",
    "pass", "pcall", "permits", "pkg", "pointer", "pragma", "prepend", "primary", "print",
    "printf", "println", "priv", "proc", "procedure", "promiselike", "promisify", "property",
    "protected", "protocol", "ptrdiff", "public", "pub", "puts", "qsort", "raise", "range",
    "readonly", "real", "realloc", "receive", "record", "recover", "redo", "ref", "refcell",
    "reflexpr", "reified", "reinterpret", "rel", "rem", "repeat", "repr", "require",
    "rescue", "restrict", "rethrows", "retry", "return", "rgba", "rollback", "rowid",
    "rune", "rwlock", "safe", "satisfies", "sbyte", "scanf", "sealed", "select", "self",
    "set", "setattr", "setitem", "shift", "short", "show", "signed", "sizeof", "snprintf",
    "some", "source", "sprintf", "sql", "sscanf", "ssize", "stackalloc", "static",
    "staticmethod", "std", "stdin", "stdout", "stderr", "str", "strcat", "strchr",
    "strcmp", "strcpy", "strdup", "stream", "strictfp", "string", "stringify", "strlen",
    "strncat", "strncmp", "strncpy", "strstr", "strtok", "strtol", "strtoul", "struct",
    "subscript", "sudo", "super", "suspend", "switch", "symbol", "sync", "synchronized",
    "tbody", "template", "test", "text", "tfoot", "thead", "then", "this", "thread",
    "threadlocal", "throw", "throws", "time", "todo", "tonumber", "tostring", "trait",
    "transaction", "transient", "trap", "true", "truncate", "try", "type", "typealias",
    "typedef", "typeid", "typename", "typeof", "typeset", "uint", "uintmax", "uintptr",
    "ulong", "umask", "unalias", "unary", "unchecked", "undefined", "undef", "unimplemented",
    "union", "unique", "unit", "unknown", "unowned", "unreachable", "unsafe", "unset",
    "unsigned", "unsized", "until", "update", "use", "using", "usingnamespace", "usize",
    "val", "var", "vararg", "varchar", "vec", "vector", "view", "viewbox", "virtual",
    "void", "volatile", "warn", "wchar", "weak", "wget", "when", "where", "while",
    "with", "writeln", "write", "xmlns", "xor", "xpcall", "yaml", "yield", "zig", "zindex",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_words() {
        let dict = Dictionary::new();
        assert!(dict.is_valid("hello"));
        assert!(dict.is_valid("world"));
        assert!(dict.is_valid("checking"));
        assert!(dict.is_valid("spelling"));
        assert!(dict.is_valid("don't"));
        assert!(dict.is_valid("extension"));
    }

    #[test]
    fn test_programming_keywords_are_valid() {
        let dict = Dictionary::new();
        // Rust
        assert!(dict.is_valid("usize"));
        assert!(dict.is_valid("isize"));
        assert!(dict.is_valid("dyn"));
        assert!(dict.is_valid("println"));
        assert!(dict.is_valid("eprintln"));
        assert!(dict.is_valid("dbg"));
        assert!(dict.is_valid("unreachable"));
        assert!(dict.is_valid("unimplemented"));

        // Python
        assert!(dict.is_valid("elif"));
        assert!(dict.is_valid("nonlocal"));
        assert!(dict.is_valid("kwargs"));
        assert!(dict.is_valid("repr"));
        assert!(dict.is_valid("isinstance"));
        assert!(dict.is_valid("issubclass"));
        assert!(dict.is_valid("hasattr"));
        assert!(dict.is_valid("getattr"));

        // C / C++
        assert!(dict.is_valid("constexpr"));
        assert!(dict.is_valid("nullptr"));
        assert!(dict.is_valid("decltype"));
        assert!(dict.is_valid("noexcept"));
        assert!(dict.is_valid("typename"));
        assert!(dict.is_valid("typeid"));
        assert!(dict.is_valid("alignas"));
        assert!(dict.is_valid("alignof"));
        assert!(dict.is_valid("ptrdiff"));
        assert!(dict.is_valid("uintptr"));

        // Go
        assert!(dict.is_valid("defer"));
        assert!(dict.is_valid("fallthrough"));
        assert!(dict.is_valid("iota"));
        assert!(dict.is_valid("goroutine"));
        assert!(dict.is_valid("rune"));

        // JavaScript / TypeScript
        assert!(dict.is_valid("typeof"));
        assert!(dict.is_valid("instanceof"));
        assert!(dict.is_valid("keyof"));
        assert!(dict.is_valid("bigint"));

        // Ruby / PHP / Shell
        assert!(dict.is_valid("elsif"));
        assert!(dict.is_valid("elseif"));
        assert!(dict.is_valid("chmod"));
        assert!(dict.is_valid("sudo"));
    }

    #[test]
    fn test_common_typos_and_suggestions() {
        let dict = Dictionary::new();
        assert!(!dict.is_valid("teh"));
        let sug_teh = dict.suggest("teh");
        assert_eq!(sug_teh.first().unwrap(), "the");

        let sug_dose = dict.suggest("dose");
        assert_eq!(sug_dose.first().unwrap(), "does");

        let sug_extention = dict.suggest("extention");
        assert_eq!(sug_extention.first().unwrap(), "extension");

        // Keyword typo suggestions
        let sug_constexpr = dict.suggest("cosntexpr");
        assert_eq!(sug_constexpr.first().unwrap(), "constexpr");

        let sug_nullptr = dict.suggest("nullprt");
        assert_eq!(sug_nullptr.first().unwrap(), "nullptr");

        let sug_elif = dict.suggest("eliff");
        assert_eq!(sug_elif.first().unwrap(), "elif");

        let sug_typeof = dict.suggest("tyepof");
        assert_eq!(sug_typeof.first().unwrap(), "typeof");

        let sug_defer = dict.suggest("deffer");
        assert_eq!(sug_defer.first().unwrap(), "defer");
    }
}
