# Content Browser and asset identity

Editor roadmap: turn the one-folder file list into a Content Browser with
the core every engine shares (Unreal Content Browser, Unity Project window,
Godot FileSystem dock), plus Unreal's Ctrl+Space Content Drawer.

## Intent

Every game is built by putting, finding, renaming and moving many asset files.
Today the browser lists one folder, and a scene names a model only by its
path, so renaming a file breaks every scene that uses it. The goal is a
generic browser any game needs, not engine-specific extras. Where there is a
choice, pick the design that still works when textures, materials, sounds and
prefabs become assets (north-star rule).

## Research summary

All three engines share these features: a folder tree plus an asset view
(grid or list), search by name, a type filter, and file operations done
inside the editor (create folder, rename, move, duplicate, delete with a
reference warning). They also share drag-in from the OS, drag into the scene
or a property field, refresh when files change on disk, thumbnails, asset
details on select, and asset ids that survive a rename or move (Unity
`.meta`, Godot `.import` / `.uid`; Unreal uses path redirectors instead).
Skipped as engine-specific: collections, query languages (`t:`,
`AND/OR`), Reference Viewer, Size Map, Migrate, source-control icons,
multiple browsers, Asset Store.

## Goals

1. **Identity.** Each file under `assets/` has a stable GUID in a sidecar
   `<file>.meta`. Scenes reference models by GUID, with the path kept as a
   fallback. Rename or move in the editor never breaks a reference.
2. **Browser layout.** Folder tree on the left. Asset view on the right
   (tiles, with a size slider whose smallest step is a list). Clickable path
   bar, Back / Forward, name search, type filter, Favorites.
3. **File operations.** New Folder, Rename (F2), Duplicate (Ctrl+D), Delete
   (Del, with a confirmation that lists referencing scenes, to the Recycle
   Bin), Show in Explorer, drag onto a folder to move, drag files in from
   the OS to copy them into the current folder.
4. **Live updates and the Inspector.** A file watcher refreshes the browser
   and the GUID index. The Inspector Mesh field takes a drop from the
   browser; clicking it highlights the asset in the browser. Selecting a
   file in the browser shows its details in the Inspector.
5. **Thumbnails.** Model tiles show a rendered thumbnail; other types show a
   type icon.
6. **Content Drawer.** Ctrl+Space (and a status-bar button) toggles the same
   browser as a panel that slides up from the status bar over the dock area.
   It hides on Esc or a click outside it. "Dock in Layout" opens the Content
   Browser tab and closes the drawer.

## Non-goals

- Import settings or an import step. Files load as they are (glTF loads
  directly); "import" means copying a file into `assets/`.
- New asset types (textures, materials, sounds as `Handle<T>`). They get
  `.meta` files now; their handles come with their own features.
- Creating assets from the browser (materials, scenes). The right-click menu
  has New Folder only until there are creatable asset types.
- Fixing references inside scene files that are not open (the GUID makes
  this unnecessary). Undo of file operations.
- The features listed as skipped in the research summary.

## Anti-goals

- A file operation that loses data: deletes go to the Recycle Bin, a move
  never overwrites an existing file, a failed operation leaves both the
  file and its `.meta` where they were.
- Scene files that can no longer be written by hand or by an agent. A scene
  with only a path (no GUID) must still load. This keeps the asset-handles
  design's anti-goal ("scene files that store ids instead of paths"): scenes
  store the path and add the GUID.
- Two assets that share a GUID.
- `.meta` files shown in the browser or offered as assets.
- The editor blocking (frozen UI) during a scan, watch event or thumbnail
  render.

## Constraints

- Dependency direction stays `core` <- `render`, `physics` <- `editor` <-
  `host`. `melon_game` depends only on `melon_core`.
- `melon_core` only reads `.meta` files (`scan_metas`); writing them is
  editor code (`asset_db`, `asset_ops`, watcher). The editor is in every
  build today (`just ship` only drops `hot-reload`), so the editor writes
  `.meta` files in every build; a future editor-less game runtime gets the
  read-only path for free. (My call, builder check.)
- `.meta` files are committed with their assets (not git-ignored).
- New crates go in `[workspace.dependencies]`.
- Colours and sizes from `theme::colors` / `theme::sizes`. Every menu item
  does something (no placeholders).
- Never call egui `Response` / `Context` methods inside `ctx.input` closures.
- Scene versions 1-3 keep loading.

## Approaches

| Decision | Options | Picked |
|---|---|---|
| Asset identity | **A GUID in a `.meta` sidecar (Unity, Godot 4.4)**; B paths, with the editor rewriting scene files on rename (Godot before 4.4); C path redirectors (Unreal) | **A** (user): survives moves inside the editor and, if the `.meta` moves with it, outside; B breaks on any rename outside the editor and must rewrite every scene on each move; C leaves redirector files and still breaks outside the editor |
| Drawer | **Docked tab + Ctrl+Space drawer**; docked tab only | **Drawer** (user) |
| What the scene stores | path only; GUID only; **GUID + path** | My call: GUID wins, path is the fallback and keeps files hand-writable |
| GUID source | **`uuid` crate, v4**; hash of path; counter | My call: random v4 cannot collide on copy and does not depend on the path |
| Which files get a `.meta` | models only; **every file under `assets/`** except `.meta` and dot-files; files and folders | My call: textures, shaders and future types get ids before their handles exist; nothing references folders |
| Change detection | **`notify` crate watcher**; keep polling | My call: the browser must see adds, removes and renames, not only modified times on known files. Asset hot reload keeps its poll. |
| Delete | **Recycle Bin (`trash` crate)**; permanent delete | My call: no data loss |
| Thumbnails | disk cache `.melon/thumbnails/<guid>.png`; **in-memory GPU textures, rendered on demand** | My call: no GPU readback or PNG encode; re-rendered once per session per visible model. Disk cache can follow if startup cost shows. |
| Drawer state | separate browser instance; **one shared `ContentBrowser`** | My call: same folder, selection and history wherever it is shown |

## Design

### M1: Identity

**`melon_core::assets`**

- `AssetGuid`: 128-bit id (`uuid::Uuid` inside), `Copy`, `Eq`, `Hash`,
  serialised as the hyphenated lower-case string.
- `AssetMeta { version: u32, guid: AssetGuid }`, RON, `META_VERSION = 1`,
  file name `<asset file name>.meta` beside the asset (`cube.glb.meta`).
- `AssetServer` entries keep their `u32` handle id; each entry gains an
  optional GUID. The server keeps `guid -> path` and `path -> guid` maps for
  every file under `assets/`, loaded or not.
- `AssetServer::scan_metas(&mut self)`: walks `<root>/assets`, reads every
  `.meta`, fills the maps. Read-only: no file writes. Runs in `Core` setup
  in every build. A broken `.meta` is logged and skipped.
- `AssetServer::load_guid<T>(&mut self, guid, fragment: Option<usize>) ->
  Option<Handle<T>>`: resolves the current path (plus `#mesh<N>`) and calls
  `load`.
- `AssetServer::rename_path(&mut self, old: &str, new: &str)`: moves the
  entry and every `old#mesh<N>` entry to the new path, keeping handle ids,
  so entities already in the world keep drawing. Also updates the GUID maps.
- `AssetServer::guid_of(&self, path) -> Option<AssetGuid>`.

**`melon_editor::asset_db` (new): writing `.meta` files**

- `reconcile(server, root)`: for each asset file without a `.meta`, write
  one with a new GUID. For a `.meta` whose asset is missing, delete it. For
  two `.meta` files with the same GUID, one keeps it and the other gets a
  new GUID, with a Console warning naming both. Who keeps it (my call,
  builder check; Explorer copies keep the modified time, so it cannot
  decide): first the path the `AssetServer` already maps to that GUID; on a
  cold start the asset with the older creation time; then path order.
  If a GUID's path changed (moved outside the editor with its `.meta`),
  call `AssetServer::rename_path` so loaded entities keep drawing. Then
  `scan_metas`.
- Runs at editor start and after watcher events settle (M4).
- Writes are atomic (temp file + rename), as for scenes and `input.ron`.

**Scenes (`SCENE_VERSION = 4`)**

- `SceneEntity` gains `model_guid: Option<AssetGuid>`; `model` stays (path,
  with `#mesh<N>`).
- Save: both fields (`model_guid` is `None` for `builtin:` and for files
  without a `.meta`).
- Load: if `model_guid` resolves, use its current path with the fragment from
  `model`. Otherwise use `model`. A GUID that does not resolve and a path that
  does not exist logs a warning and keeps the entity with the placeholder
  model, as today.

### M2: Browser layout

- `ContentBrowser` splits into state (current folder, selection, history,
  search, filter, view settings, favorites) and a `ui(&mut self, ui, ..)`
  that can be drawn in a dock tab or the drawer.
- Folder tree (collapsing rows, only folders), resizable split.
- Asset view: tiles with a size slider; the smallest step switches to list
  rows (name, type, size). Folders first, then files, sorted by name.
- Path bar: one button per segment.
- Back / Forward buttons plus mouse buttons 4 / 5 over the browser. History
  holds visited folders.
- Search box: case-insensitive name match in the current folder and its
  subfolders; results show their folder.
- Type filter (toggle chips, all on by default): Model (`glb`, `gltf`),
  Texture (`png`, `jpg`, `jpeg`, `hdr`), Shader (`wgsl`), Scene (`ron`),
  Other.
- Favorites: folders pinned at the top of the tree (right-click Add / Remove).
- Saved in `.melon/content_browser.ron`: favorites, view size, last folder,
  split width. A missing or broken file means the defaults.

### M3: File operations

All in `melon_editor::asset_ops` (no UI), each moving the `.meta` with the
file and calling `AssetServer::rename_path` where paths change:

- `create_folder(dir) -> PathBuf`: "New Folder", "New Folder 2", ...
- `rename(path, new_name)`: rejects empty names, path separators, and names
  that already exist.
- `move_to(paths, folder)`: refuses to overwrite and to move a folder into
  itself. Folder moves carry every `.meta` inside.
- `duplicate(path) -> PathBuf`: `name 2.ext`, with a fresh `.meta`.
- `delete(paths)`: to the Recycle Bin, with the `.meta`.
- `references(guid, path) -> Vec<PathBuf>`: `.ron` files under the project
  (skipping `target`, `.git`, `.melon`) that contain the GUID string or the
  path, plus "the open scene" when the open world uses it.
- `import(files, folder)`: copies OS files in, new names on clash
  (`name 2.ext`), and writes a `.meta` for each.

Opening files (user decision):

- Scenes live under `assets/scenes/`. File ▸ Save As starts there (created
  if missing); saving elsewhere still works.
- Double-click: a folder opens it; a scene (`.ron` under `assets/`) asks
  Save / Don't Save / Cancel, then loads it (there is no unsaved-change
  tracking yet, so it always asks); any other file opens in the OS default
  app (`opener` crate or `explorer`). Enter does the same on the selection.

UI: a right-click menu on items and empty space; F2 / Ctrl+D / Del when the
browser has keyboard focus; inline rename field; a delete confirmation
listing references; drag items onto a tree or tile folder to move them;
Show in Explorer (`explorer /select,`, other OSes open the folder). The
host forwards winit `DroppedFile` events to the editor; files dropped over
the browser go to its current folder, files dropped elsewhere are ignored.

### M4: Live updates and the Inspector

- A `notify` watcher on `<root>/assets` sends events over a channel. The
  editor drains it each frame; after 300 ms without new events it runs
  `reconcile` and refreshes the browser. Its own operations do the same
  without waiting.
- Inspector Mesh field: also a drop target for model files from the browser.
  Clicking the field's "show" button selects the asset in the browser,
  opens its folder, scrolls to it and flashes it for a second.
- Selecting a file in the browser (with no entity selected, or via an
  Inspector "Asset" header when one is) shows: name, path, type, size,
  modified time, GUID; for models: mesh count and bounds from the
  `AssetServer`.

### M5: Thumbnails

- `melon_render::ThumbnailRenderer` renders one model at a time into a small
  target (128 x 128) with a fixed three-quarter camera fitted to the model's
  bounds and the default light, then copies the result into a texture the
  editor registers with egui.
- At most one thumbnail per frame, only for tiles on screen. Rebuilt when
  the model's `AssetServer` version changes. Until it exists, the tile
  shows the type icon.

### M6: Content Drawer

- Ctrl+Space and a status-bar "Content Drawer" button toggle it. It slides
  up (about 150 ms) from the status bar, 40 % of the window height, full
  width, above the dock area.
- Hides on Esc, Ctrl+Space, or a click outside it. Drag-and-drop from the
  drawer to the Scene viewport hides it when the drag starts, so the drop
  target is visible.
- Title bar button "Dock in Layout": opens (or focuses) the Content Browser
  tab and hides the drawer.
- Ctrl+Space works only in Editor focus (not while the game owns input).
- While the Content Browser tab is visible, Ctrl+Space and the button focus
  that tab and flash it instead of opening the drawer, so one
  `ContentBrowser` is never drawn twice in a frame (my call, builder check).

## Tests (headless)

- GUID and `.meta`: round-trip RON; `reconcile` writes missing metas, removes
  orphans, splits duplicates (older keeps its GUID); `scan_metas` skips a
  broken `.meta`; ship-style scan writes nothing.
- `AssetServer::rename_path` keeps the handle id of the file and of its
  `#mesh<N>` entries.
- Scene v4: GUID wins over a stale path; path-only entity loads; unknown
  GUID with a valid path loads by path; v3 file loads.
- `asset_ops`: each operation moves or creates the `.meta`; rename rejects
  clashes and separators; move refuses overwrite and folder-into-itself;
  duplicate names; references finds a GUID and a path.
- Browser state: history back / forward; search covers subfolders; filter;
  `.meta` never listed; favorites round-trip.

## Review focus

- A file moved outside the editor while the editor runs: the watcher sees
  the asset and its `.meta` arrive in separate events. `reconcile` must wait
  for the quiet period so it does not delete the "orphan" `.meta` and give
  the file a new GUID.
- A file copied with its `.meta` in Explorer: the duplicate GUID must go to
  the copy, not the original that scenes reference.
- Renaming a model that is in the open scene: the entities keep drawing,
  and the next save writes the new path with the same GUID.
- Moving a folder that contains models: every `.meta` inside moves and every
  path in the `AssetServer` is updated.
- Drag from the drawer into the Scene: the drawer hides and the drop lands.

## Left to the builder

- Exact widget layout, icons and spacing inside the theme.
- Internal structure of `ContentBrowser` state and the watcher plumbing.
- How the thumbnail target is shared or pooled.
- Milestone plans: one plan per milestone, in order M1 to M6. M2 and M6 are
  UI-only; M1 and M3 carry most of the risk.
