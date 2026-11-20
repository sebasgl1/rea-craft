# REA-Craft

Fork de [Craft Anything](https://github.com/storytold/craft-anything) (MIT o Apache-2.0) que usa
[REA](https://github.com/morluto/rea) (`rea-agents`, MIT) como segundo motor de ingeniería inversa.
REA corre como plugin MCP de Craft Anything; los cambios de este fork hacen que el flujo de
recrear (Estudiar → Construir → Verificar → Refinar) lo use bien.

## Qué agrega este fork

1. **Plugins MCP más útiles** (sirve para cualquier plugin, no solo REA)
   - `default_args`: argumentos que se envían cuando una llamada no los trae, solo a las
     herramientas cuyo esquema los declara. Con `detail = "summary"`, una respuesta de REA bajó de
     1,17 MB a unos 17 KB.
   - Si el servidor manda la misma respuesta como texto y como `structuredContent`, el texto
     repetido se quita (antes llegaba duplicado al agente).
   - La confianza, la cobertura y las limitaciones que trae cada registro de REA pasan a los campos
     de evidencia de Craft.
   - Las instrucciones del servidor (las que manda al conectarse) se guardan con `plugins-refresh`
     y se le entregan al agente cuando activa el toolset del plugin (`enable_toolset`).
   - Un constructor por API ya no recibe en modo Deep las herramientas de los plugins desde el
     inicio (REA son 255): las activa con `enable_toolset` cuando las necesite.
   - `ask_tools`: herramientas de un plugin que siempre piden permiso aunque `consent` esté apagado
     (`*` comodín). Las que el servidor marca como destructivas también preguntan. Una construcción
     no usa herramientas que preguntan.
2. **Motor de estudio seleccionable**: `build.study_engines = ["craft", "rea"]`.
   - `["craft"]` (por defecto): como Craft Anything original.
   - `["craft", "rea"]`: estudian los dos; REA agrega una sección "## REA" a `capture/README.md` y
     guarda sus registros completos en `capture/rea/`.
   - `["rea"]`: REA estudia en lugar del primer vistazo de Craft (si REA no está listo, estudia Craft).
   - REA solo corre en modo Deep. En Clean room nunca se usa.
3. **Verificación con REA (`rea-surface`)** para apps JavaScript/Electron: después de cada
   compilación, REA lee `app/` y compara su superficie con la del original: los canales IPC, los
   miembros de las APIs que el preload expone a las páginas y las ventanas. Cada parte del original
   que falte en la copia vuelve al constructor como una afirmación fallida, con el nombre exacto.
   Corre justo después de la compilación, así que no necesita que la app arranque.
   - REA lee una copia de los archivos propios de la app, sin `node_modules` ni cachés; un `.asar`
     se desempaca primero con `core_archive_extract` de Craft.
   - Los canales o APIs cuyo nombre se calcula al ejecutar no se comparan (solo se cuentan).
   - Si la copia hace algo distinto a propósito, el constructor lo declara en
     `app/rea-surface-waivers.json` (`{"version": 1, "waivers": [{"part": "IPC channel x", "reason": "…"}]}`)
     y esa parte deja de fallar. Sin razón no cuenta.
4. **Carpeta de motores movible**: Settings → Engines → **Carpeta de motores** → **Mover…** lleva
   Ghidra, su Java, los demás motores y la caché de análisis de Ghidra a otra carpeta o disco (por
   ejemplo uno que no te importe desgastar), y se usa desde entonces. En el mismo disco se renombra;
   entre discos se copia, se guarda el ajuste y se borra la carpeta vieja. Desde la CLI:
   `craft-anything-cli engines-store-move --to "E:\Craft Anything\engines"`. Los proyectos (lo que
   más escribe) se cambian en Settings → General → *Los proyectos nuevos van en*.

## Prueba rápida en tu PC

En la carpeta del Release: `python rea-craft-test.py` (Windows o Linux, sin instalar nada de Python).
Revisa Node.js, REA y Claude Code (pregunta antes de instalar REA), agrega el plugin a `config.toml`
con respaldo, crea una app Electron de notas y le pide a Claude Code recrearla en modo Deep con REA.
Deja el informe en `rea-craft-test-report.md`.

`craft-anything-cli doctor` también revisa cada plugin: si su programa existe, si sus herramientas se
leyeron y si arranca y responde; y avisa si `build.study_engines` nombra a REA sin el plugin listo.

En la app de escritorio: Settings → Builds (Construcción) → **Motor de estudio** (Integrado / Integrado + REA /
Solo REA).

## Instalación

1. Node.js 22.23.4+ (o 24.22.1+), y REA: `npm install --global rea-agents`
2. En el `config.toml` de Craft Anything (Linux: `~/.config/craft-anything/config.toml`;
   Windows: `%APPDATA%\craft-anything\config.toml`; `craft-anything-cli settings-show` lo dice):

   ```toml
   [[plugins.mcp]]
   id = "rea"
   name = "REA"
   command = "node"
   args = ["<carpeta global de npm>/node_modules/rea-agents/scripts/rea.mjs", "mcp"]
   consent = false            # una construcción no corre herramientas que piden permiso
   # El Ghidra (y su Java) de Craft Anything se le pasan a REA solos; para otro, ponlo aquí:
   # env = { GHIDRA_INSTALL_DIR = "<Ghidra 12.1+>" }

   # Estas herramientas de REA corren programas, depuradores o cambian un celular: siempre
   # preguntan, y una construcción no las usa. Las que REA marca como destructivas también preguntan.
   ask_tools = ["*adb*", "*frida*", "*gdb*", "*rizin*", "*cutter*", "*build_computer*",
                "*build_capture*", "*build_task*", "*build_environment*", "*build_consent*",
                "*build_checkpoint*", "capture_*", "observe_*", "instrument_*",
                "trace_native_ui_action", "reconcile_javascript_runtime"]

   [plugins.mcp.default_args]
   detail = "summary"
   ```

   `npm root -g` dice cuál es la carpeta global de npm.
3. `craft-anything-cli --allow-run plugins-refresh` (lee las herramientas de REA y sus instrucciones)
4. `craft-anything-cli settings-set --key build.study_engines --value '["craft","rea"]'`
5. Recrear en modo Deep, por ejemplo:
   `craft-anything-cli rebuild /ruta/a/la/app "Recrea esta app en Electron" --deep`

## Límites conocidos

- Para sitios web, el estudio sigue siendo el de Craft: las herramientas web de REA necesitan una
  conexión CDP a un navegador. El constructor igual puede usar REA con `enable_toolset`.
- REA no descompila binarios sin un motor externo (Ghidra, Hopper, IDA o Kuna); Craft sí, con el
  suyo. Si Craft tiene Ghidra (Settings → Engines, o `engines-install`), se le pasa a REA solo
  (`GHIDRA_INSTALL_DIR` y `JAVA_HOME`), salvo que el plugin ya nombre otro.
- Esta versión se probó con REA 6.9.0. `doctor` avisa si tienes otra versión, porque la verificación
  `rea-surface` lee el formato de sus resultados.
- La verificación `rea-surface` exige los mismos nombres de canales y APIs que el original, salvo
  los que el constructor declare en `app/rea-surface-waivers.json`.
- Si la app tiene más de 5.000 archivos fuente propios (o 64 MB), REA lee solo una parte y lo que no
  encuentre no cuenta como faltante.
- Cada llamada por CLI arranca REA de nuevo; dentro de la app o del servidor MCP, REA queda
  abierto durante la sesión.

Nota: las reglas del proyecto original piden no nombrar otros productos de ingeniería inversa en
el código y la documentación; este fork lo hace a propósito porque su objetivo es integrar REA.
Estos cambios no se pueden proponer tal cual al repositorio original.
