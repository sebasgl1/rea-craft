#!/usr/bin/env python3
"""REA-Craft live test: REA inside Craft Anything, with Claude Code as the builder.

Run it from the folder where you unpacked a REA-Craft release (craft-anything-cli is next to it),
on Windows or Linux, with the Python you already have (standard library only):

    python rea-craft-test.py

It checks what's needed (Node.js, REA, Claude Code), adds the REA plugin to Craft Anything's
config.toml if it isn't there (after a backup), makes a small Electron notes app, asks Claude Code
to rebuild it in Deep mode with REA studying and checking, and writes rea-craft-test-report.md
with what happened. Send that report back. It asks before installing anything.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
WINDOWS = os.name == "nt"
REPORT = HERE / "rea-craft-test-report.md"
lines = []


def say(text=""):
    print(text)
    lines.append(text)


def find_cli():
    name = "craft-anything-cli.exe" if WINDOWS else "craft-anything-cli"
    local = HERE / name
    if local.is_file():
        return str(local)
    return shutil.which("craft-anything-cli")


def run(cmd, timeout=600, check=False):
    """Run a command, return (code, stdout, stderr). Never waits for input."""
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL, encoding="utf-8", errors="replace")
    except FileNotFoundError:
        return 127, "", f"{cmd[0]} not found"
    except subprocess.TimeoutExpired:
        return 124, "", f"timed out after {timeout} s"
    if check and p.returncode != 0:
        said = (p.stdout.strip() + "\n" + p.stderr.strip()).strip()[-1500:]
        say(f"    falló: {' '.join(cmd)}\n    {said}")
    return p.returncode, p.stdout, p.stderr


def tool(name):
    """A program on PATH; on Windows also its .cmd shim (npm installs those)."""
    found = shutil.which(name)
    if not found and WINDOWS:
        found = shutil.which(name + ".cmd")
    return found


def ask(question):
    try:
        return input(f"{question} [s/N] ").strip().lower() in ("s", "si", "sí", "y", "yes")
    except EOFError:
        return False


SAMPLE = {
    "package.json": json.dumps({"name": "notes-demo", "version": "1.0.0", "main": "main.js", "devDependencies": {"electron": "^33.0.0"}}, indent=2),
    "main.js": """const { app, BrowserWindow, ipcMain, clipboard } = require('electron');
const path = require('path');
const { search } = require('./src/search');
function create() {
  const w = new BrowserWindow({ width: 600, height: 400, webPreferences: { preload: path.join(__dirname, 'preload.js'), contextIsolation: true } });
  w.loadFile('index.html');
}
ipcMain.handle('notes:search', (_e, q) => search(q));
ipcMain.handle('notes:copy', (_e, t) => { clipboard.writeText(t); return true; });
app.whenReady().then(create);
app.on('window-all-closed', () => app.quit());
""",
    "preload.js": """const { contextBridge, ipcRenderer } = require('electron');
contextBridge.exposeInMainWorld('notes', {
  search: (q) => ipcRenderer.invoke('notes:search', q),
  copy: (t) => ipcRenderer.invoke('notes:copy', t),
});
""",
    "src/search.js": """const notes = [
  { id: 1, title: 'Shopping', body: 'milk eggs bread' },
  { id: 2, title: 'Work', body: 'ship the release' },
  { id: 3, title: 'Ideas', body: 'a notes app with search' },
];
function score(n, q) {
  const t = (n.title + ' ' + n.body).toLowerCase();
  if (!t.includes(q.toLowerCase())) return 0;
  return n.title.toLowerCase().includes(q.toLowerCase()) ? 2 : 1;
}
exports.search = (q) => notes.map((n) => ({ n, s: score(n, q) })).filter((x) => x.s > 0).sort((a, b) => b.s - a.s).map((x) => x.n);
""",
    "index.html": """<!doctype html><meta charset="utf-8"><title>Notes</title>
<input id="q" placeholder="Search notes"><ul id="r"></ul>
<script>
q.oninput = async () => {
  const found = await notes.search(q.value);
  r.innerHTML = found.map((n) => `<li>${n.title} <button data-t="${n.body}">Copy</button></li>`).join('');
  r.querySelectorAll('button').forEach((b) => (b.onclick = () => notes.copy(b.dataset.t)));
};
</script>
""",
}

ASK_TOOLS = ["*adb*", "*frida*", "*gdb*", "*rizin*", "*cutter*", "*build_computer*", "*build_capture*", "*build_task*",
             "*build_environment*", "*build_consent*", "*build_checkpoint*", "capture_*", "observe_*", "instrument_*",
             "trace_native_ui_action", "reconcile_javascript_runtime"]


def toml_str(s):
    return json.dumps(s)  # a JSON string is a valid TOML basic string


def main():
    say("# Prueba de REA-Craft")
    say(f"Fecha: {time.strftime('%Y-%m-%d %H:%M')} · Sistema: {sys.platform} · Python {sys.version.split()[0]}")
    say()

    cli = find_cli()
    if not cli:
        say("No encontré craft-anything-cli junto a este script ni en el PATH. Descomprime el Release y corre el script desde esa carpeta.")
        return 1
    code, out, _ = run([cli, "--version"])
    say(f"- craft-anything-cli: {out.strip() or 'no respondió'} ({cli})")

    node = tool("node")
    code, out, _ = run([node, "--version"]) if node else (127, "", "")
    say(f"- Node.js: {out.strip() if code == 0 else 'no está instalado'}")
    if code != 0:
        say("  REA necesita Node.js 22.23.4 o más nuevo: https://nodejs.org/")
        return 1

    npm = tool("npm")
    rea_main = None
    if npm:
        code, root, _ = run([npm, "root", "-g"])
        candidate = Path(root.strip()) / "rea-agents" / "scripts" / "rea.mjs"
        if candidate.is_file():
            rea_main = candidate
    if not rea_main:
        say("- REA (rea-agents): no está instalado")
        if npm and ask("¿Instalo REA con `npm install --global rea-agents`?"):
            code, _, err = run([npm, "install", "--global", "rea-agents"], timeout=900)
            code, root, _ = run([npm, "root", "-g"])
            candidate = Path(root.strip()) / "rea-agents" / "scripts" / "rea.mjs"
            rea_main = candidate if candidate.is_file() else None
        if not rea_main:
            say("  Sin REA no se puede seguir.")
            return 1
    code, out, _ = run([node, str(rea_main), "--version"])
    say(f"- REA: {out.strip().splitlines()[0] if out.strip() else '?'} ({rea_main})")

    claude = tool("claude")
    say(f"- Claude Code: {'encontrado' if claude else 'no está instalado'}")
    if not claude:
        say("  Instálalo e inicia sesión (`claude` en una terminal) y vuelve a correr este script.")
        return 1

    # Craft Anything's config.toml: add the REA plugin once, after a backup.
    code, out, err = run([cli, "settings-show", "--json"])
    try:
        config_path = Path(json.loads(out)["result"]["path"])
    except (ValueError, KeyError):
        say(f"No pude leer los ajustes de Craft Anything: {err.strip()[-400:]}")
        return 1
    text = config_path.read_text(encoding="utf-8") if config_path.is_file() else ""
    if 'id = "rea"' in text:
        say(f"- Plugin REA: ya estaba en {config_path}")
    else:
        backup = None
        if config_path.is_file():
            backup = config_path.with_name(f"config.toml.antes-de-rea-{time.strftime('%Y%m%d-%H%M%S')}")
            shutil.copy2(config_path, backup)
            say(f"- Respaldo de los ajustes: {backup}")
        block = (
            "\n[[plugins.mcp]]\n"
            'id = "rea"\nname = "REA"\n'
            f"command = {toml_str(node)}\n"
            f"args = [{toml_str(str(rea_main))}, \"mcp\"]\n"
            "consent = false\n"
            f"ask_tools = [{', '.join(toml_str(t) for t in ASK_TOOLS)}]\n"
            "\n[plugins.mcp.default_args]\n"
            'detail = "summary"\n'
        )
        # Craft Anything writes an empty `mcp = []` under [plugins]; a [[plugins.mcp]] table after it
        # would define the list twice, which TOML refuses, and Craft would load its defaults.
        lines = text.splitlines()
        section = None
        kept = []
        for line in lines:
            stripped = line.strip()
            if stripped.startswith("[") and not stripped.startswith("[["):
                section = stripped
            if section == "[plugins]" and stripped.replace(" ", "") == "mcp=[]":
                continue
            kept.append(line)
        text = "\n".join(kept) + ("\n" if kept else "")
        config_path.parent.mkdir(parents=True, exist_ok=True)
        with open(config_path, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(text + block)
        code, out, err = run([cli, "--format", "text", "plugins-list"])
        if code != 0 or "rea" not in out:
            if backup:
                shutil.copy2(backup, config_path)
            say(f"- No pude agregar REA a los ajustes (se restauró el respaldo): {(out + err).strip()[-800:]}")
            return 1
        say(f"- Plugin REA: agregado a {config_path}")
    run([cli, "--allow-run", "plugins-refresh"], timeout=300, check=True)
    run([cli, "settings-set", "--key", "build.study_engines", "--value", '["craft","rea"]'], check=True)

    say()
    say("## Doctor")
    code, out, err = run([cli, "--format", "text", "doctor"], timeout=300)
    for line in (out or err).splitlines():
        if line.startswith("[") and ("plugin" in line or "ai" in line or "FAIL" in line or "warn" in line):
            say(f"    {line}")

    # The sample app and the rebuild.
    work = Path(tempfile.mkdtemp(prefix="rea-craft-test-"))
    app = work / "notes-original"
    for rel, body in SAMPLE.items():
        f = app / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(body, encoding="utf-8")
    projects = work / "projects"
    say()
    say("## Recreación con Claude Code")
    say(f"App original: {app}")
    say("Esto puede tardar varios minutos…")
    started = time.time()
    code, out, err = run([cli, "rebuild", str(app), "Recreate this notes app in Electron, with search and copy",
                          "--deep", "--unattended", "--rounds", "2", "--backend", "Claude Code",
                          "--projects-dir", str(projects), "--format", "text"], timeout=3600)
    say(f"Terminó en {int(time.time() - started)} s con código {code}.")
    say()
    say("Últimas líneas:")
    say("```")
    for line in (out + err).strip().splitlines()[-25:]:
        say(line)
    say("```")

    project = next((p for p in projects.glob("*") if (p / "craft.json").is_file()), None) if projects.is_dir() else None
    if not project:
        say("No se creó el proyecto.")
        return 1
    readme = project / "capture" / "README.md"
    rea_notes = readme.is_file() and "## REA" in readme.read_text(encoding="utf-8", errors="replace")
    say()
    say("## Resultado")
    say(f"- Estudio con REA en capture/README.md: {'sí' if rea_notes else 'NO'}")
    surface = project / "capture" / "rea" / "surface.json"
    if surface.is_file():
        s = json.loads(surface.read_text(encoding="utf-8"))["surface"]
        say(f"- Superficie del original según REA: canales {s.get('channels')}, APIs {s.get('bridge')}, ventanas {s.get('windows')}")
    try:
        manifest = json.loads((project / "craft.json").read_text(encoding="utf-8"))
    except ValueError:
        manifest = {}
    say(f"- Estado final: {manifest.get('status')} — {manifest.get('note', '')}")
    for c in manifest.get("checks", []):
        failed = [cl.get("title") for cl in c.get("claims", []) if not cl.get("passed")]
        mark = "ok" if c.get("passed") and not failed else "falla"
        say(f"  - Ronda {c.get('round')} · {c.get('check')}: {mark} — {str(c.get('note', ''))[:300]}")
    waivers = project / "app" / "rea-surface-waivers.json"
    if waivers.is_file():
        say(f"- El constructor declaró diferencias: {waivers.read_text(encoding='utf-8')[:500]}")
    say()
    say(f"Proyecto completo: {project}")
    return 0


def finish(code):
    REPORT.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"\nInforme guardado en {REPORT}")
    return code


if __name__ == "__main__":
    try:
        status = main()
    except KeyboardInterrupt:
        say("Cancelado.")
        status = 130
    finish(status)
    sys.exit(status)
