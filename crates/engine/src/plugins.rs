//! Plugins: external MCP servers whose tools become Craft Anything tools, each server its own
//! toolset. A server is listed in `config.toml`:
//!
//! ```toml
//! [[plugins.mcp]]
//! id = "notes"                 # toolset id; tool ids become notes_<tool>
//! command = "notes-mcp"
//! args = ["--stdio"]
//! consent = true               # ask before each call (default): it runs someone else's code
//!
//! [[plugins.mcp]]
//! id = "docs"                  # a remote server (Streamable HTTP)
//! url = "https://mcp.example.com/mcp"
//! headers = { Authorization = "Bearer …" }
//!
//! [[plugins.mcp]]
//! id = "rea"                   # REA (rea-agents): `default_args` fills an argument a call leaves out,
//! command = "rea"              # only for tools whose schema declares it
//! args = ["mcp"]
//! consent = false              # a build only runs plugin tools that don't ask first
//! default_args = { detail = "summary" }
//! ask_tools = ["*adb*", "*frida*"]  # these ask even with consent off (destructive tools always do)
//! ```
//!
//! Its tool list is cached (`plugins_refresh`), so starting the app or a CLI command never starts
//! every server; a server starts on its first call and stays up for the session. Results are
//! recorded as evidence from an external service. Effects come from the server's own tool
//! annotations, plus launching a process (local) or using the network (remote); `consent` adds
//! "runs foreign code", which asks first. `env` and `headers` values are never shown by
//! `settings_show` or `plugins_list`.

use crate::registry::{RuntimeTool, custom_toolset};
use crate::session::Session;
use crate::tool::{Ctx, Effects, Requires, ToolOutput};
use craft_anything_engines::mcp_client::{McpClient, RemoteTool, Server, ServerCommand, ServerUrl, content_text, url_allowed};
use craft_anything_evidence::{Authority, Confidence, Coverage, CoverageStatus, Environment, ErrorCode, EvidenceBuilder, Limitation, Subject, ToolError};
use craft_anything_platform::sync::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

/// How long a server may take to start and answer the handshake or its tool list.
pub const START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long one call may take.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Most tools one server may add.
const MAX_TOOLS: usize = 500;

/// One configured MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpPlugin {
    /// Toolset id and tool-id prefix: lower-case letters, digits and `_`.
    pub id: String,
    /// The name people see (default: the id).
    #[serde(default)]
    pub name: Option<String>,
    /// The program of a local server (or `url` for a remote one).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// A remote server's address: `https://…`, or `http://` to this computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Sent with every request to a remote server (e.g. `Authorization`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Ask before each call (it runs someone else's code). Default true.
    #[serde(default = "yes")]
    pub consent: bool,
    /// Arguments sent when a call leaves them out, for the tools whose input schema declares them
    /// (REA's `detail = "summary"` keeps its answers small enough for an agent).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub default_args: BTreeMap<String, Value>,
    /// Tools that ask before each call even when `consent` is off, by their name on the server
    /// (`*` matches any run of characters: `"*adb*"`, `"start_*"`). A tool the server marks
    /// destructive always asks. A build refuses tools that ask, so this is how a plugin trusted for
    /// reading keeps the tools that run programs or change a device out of builds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ask_tools: Vec<String>,
}

fn yes() -> bool {
    true
}

impl McpPlugin {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    /// How to reach it: a remote `url`, else the local `command`.
    pub fn server(&self) -> Server {
        match self.remote_url() {
            Some(url) => Server::Url(ServerUrl { url: url.to_string(), headers: self.headers.clone() }),
            None => Server::Command(ServerCommand { command: self.command.clone(), args: self.args.clone(), env: self.env.clone(), cwd: self.cwd.clone() }),
        }
    }

    fn remote_url(&self) -> Option<&str> {
        self.url.as_deref().map(str::trim).filter(|u| !u.is_empty())
    }

    /// What's wrong with it, if anything: its id, or not exactly one of `command` and `url`.
    pub fn problem(&self) -> Option<String> {
        if !RuntimeTool::valid_id(&self.id) || custom_toolset(&self.id).is_none() {
            return Some(format!("Plugin id {:?} must be lower-case letters, digits and _.", self.id));
        }
        match (self.command.trim().is_empty(), self.remote_url()) {
            (true, None) => Some(format!("The {} plugin needs a command (a local server) or a url (a remote one).", self.id)),
            (false, Some(_)) => Some(format!("The {} plugin has both a command and a url; keep one.", self.id)),
            (true, Some(u)) if !url_allowed(u) => Some(format!("The {} plugin's url must start with https:// (or http:// to this computer).", self.id)),
            _ => None,
        }
    }

    /// Where it runs, for people: the url, or the command line.
    pub fn place(&self) -> String {
        match self.remote_url() {
            Some(u) => u.to_string(),
            None => std::iter::once(self.command.as_str()).chain(self.args.iter().map(String::as_str)).collect::<Vec<_>>().join(" "),
        }
    }

    /// Where its tool list is cached, under a config folder ([`Session::config_dir`]).
    pub fn cache_path(&self, config: &std::path::Path) -> PathBuf {
        config.join("plugins").join(format!("{}.tools.json", self.id))
    }

    /// Where the server's own instructions (from its handshake) are kept by the last refresh.
    pub fn instructions_path(&self, config: &std::path::Path) -> PathBuf {
        config.join("plugins").join(format!("{}.instructions.md", self.id))
    }

    /// The server's instructions kept by the last refresh, if it sent any.
    pub fn cached_instructions(&self, config: &std::path::Path) -> Option<String> {
        std::fs::read_to_string(self.instructions_path(config)).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
    }

    /// The tools cached by the last refresh (none before the first).
    pub fn cached_tools(&self, config: &std::path::Path) -> Vec<RemoteTool> {
        std::fs::read(self.cache_path(config)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
}

/// `[plugins]` in config.toml.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSettings {
    pub mcp: Vec<McpPlugin>,
    /// WASM component plugins (`crate::wasm_plugins`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub wasm: Vec<crate::wasm_plugins::WasmPluginConfig>,
}

/// One proxied tool: which server, and the tool's name there.
struct Route {
    server: String,
    remote: String,
    /// The plugin's `default_args` this tool's schema declares.
    defaults: serde_json::Map<String, Value>,
}

/// The session's plugins: their settings, running clients and the tool routes.
#[derive(Default)]
pub struct Plugins {
    servers: HashMap<String, (McpPlugin, Mutex<Option<McpClient>>)>,
    routes: HashMap<&'static str, Route>,
}

/// A remote tool name as part of our id: lower case, `_` for anything else, no repeats.
fn id_part(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        let c = if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' };
        if !(c == '_' && out.ends_with('_')) {
            out.push(c);
        }
    }
    out.trim_matches('_').to_string()
}

/// Whether `name` matches a pattern where `*` stands for any run of characters (case ignored).
fn matches_pattern(pattern: &str, name: &str) -> bool {
    let (p, n) = (pattern.trim().to_ascii_lowercase(), name.to_ascii_lowercase());
    let parts: Vec<&str> = p.split('*').collect();
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else { return false };
    if parts.len() == 1 {
        return p == n;
    }
    if !n.starts_with(first) || !n.ends_with(last) || n.len() < first.len().saturating_add(last.len()) {
        return false;
    }
    let Some(mut rest) = n.get(first.len()..n.len().saturating_sub(last.len())) else { return false };
    for part in parts.iter().skip(1).take(parts.len().saturating_sub(2)) {
        match rest.find(part).and_then(|i| rest.get(i.saturating_add(part.len())..)) {
            Some(after) => rest = after,
            None => return false,
        }
    }
    true
}

/// Whether a plugin's tool asks before each call: the plugin asks for all of them, the tool is
/// named in `ask_tools`, or the server marks it destructive.
fn asks(p: &McpPlugin, t: &RemoteTool) -> bool {
    p.consent || t.annotations.get("destructiveHint").and_then(Value::as_bool) == Some(true) || p.ask_tools.iter().any(|pat| matches_pattern(pat, &t.name))
}

/// What a remote tool may do, from its annotations (MCP's hints), plus starting a local server
/// or reaching a remote one. `consent`: it asks before each call ([`asks`]).
fn effects_of(t: &RemoteTool, consent: bool, remote: bool) -> Effects {
    let hint = |k: &str| t.annotations.get(k).and_then(Value::as_bool);
    let mut e = Effects::MUTATES_SESSION | if remote { Effects::ACCESSES_NETWORK } else { Effects::LAUNCHES_PROCESS };
    if hint("readOnlyHint") != Some(true) {
        e |= Effects::WRITES_FILESYSTEM;
    }
    if hint("destructiveHint") == Some(true) {
        e |= Effects::MAY_DISCARD_DATA;
    }
    if hint("openWorldHint") != Some(false) || remote {
        e |= Effects::ACCESSES_NETWORK;
    }
    if hint("idempotentHint") == Some(true) {
        e |= Effects::IDEMPOTENT;
    }
    if consent {
        e |= Effects::EXECUTES_FOREIGN_CODE;
    }
    e
}

/// Read the configured plugins into `session` and return their (cached) tools for
/// [`crate::Engine::shared_with`]. A plugin with an invalid id, or a tool whose id clashes, is
/// skipped and logged.
pub fn install(session: &mut Session) -> Vec<RuntimeTool> {
    let mut plugins = Plugins::default();
    let mut tools = Vec::new();
    let config = session.config_dir();
    for p in session.settings.plugins.mcp.iter().filter(|p| p.enabled) {
        if let Some(problem) = p.problem() {
            log::error!("plugin skipped: {problem}");
            continue;
        }
        let Some(set) = custom_toolset(&p.id) else { continue };
        let remote = matches!(p.server(), Server::Url(_));
        for t in p.cached_tools(&config).into_iter().take(MAX_TOOLS) {
            let mut id = format!("{}_{}", p.id, id_part(&t.name));
            id.truncate(64);
            if !RuntimeTool::valid_id(&id) || tools.iter().any(|x: &RuntimeTool| x.id == id) {
                log::error!("plugin {} tool {:?} skipped (its id {id:?} is invalid or taken)", p.id, t.name);
                continue;
            }
            let schema = if t.input_schema.is_object() { t.input_schema.clone() } else { json!({"type": "object"}) };
            let leaked: &'static str = Box::leak(id.clone().into_boxed_str());
            let declared = schema.get("properties").and_then(Value::as_object);
            let defaults =
                p.default_args.iter().filter(|(k, _)| declared.is_some_and(|d| d.contains_key(k.as_str()))).map(|(k, v)| (k.clone(), v.clone())).collect();
            plugins.routes.insert(leaked, Route { server: p.id.clone(), remote: t.name.clone(), defaults });
            tools.push(RuntimeTool {
                id,
                title: t.title.clone().unwrap_or_else(|| format!("{}: {}", p.display_name(), t.name)),
                summary: t.description.clone().filter(|d| !d.trim().is_empty()).unwrap_or_else(|| format!("{} from the {} plugin.", t.name, p.display_name())),
                beginner: format!("A tool from the {} plugin.", p.display_name()),
                toolset: set,
                effects: effects_of(&t, asks(p, &t), remote),
                requires: Requires::Nothing,
                schema,
                run: call_plugin,
            });
        }
        plugins.servers.insert(p.id.clone(), (p.clone(), Mutex::new(None)));
    }
    session.ext.insert(Arc::new(plugins));
    crate::wasm_plugins::install(session, &mut tools);
    tools
}

/// Start a server, list its tools and cache them. New tools are offered from the next start.
pub fn refresh(p: &McpPlugin, config: &std::path::Path) -> Result<Vec<RemoteTool>, ToolError> {
    if let Some(problem) = p.problem() {
        return Err(ToolError::invalid(problem).with_remediation("Fix it in config.toml ([[plugins.mcp]])."));
    }
    let mut client = start(p, config)?;
    let tools = client.list_tools(START_TIMEOUT).map_err(|e| logged(p, config, client.with_output(e)))?;
    let path = p.cache_path(config);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| ToolError::io(dir.display(), e))?;
    }
    let json = serde_json::to_vec_pretty(&tools).map_err(|e| ToolError::internal(e.to_string()))?;
    crate::project::write_atomic(&path, &json)?;
    let guide = p.instructions_path(config);
    match client.instructions() {
        Some(text) => crate::project::write_atomic(&guide, text.as_bytes())?,
        None => {
            let _ = std::fs::remove_file(&guide);
        }
    }
    Ok(tools)
}

/// The instructions of the plugin whose toolset is `toolset`, kept by its last refresh: how its
/// server wants its tools used (MCP's `instructions`). Shown when an agent enables the toolset.
pub fn instructions_for(session: &Session, toolset: &str) -> Option<String> {
    let p = session.settings.plugins.mcp.iter().find(|p| p.enabled && p.id == toolset)?;
    p.cached_instructions(&session.config_dir())
}

/// Whether a plugin with this id is configured, on, and has a cached tool list.
pub fn is_ready(session: &Session, id: &str) -> bool {
    session.settings.plugins.mcp.iter().any(|p| p.enabled && p.id == id && p.problem().is_none() && p.cache_path(&session.config_dir()).is_file())
}

/// A plugin's text kept in our evidence, at most 2,000 characters.
fn short(s: &str) -> String {
    if s.chars().count() <= 2_000 { s.to_string() } else { s.chars().take(2_000).collect::<String>() + "…" }
}

/// What a result says about itself, when its structured payload carries the usual evidence fields
/// (REA's records do): `confidence`, `limitations` (text, or `{code, message}`) and `coverage`
/// (top level, or in `normalized_result`).
fn facts_of(payload: &Value) -> (Option<Confidence>, Vec<Limitation>, Option<Coverage>) {
    let confidence = match payload.get("confidence").and_then(Value::as_str) {
        Some("observed") => Some(Confidence::Observed),
        Some("derived") => Some(Confidence::Derived),
        Some("inferred") => Some(Confidence::Inferred),
        _ => None,
    };
    let limitations = payload
        .get("limitations")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(40)
                .filter_map(|l| match l {
                    Value::String(m) => Some(Limitation::new("plugin_limitation", short(m))),
                    Value::Object(o) => o
                        .get("message")
                        .and_then(Value::as_str)
                        .map(|m| Limitation::new(short(o.get("code").and_then(Value::as_str).unwrap_or("plugin_limitation")), short(m))),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let coverage = [payload.get("coverage"), payload.get("normalized_result").and_then(|n| n.get("coverage"))].into_iter().flatten().find_map(|c| {
        let status = match c.get("status").and_then(Value::as_str)? {
            s if s.starts_with("complete") => CoverageStatus::Complete,
            s if s.starts_with("partial") || s == "truncated" => CoverageStatus::Partial,
            _ => CoverageStatus::Unknown,
        };
        Some(Coverage {
            status,
            examined: c.get("examined").and_then(Value::as_u64),
            total: c.get("total").and_then(Value::as_u64),
            next_offset: c.get("next_offset").and_then(Value::as_u64),
        })
    });
    (confidence, limitations, coverage)
}

/// The same answer twice is sent once: a text block that only repeats `structuredContent` as JSON
/// is dropped (REA sends both, so its records reached agents at double their size).
fn without_repeated_text(mut result: Value) -> Value {
    let Some(structured) = result.get("structuredContent").cloned() else { return result };
    if let Some(blocks) = result.get_mut("content").and_then(Value::as_array_mut) {
        let before = blocks.len();
        blocks.retain(|b| {
            let repeats = b.get("type").and_then(Value::as_str) == Some("text")
                && b.get("text").and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()).is_some_and(|v| v == structured);
            !repeats
        });
        if blocks.len() < before {
            blocks.push(json!({"type": "text", "text": "(The full answer is in structuredContent.)"}));
        }
    }
    result
}

/// How healthy one configured plugin looks, for `doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginHealth {
    pub id: String,
    pub level: HealthLevel,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthLevel {
    Ok,
    Info,
    Warn,
    Fail,
}

fn health(id: &str, level: HealthLevel, message: impl Into<String>, fix: Option<String>) -> PluginHealth {
    PluginHealth { id: id.to_string(), level, message: message.into(), fix }
}

/// How long `doctor` waits for one plugin's handshake and tool list.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

/// `doctor`'s look at every configured MCP plugin: its settings, whether its program is found,
/// whether its tool list was read, and (when `start` is set) whether it starts and answers now with
/// the tools that were cached. Starting runs the plugin's own program, as its first call would.
/// `study_engines` naming a plugin that isn't ready is reported too.
pub fn health_of(session: &Session, start: bool) -> Vec<PluginHealth> {
    let config = session.config_dir();
    let mut out = Vec::new();
    for p in &session.settings.plugins.mcp {
        let id = p.id.as_str();
        if !p.enabled {
            out.push(health(id, HealthLevel::Info, format!("The {} plugin is turned off.", p.display_name()), None));
            continue;
        }
        if let Some(problem) = p.problem() {
            out.push(health(id, HealthLevel::Fail, problem, Some("Fix it in config.toml ([[plugins.mcp]]).".into())));
            continue;
        }
        if let Server::Command(cmd) = p.server() {
            let program = std::path::Path::new(&cmd.command);
            let found = (program.is_absolute() && program.is_file()) || craft_anything_platform::which(&cmd.command).is_some();
            if !found {
                out.push(health(
                    id,
                    HealthLevel::Fail,
                    format!("The {} plugin's program `{}` wasn't found.", p.display_name(), cmd.command),
                    Some("Install it, or set its full path as `command` in config.toml ([[plugins.mcp]]).".into()),
                ));
                continue;
            }
        }
        let cached = p.cached_tools(&config).len();
        let refresh = "Run `craft-anything-cli plugins-refresh --allow-run`.".to_string();
        if cached == 0 {
            out.push(health(
                id,
                HealthLevel::Warn,
                format!("The {} plugin's tools haven't been read yet, so none are offered.", p.display_name()),
                Some(refresh),
            ));
            continue;
        }
        if !start || p.consent {
            // A plugin that asks before each call isn't run without asking, not even by doctor.
            let why = if p.consent { "not started: it asks before running" } else { "not started" };
            out.push(health(id, HealthLevel::Ok, format!("The {} plugin offers {cached} tools ({why}).", p.display_name()), None));
            continue;
        }
        let began = std::time::Instant::now();
        let answered = McpClient::connect(p.display_name(), &p.server(), HEALTH_TIMEOUT).and_then(|mut c| c.list_tools(HEALTH_TIMEOUT));
        match answered {
            Ok(tools) if tools.len() == cached => out.push(health(
                id,
                HealthLevel::Ok,
                format!("The {} plugin started and answered in {} ms with its {cached} tools.", p.display_name(), began.elapsed().as_millis()),
                None,
            )),
            Ok(tools) => out.push(health(
                id,
                HealthLevel::Warn,
                format!("The {} plugin now has {} tools, and {cached} were read last time (it was updated).", p.display_name(), tools.len()),
                Some(refresh),
            )),
            Err(e) => out.push(health(
                id,
                HealthLevel::Fail,
                format!("The {} plugin didn't start or answer: {}", p.display_name(), e.message),
                Some(e.remediation.unwrap_or_else(|| "Run its command by hand to see what it prints.".into())),
            )),
        }
    }
    let mut named: Vec<&str> =
        session.settings.build.study_engines.iter().map(|e| e.trim()).filter(|e| !e.is_empty() && *e != crate::settings::STUDY_CRAFT).collect();
    named.sort_unstable();
    named.dedup();
    for engine in named {
        if !is_ready(session, engine) {
            out.push(health(
                engine,
                HealthLevel::Warn,
                format!("build.study_engines names `{engine}`, but no plugin with that id is configured, on and read, so the built-in engine studies instead."),
                Some(format!("Add the `{engine}` plugin to config.toml ([[plugins.mcp]]) and run `craft-anything-cli plugins-refresh --allow-run`.")),
            ));
        } else if !session.settings.deep_mode {
            out.push(health(
                engine,
                HealthLevel::Info,
                format!("`{engine}` studies and checks only in Deep mode, which is off in Settings (`--deep` turns it on for one run)."),
                None,
            ));
        }
    }
    out
}

/// Start a plugin's server and complete the handshake, keeping a failure log when it fails.
fn start(p: &McpPlugin, config: &std::path::Path) -> Result<McpClient, ToolError> {
    McpClient::connect(p.display_name(), &p.server(), START_TIMEOUT).map_err(|e| logged(p, config, e))
}

/// A local server that didn't start or answer: write what ran and what it printed to a failure
/// log in the config folder's `logs/` ([`crate::failure_log`]) and name it in the error. Its
/// environment is left out and what looks like a key in its arguments is hidden. A remote server's error already says what it
/// answered, and its address can carry a key, so it gets no log; nor does a stop.
fn logged(p: &McpPlugin, config: &std::path::Path, e: ToolError) -> ToolError {
    let Server::Command(cmd) = p.server() else { return e };
    if e.code == ErrorCode::Cancelled {
        return e;
    }
    // An argument can carry a key where no flag names it (`https://…/mcp?token=…`).
    let command = crate::journal::scrubbed_line(&craft_anything_platform::process::printable_command(std::ffi::OsStr::new(&cmd.command), &cmd.args));
    let detail = |k: &str| e.details.as_ref().and_then(|d| d.get(k)).cloned();
    let report = crate::failure_log::Report {
        what: format!("The {} plugin (an MCP server) didn't start", p.display_name()),
        check: command.clone(),
        command,
        error: Some(e.message.clone()),
        cwd: cmd.cwd.clone(),
        exit_code: detail("exit_code").and_then(|c| c.as_i64()).and_then(|c| i32::try_from(c).ok()),
        stderr: detail("output_tail").and_then(|t| t.as_str().map(|s| s.as_bytes().to_vec())).unwrap_or_default(),
        ..Default::default()
    };
    match crate::failure_log::write(&crate::failure_log::dir_in(config), &report) {
        Ok(path) => {
            let said = e.remediation.clone().map(|r| format!("{r}\n\n")).unwrap_or_default();
            e.with_remediation(format!("{said}The details are in {}", path.display()))
        }
        Err(w) => {
            log::warn!("couldn't write a failure log for the {} plugin: {w}", p.display_name());
            e
        }
    }
}

/// The function behind every proxied tool: route by [`Ctx::tool`], start the server if needed,
/// call it, and record the result as evidence.
fn call_plugin(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let plugins = ctx.session.ext.get::<Plugins>().ok_or_else(|| ToolError::internal("plugins aren't loaded in this session"))?;
    let route = plugins.routes.get(ctx.tool).ok_or_else(|| ToolError::internal(format!("no plugin serves {}", ctx.tool)))?;
    let (plugin, client) = plugins.servers.get(&route.server).ok_or_else(|| ToolError::internal(format!("plugin {} isn't loaded", route.server)))?;
    let mut slot = client.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_none() {
        ctx.report(None, format!("Starting the {} plugin…", plugin.display_name()));
        *slot = Some(start(plugin, &ctx.session.config_dir())?);
    }
    let Some(c) = slot.as_mut() else { return Err(ToolError::internal("plugin client missing")) };
    let cancel = ctx.cancel.clone();
    let mut args = if params.is_null() { json!({}) } else { params.clone() };
    if let Some(given) = args.as_object_mut() {
        for (k, v) in &route.defaults {
            given.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    let result = match c.call_tool(&route.remote, args.clone(), CALL_TIMEOUT, &move || cancel.is_cancelled()) {
        Ok(r) => r,
        Err(e) => {
            // A server that died is started again on the next call.
            if e.code == ErrorCode::EngineMissing {
                *slot = None;
            }
            return Err(e);
        }
    };
    let text = content_text(&result);
    let summary: String = if text.is_empty() { format!("{} answered.", plugin.display_name()) } else { text.chars().take(300).collect() };
    let structured = result.get("structuredContent").cloned().or_else(|| serde_json::from_str::<Value>(&text).ok()).unwrap_or(Value::Null);
    let (confidence, limitations, coverage) = facts_of(&structured);
    let result = without_repeated_text(result);
    let env = Environment {
        clean_room: ctx.session.clean_room(),
        engine: format!("plugin:{}", plugin.id),
        engine_version: None,
        host: craft_anything_evidence::host_triple(),
        language_id: None,
    };
    let e = EvidenceBuilder::new(ctx.tool, Subject { kind: "plugin".into(), sha256: None, name: Some(plugin.display_name().to_string()) }, env)
        .params(args)
        .authority(Authority::OutsideService)
        .confidence(confidence.unwrap_or(Confidence::Observed))
        .limitations(limitations)
        .coverage(coverage.unwrap_or_default())
        .result(result)
        .summary(summary)
        .build()?;
    Ok(ToolOutput::evidence(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_names_become_our_ids_and_annotations_our_effects() {
        assert_eq!(id_part("Search Notes!"), "search_notes");
        assert_eq!(id_part("get-page__v2"), "get_page_v2");
        let t = |ann: Value| RemoteTool { name: "x".into(), title: None, description: None, input_schema: json!({}), annotations: ann };
        let read = effects_of(&t(json!({"readOnlyHint": true, "openWorldHint": false})), false, false);
        assert!(!read.contains(Effects::WRITES_FILESYSTEM) && !read.contains(Effects::ACCESSES_NETWORK) && !read.needs_consent());
        let risky = effects_of(&t(json!({"destructiveHint": true})), true, false);
        assert!(risky.contains(Effects::MAY_DISCARD_DATA) && risky.needs_consent(), "consent plugins ask first");
        assert!(effects_of(&t(Value::Null), false, false).contains(Effects::WRITES_FILESYSTEM), "no hints: assume it may write");
        let remote = effects_of(&t(json!({"readOnlyHint": true, "openWorldHint": false})), false, true);
        assert!(remote.contains(Effects::ACCESSES_NETWORK) && !remote.contains(Effects::LAUNCHES_PROCESS), "a remote server uses the network");
    }

    #[test]
    fn plugins_without_a_cache_add_no_tools_and_bad_ids_are_skipped() {
        let mut s = crate::testing::session();
        s.settings.plugins.mcp =
            vec![McpPlugin { id: "Bad Id".into(), ..plugin("x") }, McpPlugin { id: "never_refreshed_plugin_for_tests".into(), ..plugin("x") }];
        assert!(install(&mut s).is_empty());
        assert!(s.ext.get::<Plugins>().is_some());
        let parsed: PluginSettings = toml::from_str("[[mcp]]\nid = \"notes\"\ncommand = \"notes-mcp\"\n").unwrap();
        assert!(parsed.mcp[0].enabled && parsed.mcp[0].consent, "on and asking by default");
    }

    fn plugin(command: &str) -> McpPlugin {
        McpPlugin {
            id: "notes".into(),
            name: None,
            command: command.into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            headers: BTreeMap::new(),
            enabled: true,
            consent: true,
            default_args: BTreeMap::new(),
            ask_tools: Vec::new(),
        }
    }

    #[test]
    fn a_plugin_that_exits_at_once_says_what_it_printed_and_leaves_a_log() {
        #[cfg(windows)]
        let (command, args) = ("cmd", vec!["/C".to_string(), "echo bad token in notes.cfg 1>&2 & exit 3".to_string()]);
        #[cfg(not(windows))]
        let (command, args) = ("sh", vec!["-c".to_string(), "echo bad token in notes.cfg >&2; exit 3".to_string()]);
        let config = tempfile::tempdir().unwrap();
        let p = McpPlugin { args, env: BTreeMap::from([("NOTES_KEY".to_string(), "sk-test-secret".to_string())]), ..plugin(command) };
        let e = refresh(&p, config.path()).unwrap_err();
        assert!(e.message.contains("exited with exit code 3"), "{e:?}");
        let said = e.remediation.unwrap_or_default();
        assert!(said.contains("bad token in notes.cfg") && said.contains("plugins.mcp"), "{said}");
        let logs: Vec<_> = std::fs::read_dir(crate::failure_log::dir_in(config.path())).unwrap().flatten().map(|f| f.path()).collect();
        assert_eq!(logs.len(), 1);
        assert!(said.contains(&format!("The details are in {}", logs[0].display())), "{said}");
        let log = std::fs::read_to_string(&logs[0]).unwrap();
        for part in ["notes plugin (an MCP server) didn't start", "Exit code: 3", "bad token in notes.cfg", command] {
            assert!(log.contains(part), "{part}: {log}");
        }
        assert!(!log.contains("sk-test-secret"), "its environment stays out of the log");
    }

    #[test]
    fn a_plugin_that_cant_start_names_its_log_and_a_remote_one_writes_none() {
        let config = tempfile::tempdir().unwrap();
        let args = vec!["--url".to_string(), "https://mcp.example.test/mcp?token=plainsecretvalue42".to_string()];
        let e = refresh(&McpPlugin { args, ..plugin("craft-anything-no-such-plugin-server") }, config.path()).unwrap_err();
        assert!(e.remediation.unwrap_or_default().contains("The details are in"), "a missing program is logged too");
        let logs: Vec<_> = std::fs::read_dir(crate::failure_log::dir_in(config.path())).unwrap().flatten().map(|f| f.path()).collect();
        let log = std::fs::read_to_string(&logs[0]).unwrap();
        assert!(log.contains("mcp.example.test/mcp?token=") && !log.contains("plainsecretvalue42"), "a key in an argument is hidden: {log}");
        let remote = McpPlugin { url: Some("http://127.0.0.1:0/mcp".into()), ..plugin("") };
        let other = tempfile::tempdir().unwrap();
        assert!(refresh(&remote, other.path()).is_err());
        assert!(!crate::failure_log::dir_in(other.path()).exists(), "no log for a remote server");
    }

    #[test]
    fn a_repeated_text_block_is_dropped_and_the_records_own_facts_are_kept() {
        let rec = json!({"confidence": "derived", "limitations": ["only static facts", {"code": "truncated", "message": "page 1"}], "normalized_result": {"coverage": {"status": "complete-within-view", "examined": 12, "total": 12}}});
        let result = json!({"content": [{"type": "text", "text": rec.to_string()}], "structuredContent": rec});
        let trimmed = without_repeated_text(result);
        let blocks = trimmed["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0]["text"].as_str().unwrap().contains("structuredContent"));
        let (c, l, cov) = facts_of(&trimmed["structuredContent"]);
        assert_eq!(c, Some(Confidence::Derived));
        assert_eq!(l.len(), 2);
        assert_eq!(l[1].code, "truncated");
        let cov = cov.unwrap();
        assert_eq!((cov.status, cov.examined, cov.total), (CoverageStatus::Complete, Some(12), Some(12)));
        let other = json!({"content": [{"type": "text", "text": "hello"}], "structuredContent": {"a": 1}});
        assert_eq!(without_repeated_text(other.clone()), other, "a different text is kept");
    }

    #[test]
    fn named_and_destructive_tools_ask_even_when_the_plugin_doesnt() {
        assert!(matches_pattern("*adb*", "install_adb_package") && matches_pattern("start_*", "start_frida_session"));
        assert!(matches_pattern("*_scenario", "capture_web_scenario") && matches_pattern("Close_GDB_Session", "close_gdb_session"));
        assert!(matches_pattern("a*b*c", "axxbyyc") && !matches_pattern("a*b*c", "axxcyyb"));
        assert!(!matches_pattern("*adb*", "analyze_javascript_application") && !matches_pattern("start_*", "restart"));
        assert!(!matches_pattern("ab*ba", "aba"), "the two ends can't share a character");
        let t = |name: &str, ann: Value| RemoteTool { name: name.into(), title: None, description: None, input_schema: json!({}), annotations: ann };
        let p = McpPlugin { consent: false, ask_tools: vec!["*adb*".into()], ..plugin("rea") };
        assert!(!asks(&p, &t("analyze_javascript_application", json!({"readOnlyHint": false}))));
        assert!(asks(&p, &t("install_adb_package", Value::Null)));
        assert!(asks(&p, &t("set_comment", json!({"destructiveHint": true}))), "a destructive tool always asks");
        assert!(effects_of(&t("install_adb_package", Value::Null), asks(&p, &t("install_adb_package", Value::Null)), false).needs_consent());
        assert!(asks(&McpPlugin { consent: true, ..plugin("x") }, &t("anything", Value::Null)));
        let parsed: PluginSettings = toml::from_str("[[mcp]]\nid = \"rea\"\ncommand = \"rea\"\nask_tools = [\"*adb*\"]\n").unwrap();
        assert_eq!(parsed.mcp[0].ask_tools, ["*adb*"]);
    }

    #[test]
    fn doctor_names_a_missing_program_an_unread_plugin_and_a_study_engine_with_no_plugin() {
        let mut s = crate::testing::session();
        let here = std::env::current_exe().unwrap().display().to_string();
        s.settings.plugins.mcp = vec![
            McpPlugin { id: "gone".into(), ..plugin("craft-anything-no-such-plugin-server") },
            McpPlugin { id: "unread".into(), ..plugin(&here) },
            McpPlugin { id: "off".into(), enabled: false, ..plugin(&here) },
        ];
        s.settings.build.study_engines = vec!["craft".into(), "rea".into()];
        let h = health_of(&s, false);
        let level = |id: &str| h.iter().find(|x| x.id == id).map(|x| x.level);
        assert_eq!(level("gone"), Some(HealthLevel::Fail));
        assert_eq!(level("unread"), Some(HealthLevel::Warn));
        assert_eq!(level("off"), Some(HealthLevel::Info));
        assert_eq!(level("rea"), Some(HealthLevel::Warn), "study_engines names rea with no rea plugin");
        assert!(h.iter().find(|x| x.id == "unread").and_then(|x| x.fix.as_deref()).is_some_and(|f| f.contains("plugins-refresh")));
        assert!(h.iter().all(|x| x.id != "craft"), "the built-in engine needs no plugin");

        // A plugin that asks before each call is reported from its cache, never started.
        let cached = McpPlugin { id: "asks".into(), ..plugin(&here) };
        let cache = cached.cache_path(&s.config_dir());
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(&cache, r#"[{"name": "x"}]"#).unwrap();
        s.settings.plugins.mcp = vec![cached];
        s.settings.build.study_engines = vec!["rea".into(), " rea ".into()];
        let h = health_of(&s, true);
        let asks = h.iter().find(|x| x.id == "asks").unwrap();
        assert_eq!(asks.level, HealthLevel::Ok);
        assert!(asks.message.contains("not started: it asks before running"), "{}", asks.message);
        assert_eq!(h.iter().filter(|x| x.id == "rea").count(), 1, "a name listed twice is said once");
    }

    #[test]
    fn default_args_parse_from_config() {
        let parsed: PluginSettings =
            toml::from_str("[[mcp]]\nid = \"rea\"\ncommand = \"rea\"\nconsent = false\ndefault_args = { detail = \"summary\" }\n").unwrap();
        assert_eq!(parsed.mcp[0].default_args.get("detail"), Some(&json!("summary")));
        assert!(!parsed.mcp[0].consent);
    }

    #[test]
    fn a_plugin_is_local_or_remote_never_both() {
        assert!(plugin("notes-mcp").problem().is_none());
        assert!(matches!(plugin("notes-mcp").server(), Server::Command(_)));
        let remote = McpPlugin { url: Some("https://mcp.example.com/mcp".into()), ..plugin("") };
        assert!(remote.problem().is_none() && matches!(remote.server(), Server::Url(_)));
        assert_eq!(remote.place(), "https://mcp.example.com/mcp");
        assert!(plugin("").problem().unwrap().contains("command"));
        assert!(McpPlugin { url: Some("https://x".into()), ..plugin("notes-mcp") }.problem().unwrap().contains("both"));
        assert!(McpPlugin { url: Some("http://example.com/mcp".into()), ..plugin("") }.problem().unwrap().contains("https://"));
        let parsed: PluginSettings =
            toml::from_str("[[mcp]]\nid = \"docs\"\nurl = \"https://mcp.example.com/mcp\"\nheaders = { Authorization = \"Bearer t\" }\n").unwrap();
        assert!(parsed.mcp[0].problem().is_none() && parsed.mcp[0].headers.contains_key("Authorization"));
        assert!(!toml::to_string(&parsed).unwrap().contains("command"), "a remote plugin is saved without an empty command");
    }
}
