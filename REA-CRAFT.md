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
   # env = { GHIDRA_INSTALL_DIR = "<Ghidra 12.1+>" }   # para que REA descompile con Ghidra

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
  suyo. El Ghidra que instala Craft se le puede pasar a REA con `GHIDRA_INSTALL_DIR`.
- La verificación `rea-surface` exige los mismos nombres de canales y APIs que el original. Si una
  copia los cambia a propósito, el constructor lo puede explicar en su respuesta, pero la
  verificación seguirá marcándolos.
- Cada llamada por CLI arranca REA de nuevo; dentro de la app o del servidor MCP, REA queda
  abierto durante la sesión.

Nota: las reglas del proyecto original piden no nombrar otros productos de ingeniería inversa en
el código y la documentación; este fork lo hace a propósito porque su objetivo es integrar REA.
Estos cambios no se pueden proponer tal cual al repositorio original.
