//! Standalone web search for the context MCP, mirroring the Codex CLI's
//! web-search extension (`codex-rs/ext/web-search` backed by `codex-api`'s
//! `SearchClient`): search commands are posted to the provider-relative
//! `alpha/search` endpoint, authenticated with the saved Codex CLI credentials
//! from `$CODEX_HOME/auth.json`. The tool is only advertised while credentials
//! can be found, so unavailable states keep it out of the model's context.
//!
//! Unlike the Codex extension, this server does not see the conversation, so
//! the request carries only the tool call's commands and no `input` history.

use crate::{MAX_OUTPUT_BYTES, floor_boundary};
use anyhow::{Context as _, Result, bail};
use paths::home_dir;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

const SEARCH_ENDPOINT: &str = "alpha/search";
const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const OPENAI_API_BASE_URL: &str = "https://api.openai.com/v1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_ERROR_BYTES: usize = 2 * 1024;
const DEFAULT_MODEL: &str = "gpt-5.6-terra";
/// The Codex backend identifies client builds via this header; the search
/// endpoint is part of the Codex subscription service, so requests borrowing
/// Codex credentials use the Codex CLI's originator.
const ORIGINATOR: &str = "codex_cli_rs";

#[derive(Clone, Debug)]
enum Credentials {
    ChatGpt {
        access_token: String,
        account_id: Option<String>,
    },
    ApiKey {
        key: String,
    },
}

#[derive(Deserialize)]
struct AuthFile {
    #[serde(default, rename = "OPENAI_API_KEY")]
    api_key: Option<String>,
    #[serde(default)]
    tokens: Option<AuthTokens>,
}

#[derive(Deserialize)]
struct AuthTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct SearchResponse {
    output: String,
}

/// Per-process search session. `session_id` corresponds to the `SearchRequest`
/// `id` field, which keys the backend's stored page state so `open`, `click`,
/// and `find` commands can reference earlier results across tool calls.
pub struct Session {
    session_id: String,
    auth_home: Option<PathBuf>,
    base_url_override: Option<String>,
}

impl Session {
    pub fn from_env() -> Self {
        let base_url_override = std::env::var("ZEDSTORM_WEB_SEARCH_BASE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty());
        Self {
            session_id: uuid::Uuid::new_v4().to_string(),
            auth_home: None,
            base_url_override,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_overrides(
        session_id: impl Into<String>,
        auth_home: Option<PathBuf>,
        base_url_override: Option<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            auth_home,
            base_url_override,
        }
    }

    pub fn is_available(&self) -> bool {
        self.credentials().is_ok_and(|credentials| credentials.is_some())
    }

    fn credentials(&self) -> Result<Option<Credentials>> {
        let Some(file) = codex_file(self.auth_home.as_deref(), "auth.json") else {
            return Ok(None);
        };
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(env_credentials());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("Cannot read {}", file.display()));
            }
        };
        let auth: AuthFile = serde_json::from_str(&text)
            .with_context(|| format!("Cannot parse {}", file.display()))?;
        Ok(credentials_from(&auth).or_else(env_credentials))
    }

    fn resolve_base_url(&self, credentials: &Credentials) -> String {
        if let Some(url) = &self.base_url_override {
            return url.trim_end_matches('/').to_owned();
        }
        match credentials {
            Credentials::ChatGpt { .. } => CHATGPT_CODEX_BASE_URL.to_owned(),
            Credentials::ApiKey { .. } => std::env::var("OPENAI_BASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| OPENAI_API_BASE_URL.to_owned()),
        }
    }

    /// The Codex CLI records its selected model in `$CODEX_HOME/config.toml`;
    /// the search service composes its answer for that model unless the caller
    /// overrides it.
    fn configured_model(&self) -> Option<String> {
        let file = codex_file(self.auth_home.as_deref(), "config.toml")?;
        let text = std::fs::read_to_string(&file).ok()?;
        let table: ::toml::Value = ::toml::from_str(&text).ok()?;
        table
            .get("model")?
            .as_str()
            .filter(|model| !model.trim().is_empty())
            .map(str::to_owned)
    }

    pub fn search(&self, arguments: &Value) -> Result<String> {
        let inputs = parse_arguments(arguments)?;
        let Some(credentials) = self.credentials()? else {
            bail!(
                "web_search is unavailable: no Codex credentials; sign in with \
                 `codex login` or from the Codex panel and retry"
            );
        };
        let model = inputs
            .model
            .or_else(|| {
                std::env::var("ZEDSTORM_WEB_SEARCH_MODEL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .or_else(|| self.configured_model())
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        let url = format!(
            "{}/{}",
            self.resolve_base_url(&credentials).trim_end_matches('/'),
            SEARCH_ENDPOINT
        );
        let body = json!({
            "id": self.session_id,
            "model": model,
            "commands": serde_json::to_value(&inputs.commands)?,
        });
        let (status, text) = self.post(&url, &credentials, &body)?;
        match status {
            200..=299 => (),
            401 | 403 => bail!(
                "web_search returned {status}; the Codex credentials may be \
                 expired, run `codex login` and retry: {}",
                bounded_text(&text)
            ),
            _ => bail!("web_search returned {status}: {}", bounded_text(&text)),
        }
        let response: SearchResponse = serde_json::from_str(&text).with_context(|| {
            format!("Cannot parse web search response: {}", bounded_text(&text))
        })?;
        if response.output.is_empty() {
            return Ok("The web search returned no output.".to_owned());
        }
        let bounded = floor_boundary(&response.output, MAX_OUTPUT_BYTES);
        if bounded < response.output.len() {
            return Ok(format!(
                "{}\n\n[output truncated]",
                &response.output[..bounded]
            ));
        }
        Ok(response.output)
    }

    fn post(&self, url: &str, credentials: &Credentials, body: &Value) -> Result<(u16, String)> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("Cannot create HTTP runtime")?
            .block_on(async {
                let client = reqwest::Client::builder()
                    .timeout(REQUEST_TIMEOUT)
                    .build()
                    .context("Cannot create HTTP client")?;
                let body = serde_json::to_vec(body).context("Cannot encode web_search request")?;
                let mut request = client
                    .post(url)
                    .header("originator", ORIGINATOR)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body);
                match credentials {
                    Credentials::ChatGpt {
                        access_token,
                        account_id,
                    } => {
                        request = request.bearer_auth(access_token);
                        if let Some(account_id) = account_id {
                            request = request.header("chatgpt-account-id", account_id);
                        }
                    }
                    Credentials::ApiKey { key } => {
                        request = request.bearer_auth(key);
                    }
                }
                let response = request
                    .send()
                    .await
                    .context("web_search request failed")?;
                let status = response.status().as_u16();
                let text = response
                    .text()
                    .await
                    .context("Cannot read web_search response")?;
                anyhow::Ok((status, text))
            })
    }
}

/// Tool definition matching the Codex `web.run` command set, condensed for
/// MCP tool listings.
pub fn tool_definition() -> Value {
    let query = json!({
        "type": "object",
        "properties": {
            "q": {"type": "string"},
            "recency": {"type": "integer", "description": "Only include results from the last N days"},
            "domains": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["q"],
        "additionalProperties": false
    });
    json!({
        "name": "web_search",
        "description": "Search the internet using the Codex web-search backend and this machine's Codex credentials. Pass 'query' (a string, or up to 4 strings to search in parallel); reference ids in results (e.g. turn0search0) can be used in later open/click/find calls. Returns the backend's composed answer text with URLs; cite returned URLs in your reply.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "oneOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}],
                    "description": "Search query text, or up to 4 queries searched in parallel; equivalent to search_query entries"
                },
                "model": {"type": "string", "description": "Model the backend composes the answer with; defaults to the Codex CLI's configured model"},
                "response_length": {"enum": ["short", "medium", "long"], "description": "Composed answer length"},
                "search_query": {"type": "array", "items": query},
                "image_query": {"type": "array", "items": query},
                "open": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "ref_id": {"type": "string", "description": "URL or a previous result's reference id, e.g. turn0search0"},
                        "lineno": {"type": "integer", "description": "Line number to position the page at"}
                    },
                    "required": ["ref_id"],
                    "additionalProperties": false
                }},
                "click": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "ref_id": {"type": "string"},
                        "id": {"type": "integer", "description": "Numbered link id to open"}
                    },
                    "required": ["ref_id", "id"],
                    "additionalProperties": false
                }},
                "find": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "ref_id": {"type": "string", "description": "URL or previously opened page reference id"},
                        "pattern": {"type": "string", "description": "Text pattern to find in the page"}
                    },
                    "required": ["ref_id", "pattern"],
                    "additionalProperties": false
                }},
                "screenshot": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "ref_id": {"type": "string"},
                        "pageno": {"type": "integer", "description": "Zero-indexed PDF page number"}
                    },
                    "required": ["ref_id", "pageno"],
                    "additionalProperties": false
                }},
                "finance": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "ticker": {"type": "string"},
                        "type": {"enum": ["equity", "fund", "crypto", "index"]},
                        "market": {"type": "string", "description": "ISO country code, 'OTC', or '' for cryptocurrency"}
                    },
                    "required": ["ticker", "type"],
                    "additionalProperties": false
                }},
                "weather": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "location": {"type": "string", "description": "'Country, Area, City'"},
                        "start": {"type": "string"},
                        "duration": {"type": "integer"}
                    },
                    "required": ["location"],
                    "additionalProperties": false
                }},
                "sports": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "fn": {"enum": ["schedule", "standings"]},
                        "league": {"enum": ["nba", "wnba", "nfl", "nhl", "mlb", "epl", "ncaamb", "ncaawb", "ipl"]},
                        "team": {"type": "string"},
                        "opponent": {"type": "string"},
                        "date_from": {"type": "string"},
                        "date_to": {"type": "string"},
                        "num_games": {"type": "integer"},
                        "locale": {"type": "string"}
                    },
                    "required": ["fn", "league"],
                    "additionalProperties": false
                }},
                "time": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "utc_offset": {"type": "string", "description": "Formatted like '+03:00'"}
                    },
                    "required": ["utc_offset"],
                    "additionalProperties": false
                }}
            },
            "required": [],
            "additionalProperties": false
        },
        "annotations": {"readOnlyHint": true, "openWorldHint": true}
        })
}

fn bounded_text(text: &str) -> String {
    let bounded = floor_boundary(text, MAX_ERROR_BYTES);
    format!("{:?}...", &text[..bounded])
}

fn codex_directory(auth_home: Option<&Path>) -> PathBuf {
    if let Some(directory) = auth_home {
        return directory.to_owned();
    }
    if let Some(directory) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(directory);
    }
    home_dir().join(".codex")
}

fn codex_file(auth_home: Option<&Path>, name: &str) -> Option<PathBuf> {
    Some(codex_directory(auth_home).join(name))
}

fn credentials_from(auth: &AuthFile) -> Option<Credentials> {
    if let Some(tokens) = &auth.tokens {
        let Some(access_token) = tokens
            .access_token
            .as_ref()
            .filter(|token| !token.trim().is_empty())
        else {
            return None;
        };
        return Some(Credentials::ChatGpt {
            access_token: access_token.clone(),
            account_id: tokens.account_id.clone(),
        });
    }
    let Some(key) = auth
        .api_key
        .as_ref()
        .filter(|key| !key.trim().is_empty())
    else {
        return None;
    };
    Some(Credentials::ApiKey { key: key.clone() })
}

fn env_credentials() -> Option<Credentials> {
    std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .map(|key| Credentials::ApiKey { key })
}

#[derive(Debug, Default)]
struct Inputs {
    commands: Commands,
    model: Option<String>,
}

fn parse_arguments(arguments: &Value) -> Result<Inputs> {
    let object = arguments.as_object().context("Arguments must be an object")?;
    let mut object: Map<String, Value> = object.clone();
    let model = match object.remove("model") {
        Some(Value::String(model)) => Some(model),
        Some(value) => bail!("'model' must be a string, got {value:?}"),
        None => None,
    };
    let mut commands = Commands::default();
    if let Some(query) = object.remove("query") {
        commands
            .search_query
            .extend(parse_query_list("query", &query)?);
    }
    if let Some(query) = object.remove("image_query") {
        commands
            .image_query
            .extend(parse_query_list("image_query", &query)?);
    }
    let extra: Commands = serde_json::from_value(Value::Object(object))
        .context("Invalid web search commands")?;
    commands.search_query.extend(extra.search_query);
    commands.image_query.extend(extra.image_query);
    commands.open.extend(extra.open);
    commands.click.extend(extra.click);
    commands.find.extend(extra.find);
    commands.screenshot.extend(extra.screenshot);
    commands.finance.extend(extra.finance);
    commands.weather.extend(extra.weather);
    commands.sports.extend(extra.sports);
    commands.time.extend(extra.time);
    commands.response_length = commands.response_length.or(extra.response_length);
    Ok(Inputs { commands, model })
}

/// Accepts a single query string, a list of query strings, or a list of typed
/// `{q, recency?, domains?}` query objects.
fn parse_query_list(field: &str, value: &Value) -> Result<Vec<SearchQuery>> {
    match value {
        Value::String(text) => Ok(vec![SearchQuery {
            q: text.clone(),
            recency: None,
            domains: None,
        }]),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let entry = match item {
                    Value::String(text) => SearchQuery {
                        q: text.clone(),
                        recency: None,
                        domains: None,
                    },
                    _ => serde_json::from_value(item.clone())
                        .with_context(|| format!("Invalid '{field}' query at index {index}"))?,
                };
                Ok(entry)
            })
            .collect(),
        _ => bail!("'{field}' must be a string or a list of strings"),
    }
}

/// Search command set mirroring `codex-api`'s `SearchCommands`. Every command
/// is optional, matching the Codex tool's behavior for empty or accidental
/// calls.
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Commands {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    search_query: Vec<SearchQuery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    image_query: Vec<SearchQuery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    open: Vec<OpenOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    click: Vec<ClickOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    find: Vec<FindOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    screenshot: Vec<ScreenshotOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    finance: Vec<FinanceOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    weather: Vec<WeatherOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sports: Vec<SportsOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    time: Vec<TimeOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_length: Option<SearchResponseLength>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SearchQuery {
    /// Search query.
    q: String,
    /// Filter by recency, as a number of recent days.
    #[serde(skip_serializing_if = "Option::is_none")]
    recency: Option<u64>,
    /// Filter by a specific list of domains.
    #[serde(skip_serializing_if = "Option::is_none")]
    domains: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenOperation {
    /// Reference id or URL to open.
    ref_id: String,
    /// Line number to position the page at.
    #[serde(skip_serializing_if = "Option::is_none")]
    lineno: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ClickOperation {
    /// Reference id containing the numbered link.
    ref_id: String,
    /// Numbered link id to open.
    id: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct FindOperation {
    /// Reference id or URL to search within.
    ref_id: String,
    /// Text pattern to find.
    pattern: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ScreenshotOperation {
    /// Reference id or URL to screenshot.
    ref_id: String,
    /// Zero-indexed PDF page number.
    pageno: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct FinanceOperation {
    /// Ticker symbol to look up.
    ticker: String,
    /// Asset type to look up.
    r#type: FinanceAssetType,
    /// ISO 3166-1 alpha-3 country code, "OTC", or "" for cryptocurrency.
    #[serde(skip_serializing_if = "Option::is_none")]
    market: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FinanceAssetType {
    Equity,
    Fund,
    Crypto,
    Index,
}

#[derive(Debug, Serialize, Deserialize)]
struct WeatherOperation {
    /// Location in "Country, Area, City" format.
    location: String,
    /// Start date in YYYY-MM-DD format. Defaults to today.
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<String>,
    /// Number of days to return. Defaults to 7.
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SportsOperation {
    /// Sports function to call.
    r#fn: SportsFunction,
    /// League to look up.
    league: SportsLeague,
    /// Team to look up, using the common 3 or 4 letter alias used in broadcasts.
    #[serde(skip_serializing_if = "Option::is_none")]
    team: Option<String>,
    /// Opponent to use with `team` when narrowing the lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    opponent: Option<String>,
    /// Start date in YYYY-MM-DD format.
    #[serde(skip_serializing_if = "Option::is_none")]
    date_from: Option<String>,
    /// End date in YYYY-MM-DD format.
    #[serde(skip_serializing_if = "Option::is_none")]
    date_to: Option<String>,
    /// Number of games to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    num_games: Option<u64>,
    /// Locale for the lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    locale: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SportsFunction {
    Schedule,
    Standings,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SportsLeague {
    Nba,
    Wnba,
    Nfl,
    Nhl,
    Mlb,
    Epl,
    Ncaamb,
    Ncaawb,
    Ipl,
}

#[derive(Debug, Serialize, Deserialize)]
struct TimeOperation {
    /// UTC offset formatted like "+03:00".
    utc_offset: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SearchResponseLength {
    Short,
    Medium,
    Long,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[derive(Default)]
    struct Captured {
        path: Option<String>,
        headers: HashMap<String, String>,
        body: Option<Value>,
    }

    /// Serves a single JSON response and captures the incoming request.
    fn serve_json(response: Value) -> (String, Arc<Mutex<Option<Captured>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("local address");
        let captured = Arc::new(Mutex::new(None));
        let handle = captured.clone();
        let response = serde_json::to_string(&response).expect("serialize response");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept one request");
            let mut buffer = Vec::new();
            let mut reader = BufReader::new(&stream);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("read request line");
                if line.trim().is_empty() {
                    break;
                }
                buffer.extend_from_slice(line.as_bytes());
            }
            let mut request = buffer.split(|byte| *byte == b'\n');
            let first_line = request
                .next()
                .map(|line| String::from_utf8_lossy(line).into_owned())
                .unwrap_or_default();
            let path = first_line.split_whitespace().nth(1).map(str::to_owned);
            let mut headers = HashMap::new();
            for line in request {
                let line = String::from_utf8_lossy(line).into_owned();
                if let Some((name, value)) = line.split_once(':') {
                    headers.insert(name.to_lowercase().trim().to_string(), value.trim().to_string());
                }
            }
            let length: usize = headers
                .get("content-length")
                .and_then(|value| value.parse().ok())
                .unwrap_or_default();
            let mut body = vec![0; length];
            reader.read_exact(&mut body).expect("read body");
            let body = serde_json::from_slice(&body).ok();
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            );
            stream.write_all(http.as_bytes()).expect("write response");
            *handle.lock().expect("lock capture") = Some(Captured { path, headers, body });
        });
        (format!("http://{address}"), captured)
    }

    fn write_auth_file(home: &Path, value: Value) -> Result<()> {
        let file = codex_file(Some(home), "auth.json").expect("auth file path");
        std::fs::write(file, serde_json::to_string(&value)?)
            .context("Cannot write test auth.json")
    }

    #[test]
    fn hidden_without_codex_credentials() -> Result<()> {
        let home = tempfile::tempdir()?;
        let session = Session::with_overrides("session", Some(home.path().to_owned()), None);
        assert!(!session.is_available());
        let search = session.search(&json!({"query": "rust"}));
        assert_eq!(
            search.expect_err("expected error").to_string(),
            "web_search is unavailable: no Codex credentials; sign in with `codex login` or from the Codex panel and retry"
        );
        Ok(())
    }

    #[test]
    fn chatgpt_credentials_send_bearer_and_post_commands() -> Result<()> {
        let (server_url, captured) = serve_json(json!({
            "encrypted_output": null,
            "output": "The capital is Paris.",
            "results": [],
        }));
        let home = tempfile::tempdir()?;
        write_auth_file(
            home.path(),
            json!({
                "OPENAI_API_KEY": null,
                "tokens": {
                    "id_token": "jwt",
                    "access_token": "test-access",
                    "account_id": "test-account",
                },
            }),
        )?;
        let session =
            Session::with_overrides("session-1", Some(home.path().to_owned()), Some(server_url));
        assert!(session.is_available());
        let output = session.search(&json!({
            "query": ["What is the capital of France?"],
            "model": "gpt-test",
        }))?;
        assert_eq!(output, "The capital is Paris.");
        let request = captured
            .lock()
            .expect("lock capture")
            .take()
            .expect("captured request");
        assert_eq!(request.path.as_deref(), Some("/alpha/search"));
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer test-access")
        );
        assert_eq!(
            request
                .headers
                .get("chatgpt-account-id")
                .map(String::as_str),
            Some("test-account")
        );
        assert_eq!(
            request.headers.get("originator").map(String::as_str),
            Some("codex_cli_rs")
        );
        let body = request.body.expect("captured body");
        assert_eq!(body["id"], "session-1");
        assert_eq!(body["model"], "gpt-test");
        assert_eq!(
            body["commands"]["search_query"][0]["q"],
            "What is the capital of France?"
        );
        // `input` is conversation history, which this server never sees.
        assert!(body.get("input").is_none());
        Ok(())
    }

    #[test]
    fn api_key_credentials_send_bearer() -> Result<()> {
        let (server_url, captured) = serve_json(json!({"output": "answer"}));
        let home = tempfile::tempdir()?;
        write_auth_file(home.path(), json!({"OPENAI_API_KEY": "sk-test"}))?;
        let session =
            Session::with_overrides("session-2", Some(home.path().to_owned()), Some(server_url));
        assert!(session.is_available());
        let output = session.search(&json!({"query": "open"}))?;
        assert_eq!(output, "answer");
        let request = captured
            .lock()
            .expect("lock capture")
            .take()
            .expect("captured request");
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer sk-test")
        );
        assert!(
            !request.headers.contains_key("chatgpt-account-id")
        );
        Ok(())
    }
}
