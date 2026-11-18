//! REA as a second engine (the `rea` MCP plugin, `[[plugins.mcp]] id = "rea"`), when
//! `build.study_engines` lists `rea` and Deep mode is on.
//!
//! - Study: REA looks at the original too. Its records go to `capture/rea/` and a short view of
//!   them into `capture/README.md`, beside (or instead of) the built-in engine's.
//! - Check: after each build of a JavaScript/Electron app, REA reads the build and its surface (the
//!   IPC channels, the APIs a preload exposes to pages, the windows it opens) is compared with the
//!   original's. Each part of the original's surface is a claim; one with no counterpart in the
//!   build is sent back to the builder.
//!
//! REA's own unknowns are not this check's unknowns: a rewrite pairs with the original only where
//! the surface matches, so the rest of REA's comparison stays a note, never a reason the build
//! can't be good enough.

use super::*;
use craft_anything_engine::settings::STUDY_REA;
use craft_anything_engine::workspace::Claim;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// REA's static map of a JavaScript or Electron app.
pub const ANALYZE_JS: &str = "rea_analyze_javascript_application";
/// REA's first look at an installed desktop app's folder (.NET, Unity, Unreal, Godot, Electron…).
pub const INSPECT_DESKTOP: &str = "rea_inspect_desktop_application";
/// The check's id.
pub const SURFACE_CHECK: &str = "rea-surface";
/// Where the study keeps REA's records, under the project folder.
pub const CAPTURE_DIR: &str = "capture/rea";
/// The original's surface, as the study found it (what the check compares against).
pub const SURFACE_FILE: &str = "surface.json";
/// Most claims one check makes: a big app's surface is summarised, not listed in full.
const MAX_CLAIMS: usize = 40;

/// Whether REA studies and checks in this session: Deep mode, `rea` in `build.study_engines`, and
/// the plugin configured, on and refreshed.
pub fn rea_on(session: &craft_anything_engine::Session, deep: bool) -> bool {
    deep && session.settings.build.studies_with(STUDY_REA) && craft_anything_engine::plugins::is_ready(session, STUDY_REA)
}

/// The record REA answered with: the plugin evidence's `structuredContent`, else its text as JSON.
pub fn record_of(out: &ToolOutput) -> Value {
    let result = match out {
        ToolOutput::Evidence(e) => &e.result,
        ToolOutput::Value(v) => v,
    };
    if let Some(s) = result.get("structuredContent") {
        return s.clone();
    }
    let text = craft_anything_engines::mcp_client::content_text(result);
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// A short view of a REA record for the study notes: its summary and statistics, the coverage of
/// what it looked at, and its first limitations. The whole record is kept beside the notes.
pub fn notes_view(record: &Value) -> String {
    let n = record.get("normalized_result").unwrap_or(record);
    let mut view = serde_json::Map::new();
    for key in ["summary", "statistics", "coverage", "runtime_candidates", "next_probes", "kind"] {
        if let Some(v) = n.get(key).filter(|v| !v.is_null()) {
            view.insert(key.to_string(), v.clone());
        }
    }
    if view.is_empty() {
        view.insert("record".into(), n.clone());
    }
    let mut body = serde_json::to_string_pretty(&Value::Object(view)).unwrap_or_default();
    if body.chars().count() > 6_000 {
        body = body.chars().take(6_000).collect::<String>() + "\n… (cut; the whole record is in the file named above)";
    }
    let limits: Vec<&str> = record.get("limitations").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).take(5).collect();
    let limits = if limits.is_empty() {
        String::new()
    } else {
        format!("\nWhat it couldn't establish:\n{}\n", limits.iter().map(|l| format!("- {l}")).collect::<Vec<_>>().join("\n"))
    };
    format!("```json\n{body}\n```\n{limits}")
}

/// What an app shows of itself to the rest of the program: its IPC channels, the members of the APIs
/// a preload exposes to its pages (`api.member`), and how many windows it opens.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Surface {
    pub channels: BTreeSet<String>,
    pub bridge: BTreeSet<String>,
    pub windows: usize,
}

impl Surface {
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty() && self.bridge.is_empty() && self.windows == 0
    }

    /// One line for people: "2 IPC channels, 2 exposed API members, 1 window".
    pub fn describe(&self) -> String {
        format!("{} IPC channel(s), {} exposed API member(s), {} window(s)", self.channels.len(), self.bridge.len(), self.windows)
    }
}

/// The surface in a complete `analyze_javascript_application` record (its application graph).
pub fn surface_of(record: &Value) -> Surface {
    let mut s = Surface::default();
    let nodes = record.pointer("/normalized_result/graph/nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    for node in &nodes {
        let kind = node.get("kind").and_then(Value::as_str).unwrap_or_default();
        let observations = node.get("observations").and_then(Value::as_array).cloned().unwrap_or_default();
        match kind {
            "ipc-channel" => {
                for o in &observations {
                    let channel = o.pointer("/properties/channel").and_then(Value::as_str).or_else(|| o.get("label").and_then(Value::as_str));
                    if let Some(c) = channel.filter(|c| !c.is_empty()) {
                        s.channels.insert(c.to_string());
                    }
                }
            }
            "context-bridge-api" => {
                for o in &observations {
                    let api = o.pointer("/properties/api_key").and_then(Value::as_str).or_else(|| o.get("label").and_then(Value::as_str)).unwrap_or("api");
                    let members: Vec<&str> =
                        o.pointer("/properties/members").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
                    if members.is_empty() {
                        s.bridge.insert(api.to_string());
                    }
                    for m in members {
                        s.bridge.insert(format!("{api}.{m}"));
                    }
                }
            }
            "browser-window" => s.windows += 1,
            _ => {}
        }
    }
    s
}

/// Each part of the original's surface, as a claim that the build has it too.
pub fn surface_claims(original: &Surface, built: &Surface) -> Vec<Claim> {
    let mut claims = Vec::new();
    for c in &original.channels {
        claims.push(Claim::new(
            format!("IPC channel {c}"),
            built.channels.contains(c),
            format!("the original's main and page code talk over the IPC channel \"{c}\"; the build has no such channel"),
        ));
    }
    for m in &original.bridge {
        claims.push(Claim::new(
            format!("Exposed API {m}"),
            built.bridge.contains(m),
            format!("the original's preload gives its pages {m}; the build's preload doesn't"),
        ));
    }
    if original.windows > 0 {
        claims.push(Claim::new(
            "Windows",
            built.windows >= original.windows,
            format!("the original opens {} window(s); REA found {} in the build", original.windows, built.windows),
        ));
    }
    claims.truncate(MAX_CLAIMS);
    claims
}

/// The original's surface and where its JavaScript is, as the study saved them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedSurface {
    pub js_input: PathBuf,
    pub surface: Surface,
}

/// Read what the study saved, if REA studied a JavaScript app in this project.
pub fn saved_surface(ws: &Workspace) -> Option<SavedSurface> {
    let bytes = std::fs::read(ws.root.join(CAPTURE_DIR).join(SURFACE_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The build is a JavaScript app REA can read: app/ has a package.json.
fn build_is_js(ws: &Workspace) -> bool {
    ws.app_dir().join("package.json").is_file()
}

/// After each build of a JavaScript/Electron app: REA's surface of the build against the original's.
pub struct ReaSurfaceCheck;

impl Check for ReaSurfaceCheck {
    fn id(&self) -> &'static str {
        SURFACE_CHECK
    }
    fn applies(&self, cx: &CheckCx) -> bool {
        cx.rea_on() && build_is_js(cx.ws) && saved_surface(cx.ws).is_some_and(|s| !s.surface.is_empty())
    }
    fn can_refine(&self, result: &CheckResult) -> bool {
        result.failed_claims().next().is_some()
    }
    fn run(&self, cx: &mut CheckCx) -> Result<CheckResult, ToolError> {
        let Some(saved) = saved_surface(cx.ws) else { return Ok(CheckResult::new(self.id(), cx.round, true, "REA hasn't studied the original's JavaScript.")) };
        let app = cx.ws.app_dir();
        let record = match cx.call(ANALYZE_JS, json!({"input_path": app.display().to_string(), "detail": "complete"})) {
            Ok(out) => record_of(&out),
            Err(e) if e.code == ErrorCode::Cancelled || e.code == ErrorCode::TimedOut => return Err(e),
            Err(e) => {
                // REA couldn't read the build: say so, without holding the build back for it.
                let mut r = CheckResult::new(self.id(), cx.round, true, format!("REA couldn't read the build's JavaScript: {}", e.message));
                r.similarity = None;
                return Ok(r);
            }
        };
        let built = surface_of(&record);
        let out_dir = cx.ws.checks_dir().join(format!("round-{}", cx.round)).join("rea");
        let mut artifacts = Vec::new();
        if std::fs::create_dir_all(&out_dir).is_ok() {
            let file = out_dir.join("surface.json");
            let saved_json = json!({"original": saved.surface, "build": built});
            if craft_anything_engine::project::write_atomic(&file, serde_json::to_string_pretty(&saved_json).unwrap_or_default().as_bytes()).is_ok()
                && let Ok(rel) = file.strip_prefix(&cx.ws.root)
            {
                artifacts.push(rel.to_path_buf());
            }
        }
        let claims = surface_claims(&saved.surface, &built);
        let failed: Vec<String> = claims.iter().filter(|c| !c.passed).map(|c| c.detail.clone()).collect();
        let note = if failed.is_empty() {
            format!("REA finds the original's surface in the build ({}).", saved.surface.describe())
        } else {
            format!(
                "REA compared what the app shows of itself (IPC channels, the APIs its preload exposes, its windows) and {} of {} parts of the original's have no counterpart in the build: {}. Add them with the same names, or say in your reply why the build does it another way. The original's surface is in capture/rea/surface.json; the build's in checks/round-{}/rea/surface.json.",
                failed.len(),
                claims.len(),
                failed.join("; "),
                cx.round
            )
        };
        let mut r = CheckResult::new(self.id(), cx.round, true, note);
        r.claims = claims;
        r.artifacts = artifacts;
        Ok(r)
    }
}

impl Worker {
    /// Study with REA: its first look at the app's folder and, for a JavaScript app, its map of
    /// the code. Records go to capture/rea/, the original's surface to capture/rea/surface.json,
    /// and the notes section comes back. A REA failure is said in the notes and the study goes on.
    pub(in crate::rebuild) fn study_with_rea(&mut self, ws: &mut Workspace, path: &Path, program: &Path, js_input: Option<&Path>) -> Result<String, ToolError> {
        let dir = ws.root.join(CAPTURE_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| ToolError::io(dir.display(), e))?;
        let keep = |name: &str, record: &Value| -> Result<(), ToolError> {
            craft_anything_engine::project::write_atomic(&dir.join(name), serde_json::to_string_pretty(record).unwrap_or_default().as_bytes())
        };
        let mut notes = String::from(
            "\n## REA\n\nREA (a second reverse-engineering engine) looked at it too. Its whole records are in capture/rea/; call enable_toolset with \"rea\" for its other tools.\n",
        );
        let folder = if path.is_dir() { path.to_path_buf() } else { program.parent().map(Path::to_path_buf).unwrap_or_else(|| path.to_path_buf()) };
        self.step(ws, Step::Study, Status::Inspecting, "REA is looking at the app's folder…")?;
        match self.call(INSPECT_DESKTOP, json!({"path": folder.display().to_string(), "detail": "summary"})) {
            Ok(out) => {
                let record = record_of(&out);
                keep("desktop.json", &record)?;
                notes.push_str(&format!("\n### What the app is made of (capture/rea/desktop.json)\n\n{}", notes_view(&record)));
            }
            Err(e) if e.code == ErrorCode::Cancelled => return Err(e),
            Err(e) => notes.push_str(&format!("\nREA's look at the folder failed: {}\n", e.message)),
        }
        if let Some(input) = js_input {
            self.step(ws, Step::Study, Status::Inspecting, "REA is mapping its JavaScript…")?;
            match self.call(ANALYZE_JS, json!({"input_path": input.display().to_string(), "detail": "complete"})) {
                Ok(out) => {
                    let record = record_of(&out);
                    keep("javascript.json", &record)?;
                    let surface = surface_of(&record);
                    keep(SURFACE_FILE, &serde_json::to_value(SavedSurface { js_input: input.to_path_buf(), surface: surface.clone() }).unwrap_or(Value::Null))?;
                    notes.push_str(&format!(
                        "\n### Its JavaScript (capture/rea/javascript.json)\n\nWhat it shows of itself: {}.\n- IPC channels: {}\n- Exposed to its pages: {}\n\nAfter each build REA reads app/ the same way and checks that this surface is there.\n\n{}",
                        surface.describe(),
                        list_or_none(&surface.channels),
                        list_or_none(&surface.bridge),
                        notes_view(&record)
                    ));
                }
                Err(e) if e.code == ErrorCode::Cancelled => return Err(e),
                Err(e) => notes.push_str(&format!("\nREA's map of the JavaScript failed: {}\n", e.message)),
            }
        }
        Ok(notes)
    }
}

fn list_or_none(items: &BTreeSet<String>) -> String {
    if items.is_empty() { "none found".into() } else { items.iter().map(|i| format!("`{i}`")).collect::<Vec<_>>().join(", ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(channels: &[&str], api: &str, members: &[&str], windows: usize) -> Value {
        let mut nodes: Vec<Value> =
            channels.iter().map(|c| json!({"kind": "ipc-channel", "observations": [{"label": c, "properties": {"channel": c, "side": "main"}}]})).collect();
        nodes.push(json!({"kind": "context-bridge-api", "observations": [{"label": api, "properties": {"api_key": api, "members": members}}]}));
        for _ in 0..windows {
            nodes.push(json!({"kind": "browser-window", "observations": []}));
        }
        nodes.push(json!({"kind": "javascript-module", "observations": [{"label": "main.js"}]}));
        json!({"normalized_result": {"graph": {"nodes": nodes}}})
    }

    #[test]
    fn the_surface_is_read_from_reas_graph() {
        let s = surface_of(&record(&["notes:search", "notes:copy"], "notes", &["copy", "search"], 1));
        assert_eq!(s.channels.iter().map(String::as_str).collect::<Vec<_>>(), ["notes:copy", "notes:search"]);
        assert_eq!(s.bridge.iter().map(String::as_str).collect::<Vec<_>>(), ["notes.copy", "notes.search"]);
        assert_eq!(s.windows, 1);
        assert!(surface_of(&json!({})).is_empty(), "a record without a graph has no surface");
    }

    #[test]
    fn a_missing_part_of_the_surface_is_a_failed_claim() {
        let original = surface_of(&record(&["notes:search", "notes:copy"], "notes", &["copy", "search"], 1));
        let same = surface_claims(&original, &original);
        assert_eq!(same.len(), 5);
        assert!(same.iter().all(|c| c.passed));
        let built = surface_of(&record(&["notes:search"], "notes", &["search"], 1));
        let claims = surface_claims(&original, &built);
        let failed: Vec<&str> = claims.iter().filter(|c| !c.passed).map(|c| c.title.as_str()).collect();
        assert_eq!(failed, ["IPC channel notes:copy", "Exposed API notes.copy"]);
        assert!(claims.iter().filter(|c| !c.passed).all(|c| !c.detail.is_empty()));
    }

    #[test]
    fn notes_keep_the_summary_and_limitations() {
        let rec = json!({"limitations": ["only static facts"], "normalized_result": {"summary": {"browser_windows": 1}, "statistics": {"relevant_files": 5}, "graph": {"nodes": []}}});
        let v = notes_view(&rec);
        assert!(v.contains("browser_windows") && v.contains("relevant_files") && v.contains("only static facts"));
        assert!(!v.contains("\"graph\""), "the graph stays in the file");
    }

    #[test]
    fn the_record_comes_from_structured_content_or_text() {
        let structured = ToolOutput::Value(json!({"structuredContent": {"a": 1}, "content": [{"type": "text", "text": "x"}]}));
        assert_eq!(record_of(&structured), json!({"a": 1}));
        let text = ToolOutput::Value(json!({"content": [{"type": "text", "text": "{\"b\":2}"}]}));
        assert_eq!(record_of(&text), json!({"b": 2}));
    }
}
