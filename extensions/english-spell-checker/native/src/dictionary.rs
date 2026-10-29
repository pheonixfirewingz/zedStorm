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
            let mut set = HashSet::with_capacity(25000);
            for line in WORDS_TXT.lines() {
                let w = line.trim();
                if !w.is_empty() {
                    set.insert(w);
                }
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
    fn test_common_typos_and_suggestions() {
        let dict = Dictionary::new();
        assert!(!dict.is_valid("teh"));
        let sug_teh = dict.suggest("teh");
        assert_eq!(sug_teh.first().unwrap(), "the");

        let sug_dose = dict.suggest("dose");
        assert_eq!(sug_dose.first().unwrap(), "does");

        let sug_extention = dict.suggest("extention");
        assert_eq!(sug_extention.first().unwrap(), "extension");
    }
}
