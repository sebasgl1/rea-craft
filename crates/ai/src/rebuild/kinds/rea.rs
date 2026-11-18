//! REA as a second engine (the `rea` MCP plugin, `[[plugins.mcp]] id = "rea"`), when
//! `build.study_engines` lists `rea` and Deep mode is on.
//!
//! - Study: REA looks at the original too. Its records go to `capture/rea/` and a short view of
//!   them into `capture/README.md`, beside (or instead of) the built-in engine's.
//! - Check: after each build of a JavaScript/Electron app, REA reads the build and its surface (the
//!   IPC channels, the APIs a preload exposes to pages, the windows it opens) is compared with the
//!   original's. Each part of the original's surface is a claim; one with no counterpart in the
//!   build is sent back to the builder, unless the builder declared the difference on purpose in
//!   `app/rea-surface-waivers.json` with a reason.
//!
//! REA reads a staged copy of the app's own sources: installed packages (`node_modules`) are left
//! out, and an ASAR is unpacked first with `core_archive_extract`. Reading every dependency made a
//! small app's record 30 times larger and a real one could pass REA's response budget.
//!
//! REA's own unknowns are not this check's unknowns: a rewrite pairs with the original only where
//! the surface matches, so the rest of REA's comparison stays a note, never a reason the build
//! can't be good enough. Channels whose names are computed at run time aren't compared either.

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
/// The builder's own list of surface differences it made on purpose, in app/.
pub const WAIVERS_FILE: &str = "rea-surface-waivers.json";
/// The waivers file's format version.
pub const WAIVERS_VERSION: u32 = 1;
/// Most claims one check makes: a big app's surface is summarised, not listed in full.
const MAX_CLAIMS: usize = 40;

/// Folders never staged for REA: installed packages and caches, not the app's own code.
const SKIP_DIRS: &[&str] = &["node_modules", "bower_components", "jspm_packages", ".git", ".cache", ".parcel-cache", "coverage"];
/// The files REA reads to map a JavaScript app.
const SOURCE_EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "jsx", "ts", "tsx", "mts", "cts", "html", "htm", "json", "map"];
/// Staging limits: files and bytes copied, entries looked at, folder depth.
const STAGE_MAX_FILES: usize = 5_000;
const STAGE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const STAGE_MAX_EXAMINED: usize = 200_000;
const STAGE_MAX_DEPTH: usize = 40;

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
/// a preload exposes to its pages (`api.member`), and how many windows it opens. Channels and APIs
/// whose names are computed at run time can't be compared by name: they're only counted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Surface {
    pub channels: BTreeSet<String>,
    pub bridge: BTreeSet<String>,
    pub windows: usize,
    /// IPC channels and exposed APIs whose names REA couldn't read (computed at run time).
    #[serde(default)]
    pub unnamed: usize,
}

impl Surface {
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty() && self.bridge.is_empty() && self.windows == 0
    }

    /// One line for people: "2 IPC channels, 2 exposed API members, 1 window".
    pub fn describe(&self) -> String {
        let unnamed =
            if self.unnamed > 0 { format!(", and {} channel(s) or API(s) with names computed at run time, not compared", self.unnamed) } else { String::new() };
        format!("{} IPC channel(s), {} exposed API member(s), {} window(s){unnamed}", self.channels.len(), self.bridge.len(), self.windows)
    }
}

/// A name REA read from the code itself (a literal), not one computed at run time.
fn literal_name<'a>(o: &'a Value, name_key: &str) -> Option<&'a str> {
    let props = o.get("properties")?;
    let resolution = props.get("resolution").and_then(Value::as_str);
    if resolution.is_some_and(|r| r != "literal") {
        return None;
    }
    props.get(name_key).and_then(Value::as_str).filter(|n| !n.is_empty())
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
                    match literal_name(o, "channel") {
                        Some(c) => {
                            s.channels.insert(c.to_string());
                        }
                        None => s.unnamed = s.unnamed.saturating_add(1),
                    }
                }
            }
            "context-bridge-api" => {
                for o in &observations {
                    let computed = o.pointer("/properties/api_key_expression").is_some_and(|e| !e.is_null());
                    let api = if computed { None } else { o.pointer("/properties/api_key").and_then(Value::as_str) };
                    let Some(api) = api.filter(|a| !a.is_empty()) else {
                        s.unnamed = s.unnamed.saturating_add(1);
                        continue;
                    };
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
            "browser-window" => s.windows = s.windows.saturating_add(1),
            _ => {}
        }
    }
    s
}

/// A difference from the original's surface the builder made on purpose, with its reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiver {
    /// The claim's title as the check names it, e.g. "IPC channel notes:copy".
    pub part: String,
    /// Why the build does it another way. A waiver without a reason doesn't count.
    pub reason: String,
}

/// `app/rea-surface-waivers.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waivers {
    pub version: u32,
    #[serde(default)]
    pub waivers: Vec<Waiver>,
}

/// The builder's waivers, if it wrote any; a file that can't be read is said in the note.
fn read_waivers(ws: &Workspace) -> Result<Vec<Waiver>, String> {
    let path = ws.app_dir().join(WAIVERS_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("app/{WAIVERS_FILE} couldn't be read ({e})")),
    };
    if bytes.len() > 1024 * 1024 {
        return Err(format!("app/{WAIVERS_FILE} is over 1 MB, so it was ignored"));
    }
    let w: Waivers = serde_json::from_slice(&bytes).map_err(|e| format!("app/{WAIVERS_FILE} isn't valid ({e})"))?;
    if w.version != WAIVERS_VERSION {
        return Err(format!("app/{WAIVERS_FILE} has version {}, and this check reads version {WAIVERS_VERSION}", w.version));
    }
    Ok(w.waivers.into_iter().filter(|w| !w.reason.trim().is_empty()).collect())
}

/// Each part of the original's surface, as a claim that the build has it too. A part the builder
/// waived with a reason holds, and says so.
pub fn surface_claims(original: &Surface, built: &Surface, waivers: &[Waiver]) -> Vec<Claim> {
    let mut claims = Vec::new();
    let mut claim = |title: String, holds: bool, detail: String| {
        let waived = (!holds).then(|| waivers.iter().find(|w| w.part.trim() == title)).flatten();
        match waived {
            Some(w) => claims.push(Claim { title: format!("{title} (changed on purpose: {})", w.reason.trim()), passed: true, detail: String::new() }),
            None => claims.push(Claim::new(title, holds, detail)),
        }
    };
    for c in &original.channels {
        claim(
            format!("IPC channel {c}"),
            built.channels.contains(c),
            format!("the original's main and page code talk over the IPC channel \"{c}\"; the build has no such channel"),
        );
    }
    for m in &original.bridge {
        claim(format!("Exposed API {m}"), built.bridge.contains(m), format!("the original's preload gives its pages {m}; the build's preload doesn't"));
    }
    if original.windows > 0 {
        claim(
            "Windows".to_string(),
            built.windows >= original.windows,
            format!("the original opens {} window(s); REA found {} in the build", original.windows, built.windows),
        );
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

/// What staging copied for REA.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Staged {
    pub files: usize,
    pub bytes: u64,
    /// A limit stopped the copy, so REA saw only part of the sources.
    pub capped: bool,
}

/// Copy the app's own sources under `root` into `dest` (made fresh): the files REA reads, without
/// installed packages or caches, following no links. Bounded by files, bytes, entries looked at and
/// depth; `capped` says when a limit was reached.
pub fn copy_own_sources(root: &Path, dest: &Path) -> Result<Staged, ToolError> {
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|e| ToolError::io(dest.display(), e))?;
    }
    std::fs::create_dir_all(dest).map_err(|e| ToolError::io(dest.display(), e))?;
    let mut staged = Staged::default();
    let mut examined = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries {
            examined = examined.saturating_add(1);
            if examined > STAGE_MAX_EXAMINED || staged.files >= STAGE_MAX_FILES {
                staged.capped = true;
                return Ok(staged);
            }
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            let name = entry.file_name().to_string_lossy().to_string();
            if meta.is_dir() {
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                if depth >= STAGE_MAX_DEPTH {
                    staged.capped = true;
                    continue;
                }
                stack.push((path, depth.saturating_add(1)));
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if !SOURCE_EXTENSIONS.contains(&ext.as_str()) {
                continue;
            }
            if staged.bytes.saturating_add(meta.len()) > STAGE_MAX_BYTES {
                staged.capped = true;
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else { continue };
            let to = dest.join(rel);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ToolError::io(parent.display(), e))?;
            }
            std::fs::copy(&path, &to).map_err(|e| ToolError::io(path.display(), e))?;
            staged.files = staged.files.saturating_add(1);
            staged.bytes = staged.bytes.saturating_add(meta.len());
        }
    }
    Ok(staged)
}

/// Where an app's JavaScript is: an `.asar` given directly, a packaged app's `resources/app.asar`,
/// or the folder itself.
fn js_root(input: &Path) -> PathBuf {
    if input.extension().is_some_and(|e| e.eq_ignore_ascii_case("asar")) {
        return input.to_path_buf();
    }
    let asar = input.join("resources").join("app.asar");
    if asar.is_file() { asar } else { input.to_path_buf() }
}

/// Stage `input`'s own sources into `dest` for REA, unpacking an ASAR first with Craft's
/// `core_archive_extract` (through `call`), then run REA's map on the copy. Returns the record and
/// what was staged.
fn analyze_staged(call: &dyn Fn(&str, Value) -> Result<ToolOutput, ToolError>, input: &Path, dest: &Path) -> Result<(Value, Staged), ToolError> {
    let root = js_root(input);
    let source = if root.is_file() {
        let out = call("core_archive_extract", json!({"path": root.display().to_string()}))?;
        let unpacked = match &out {
            ToolOutput::Evidence(e) => e.result.get("output_directory").and_then(Value::as_str).map(PathBuf::from),
            ToolOutput::Value(v) => v.get("output_directory").and_then(Value::as_str).map(PathBuf::from),
        };
        unpacked.ok_or_else(|| ToolError::internal("core_archive_extract named no folder"))?
    } else {
        root
    };
    let staged = copy_own_sources(&source, dest)?;
    let out = call(ANALYZE_JS, json!({"input_path": dest.display().to_string(), "detail": "complete"}))?;
    Ok((record_of(&out), staged))
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
        let out_dir = cx.ws.checks_dir().join(format!("round-{}", cx.round)).join("rea");
        let call = |tool: &str, params: Value| cx.call(tool, params);
        let (record, staged) = match analyze_staged(&call, &cx.ws.app_dir(), &out_dir.join("source")) {
            Ok(r) => r,
            Err(e) if e.code == ErrorCode::Cancelled || e.code == ErrorCode::TimedOut => return Err(e),
            // REA couldn't read the build: say so, without holding the build back for it.
            Err(e) => {
                return Ok(CheckResult::new(
                    self.id(),
                    cx.round,
                    true,
                    format!("REA couldn't read the build's JavaScript, so its surface wasn't compared: {}", e.message),
                ));
            }
        };
        let built = surface_of(&record);
        let mut artifacts = Vec::new();
        let file = out_dir.join("surface.json");
        let both = json!({"original": saved.surface, "build": built, "staged_files": staged.files, "staging_capped": staged.capped});
        if craft_anything_engine::project::write_atomic(&file, serde_json::to_string_pretty(&both).unwrap_or_default().as_bytes()).is_ok()
            && let Ok(rel) = file.strip_prefix(&cx.ws.root)
        {
            artifacts.push(rel.to_path_buf());
        }
        let (waivers, waiver_problem) = match read_waivers(cx.ws) {
            Ok(w) => (w, None),
            Err(p) => (Vec::new(), Some(p)),
        };
        let mut claims = surface_claims(&saved.surface, &built, &waivers);
        let mut notes = Vec::new();
        if staged.capped {
            // Part of the build wasn't read, so a part not found there isn't proof it's missing.
            for c in claims.iter_mut().filter(|c| !c.passed) {
                notes.push(format!("not found in the part REA read: {}", c.title));
                c.passed = true;
                c.detail.clear();
            }
            notes.insert(0, format!("app/ has more sources than REA reads at once ({} files staged), so missing parts aren't counted", staged.files));
        }
        if let Some(p) = waiver_problem {
            notes.push(p);
        }
        let failed: Vec<String> = claims.iter().filter(|c| !c.passed).map(|c| c.detail.clone()).collect();
        let mut note = if failed.is_empty() {
            format!("REA finds the original's surface in the build ({}).", saved.surface.describe())
        } else {
            format!(
                "REA compared what the app shows of itself (IPC channels, the APIs its preload exposes, its windows) and {} of {} parts of the original's have no counterpart in the build: {}. Add them with the same names. If the build does one another way on purpose, list it in app/{WAIVERS_FILE} as {{\"version\": {WAIVERS_VERSION}, \"waivers\": [{{\"part\": \"<the part's name as written here, e.g. IPC channel notes:copy>\", \"reason\": \"<why>\"}}]}}. The original's surface is in capture/rea/surface.json; the build's in checks/round-{}/rea/surface.json.",
                failed.len(),
                claims.len(),
                failed.join("; "),
                cx.round
            )
        };
        if !notes.is_empty() {
            note.push_str(&format!(" ({})", notes.join("; ")));
        }
        let mut r = CheckResult::new(self.id(), cx.round, true, note);
        r.claims = claims;
        r.artifacts = artifacts;
        Ok(r)
    }
}

impl Worker {
    /// Study with REA: its first look at the app's folder and, for a JavaScript app, its map of
    /// the app's own code. Records go to capture/rea/, the original's surface to
    /// capture/rea/surface.json, and the notes section comes back. A REA failure is said in the notes
    /// and the study goes on.
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
            let analyzed = {
                let me = &*self;
                let call = |tool: &str, params: Value| me.call(tool, params);
                analyze_staged(&call, input, &dir.join("source"))
            };
            match analyzed {
                Ok((record, staged)) => {
                    keep("javascript.json", &record)?;
                    let surface = surface_of(&record);
                    keep(SURFACE_FILE, &serde_json::to_value(SavedSurface { js_input: input.to_path_buf(), surface: surface.clone() }).unwrap_or(Value::Null))?;
                    let capped = if staged.capped { " A limit stopped the copy, so REA saw only part of it." } else { "" };
                    notes.push_str(&format!(
                        "\n### Its JavaScript (capture/rea/javascript.json)\n\nREA read the app's own sources ({} files, without installed packages; copied to capture/rea/source/).{capped}\n\nWhat it shows of itself: {}.\n- IPC channels: {}\n- Exposed to its pages: {}\n\nAfter each build REA reads app/ the same way and checks that this surface is there.\n\n{}",
                        staged.files,
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
        let mut nodes: Vec<Value> = channels
            .iter()
            .map(|c| json!({"kind": "ipc-channel", "observations": [{"label": c, "properties": {"channel": c, "side": "main", "resolution": "literal"}}]}))
            .collect();
        nodes.push(json!({"kind": "context-bridge-api", "observations": [{"label": api, "properties": {"api_key": api, "api_key_expression": null, "members": members}}]}));
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
        assert_eq!((s.windows, s.unnamed), (1, 0));
        assert!(surface_of(&json!({})).is_empty(), "a record without a graph has no surface");
    }

    /// REA's own shape for `const ch = 'notes:' + x; ipcMain.handle(ch, …)`: no channel, the variable as
    /// label. Taking the label made a claim for a channel called "ch" that no build could meet.
    #[test]
    fn a_channel_named_at_run_time_is_counted_not_claimed() {
        let mut rec = record(&["notes:copy"], "notes", &["copy"], 1);
        let dynamic = json!({"kind": "ipc-channel", "observations": [{"label": "ch", "properties": {"channel": null, "resolution": "dynamic", "channel_expression": "ch"}}]});
        let computed_api = json!({"kind": "context-bridge-api", "observations": [{"label": "api", "properties": {"api_key": null, "api_key_expression": "name", "members": ["x"]}}]});
        if let Some(nodes) = rec.pointer_mut("/normalized_result/graph/nodes").and_then(Value::as_array_mut) {
            nodes.push(dynamic);
            nodes.push(computed_api);
        }
        let s = surface_of(&rec);
        assert_eq!(s.channels.iter().map(String::as_str).collect::<Vec<_>>(), ["notes:copy"]);
        assert_eq!(s.bridge.iter().map(String::as_str).collect::<Vec<_>>(), ["notes.copy"]);
        assert_eq!(s.unnamed, 2);
        assert!(s.describe().contains("2 channel(s) or API(s) with names computed at run time"));
        assert!(surface_claims(&s, &Surface::default(), &[]).iter().all(|c| !c.title.contains("ch ") && !c.title.ends_with(" ch")));
    }

    #[test]
    fn a_missing_part_of_the_surface_is_a_failed_claim() {
        let original = surface_of(&record(&["notes:search", "notes:copy"], "notes", &["copy", "search"], 1));
        let same = surface_claims(&original, &original, &[]);
        assert_eq!(same.len(), 5);
        assert!(same.iter().all(|c| c.passed));
        let built = surface_of(&record(&["notes:search"], "notes", &["search"], 1));
        let claims = surface_claims(&original, &built, &[]);
        let failed: Vec<&str> = claims.iter().filter(|c| !c.passed).map(|c| c.title.as_str()).collect();
        assert_eq!(failed, ["IPC channel notes:copy", "Exposed API notes.copy"]);
        assert!(claims.iter().filter(|c| !c.passed).all(|c| !c.detail.is_empty()));
    }

    #[test]
    fn a_waived_part_holds_only_with_a_reason() {
        let original = surface_of(&record(&["notes:search", "notes:copy"], "notes", &["copy", "search"], 1));
        let built = surface_of(&record(&["notes:search"], "notes", &["search"], 1));
        let waivers = [Waiver { part: "IPC channel notes:copy".into(), reason: "copying goes through the page's clipboard API".into() }];
        let claims = surface_claims(&original, &built, &waivers);
        let failed: Vec<&str> = claims.iter().filter(|c| !c.passed).map(|c| c.title.as_str()).collect();
        assert_eq!(failed, ["Exposed API notes.copy"]);
        assert!(claims.iter().any(|c| c.passed && c.title.contains("changed on purpose: copying goes through")));

        let d = tempfile::tempdir().unwrap();
        let ws = Workspace::create(d.path(), "Notes", "Recreate this app", TargetRef::None).unwrap();
        std::fs::create_dir_all(ws.app_dir()).unwrap();
        let file = ws.app_dir().join(WAIVERS_FILE);
        std::fs::write(
            &file,
            r#"{"version": 1, "waivers": [{"part": "IPC channel notes:copy", "reason": "  "}, {"part": "Windows", "reason": "one window"}]}"#,
        )
        .unwrap();
        let read = read_waivers(&ws).unwrap();
        assert_eq!(read.len(), 1, "a waiver without a reason doesn't count");
        std::fs::write(&file, r#"{"version": 2, "waivers": []}"#).unwrap();
        assert!(read_waivers(&ws).unwrap_err().contains("version 2"));
        std::fs::write(&file, "not json").unwrap();
        assert!(read_waivers(&ws).unwrap_err().contains("isn't valid"));
        std::fs::remove_file(&file).unwrap();
        assert!(read_waivers(&ws).unwrap().is_empty(), "no file, no waivers");
    }

    #[test]
    fn staging_copies_own_sources_and_leaves_packages_out() {
        let src = tempfile::tempdir().unwrap();
        let root = src.path();
        std::fs::write(root.join("package.json"), "{}").unwrap();
        std::fs::write(root.join("main.js"), "1").unwrap();
        std::fs::write(root.join("logo.png"), "png").unwrap();
        std::fs::create_dir_all(root.join("src/ui")).unwrap();
        std::fs::write(root.join("src/ui/page.tsx"), "1").unwrap();
        for skip in ["node_modules/dep", ".git/objects", "coverage"] {
            std::fs::create_dir_all(root.join(skip)).unwrap();
            std::fs::write(root.join(skip).join("x.js"), "1").unwrap();
        }
        let out = tempfile::tempdir().unwrap();
        let dest = out.path().join("staged");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("stale.js"), "old").unwrap();
        let staged = copy_own_sources(root, &dest).unwrap();
        assert_eq!((staged.files, staged.capped), (3, false));
        assert!(dest.join("main.js").is_file() && dest.join("src/ui/page.tsx").is_file() && dest.join("package.json").is_file());
        assert!(!dest.join("logo.png").exists() && !dest.join("node_modules").exists() && !dest.join(".git").exists());
        assert!(!dest.join("stale.js").exists(), "a fresh copy each time");
    }

    #[test]
    fn staging_stops_at_its_file_limit_and_says_so() {
        let src = tempfile::tempdir().unwrap();
        for i in 0..(STAGE_MAX_FILES + 5) {
            std::fs::write(src.path().join(format!("m{i}.js")), "1").unwrap();
        }
        let out = tempfile::tempdir().unwrap();
        let staged = copy_own_sources(src.path(), &out.path().join("s")).unwrap();
        assert!(staged.capped);
        assert_eq!(staged.files, STAGE_MAX_FILES);
    }

    #[test]
    fn a_packaged_app_is_read_from_its_asar() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(js_root(d.path()), d.path());
        std::fs::create_dir_all(d.path().join("resources")).unwrap();
        std::fs::write(d.path().join("resources/app.asar"), "x").unwrap();
        assert_eq!(js_root(d.path()), d.path().join("resources/app.asar"));
        assert_eq!(js_root(&d.path().join("x.asar")), d.path().join("x.asar"));
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
