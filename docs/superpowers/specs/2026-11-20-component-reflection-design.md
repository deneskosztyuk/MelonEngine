# Component reflection and registry

Roadmap priority 1 (`docs/ROADMAP.md`).

## Intent

Adding a component today means editing `SceneEntity`, `scene_to_data`,
`spawn_scene_entity` and a hand-written Inspector card, and a game component
from `melon_game` can neither be saved nor edited. One registry of
components should drive the Inspector, scene files and (next project)
prefabs, so a new component is one derive plus one registration, and an
agent can build levels as RON data. Chosen to last (north-star rule): the
same registry later serves prefab overrides, undo diffs and animating fields.

## Goals

1. Components register once (`App::register_component::<T>()`), from the
   engine or the game plugin.
2. Scene format v5 saves every registered component of every scene entity
   by type name; versions 1–4 still load.
3. The Inspector draws any registered component from its fields, with
   custom editors kept where they are better (Transform, the Mesh field).
4. **Add Component** (searchable list of registered components that have a
   default) and **Remove** (per card) in the Inspector.

## Non-goals

- Prefabs (next project, on top of this registry).
- Undo / redo.
- Asset references other than models (`Handle<Model>`); textures,
  materials and sounds get theirs with their features.
- Reflecting resources, or editing components of the running game in a
  special way (editing works as today: in Edit mode and while paused).

## Anti-goals

- A scene that silently loses data: an unknown component name on load is
  kept and written back on save, with a Console warning (never dropped).
- Old scenes (v1–4) that stop loading.
- Runtime-only state in scene files (`PreviousTransform`, `PendingDelete`,
  `GlobalTransform`, `Children`, rapier handles).
- A hand-written Inspector card needed for a plain new component.

## Constraints

- bevy_ecs 0.19 already depends on bevy_reflect 0.19 (no new crate for the
  core; enable features as needed). bevy_reflect 0.19's `glam` support is
  glam 0.32, the engine uses glam 0.33 (rapier also pulls 0.33): reflect
  glam fields through bevy_reflect remote wrappers defined in
  `melon_core` (`#[reflect_remote(glam::Vec3)]` and friends), not by
  downgrading glam (my call). `Vec2` / `Vec3` reflect their public fields;
  `Quat` / `Vec4` (SIMD storage on x86) go through glam's `Deref` to
  `x, y, z, w` and need `#[reflect(Default)]` on the wrapper. They save in
  RON's named form `(x: 1.0, y: 2.0, z: 3.0)`, which agents read and write
  more reliably than glam's tuple form (my call, builder check).
- **ron 0.12 workspace-wide** (was 0.8), the version bevy_reflect 0.19 is
  tested against: ron 0.8's `Value` cannot hold enums or struct names, so
  unknown components could not be kept faithfully. ron 0.12's `RawValue`
  keeps them as exact text. Every existing RON file (scenes v1–4,
  `config/input.ron`, `.melon/*.ron`, `.meta` files) must still load; tests
  pin that (my call, builder check).
- Registration and the world: `ReflectComponent::insert` takes the type
  registry by reference while it mutates an entity, so the registry is
  `AppTypeRegistry` (an `Arc<RwLock<TypeRegistry>>` resource cloned out of
  the world before use), not a plain resource borrowed from the same world.
- `melon_game` derives `Reflect` and `Component` through re-exports:
  `melon_core` re-exports `bevy_reflect` and offers a small `prelude`, so the
  game crate keeps depending only on `melon_core`.
- `melon_game` depends only on `melon_core`; registration goes through the
  plugin `App`. The game plugin reloads live: re-registering a type replaces
  its entry by type path; field changes still need a restart (as today).
- Never replace `core.world`; scene load despawns in place (unchanged).
- Never call egui `Response` / `Context` methods inside `input` closures;
  colours and sizes from theme tokens; no placeholder controls.

## Approaches

| Decision | Options | Picked |
|---|---|---|
| Reflection | **A bevy_reflect** (`Reflect`, type registry, reflect serde); B own trait per component (`save` / `load` / `inspect`) | **A** (user): already compiled in through bevy_ecs; built for prefabs, undo diffs and animation later. B makes every component write its own Inspector code. |
| Add / Remove Component | now; later | **Now** (user) |
| Which components are saved | every reflected component; **only those registered with `register_component`** | My call: runtime-only components never register, so no skip list is needed. |
| glam types | downgrade to glam 0.32; **remote wrappers** | My call: keeps rapier's glam 0.33 and our math unchanged. |
| Unknown component on load | drop with warning; **keep as raw RON and write back** | My call: anti-goal "silently loses data". |

## Design

### Registry (`melon_core`)

- `AppTypeRegistry` resource (bevy_ecs's `Arc<RwLock<TypeRegistry>>`).
  `Core::new` inserts it and registers the engine's saved components:
  `Transform`, `Aabb`, `BodyInfo` (+ `BodyKind`), `Spin`, `Bob`.
  `Name`, the parent link and `Handle<Model>` are entity-level fields of the
  scene format, not entries in the component map (below).
- `App::register_component::<T>()` where `T: Component + Reflect +
  TypePath + GetTypeRegistration` (with `ReflectDefault` when `T: Default`):
  registers `ReflectComponent` and its type data. bevy's `register` keeps
  the first registration of a `TypeId`, and a reloaded game type keeps its
  `TypeId`, so a second registration of the same `TypeId` is only accepted
  when the type's size and alignment are unchanged (then
  `overwrite_registration` refreshes it); otherwise it is refused with an
  error that says to restart, exactly like the resource layout check in
  `App` (writing to old-layout storage through new code would corrupt
  memory). A same-size field change is not detected (documented, as for
  resources).
- Each registration carries a display name (type short name, spaced:
  `BodyInfo` → "Body Info") and whether it can be added (has a default).

### Model handles

`Handle<Model>` cannot round-trip by value (runtime ids). Scene files keep
writing the model as today (`model` path + `model_guid`) at the entity
level, outside the component map; the registry marks `Handle<Model>` as
inspectable through the existing Mesh field only.

### Scene format v5 (`melon_editor::scene`)

- `SceneEntity { name, parent: Option<u32>, model: Option<String>,
  model_guid: Option<AssetGuid>, components: BTreeMap<String /* type
  path */, component RON> }`; `name` is the entity's `Name` (one place
  only, my call, builder check). The component map is written and read
  through the registry (a reflect-aware `Serialize` / `DeserializeSeed`,
  like bevy_scene), so each component appears as its plain RON value.
- Save: every registered component on the entity. Load: each known type
  path deserializes and inserts through `ReflectComponent`.
- Kept components (`UnknownComponents`, one `ron::value::RawValue` per type
  path, written back on save unchanged):
  - a type path the registry does not know (e.g. the game plugin did not
    load), and
  - a known type path whose data no longer deserializes (e.g. a renamed
    field): kept as is, Console warning naming the entity, type and error.
- When the game plugin (re)registers types, kept components whose type is
  now known and whose data deserializes are inserted and leave
  `UnknownComponents` (my calls, builder check). The Console warns once per
  type per load.
- Physics bodies are created after load from `BodyInfo`, as today.
- Versions 1–4: read with the old structs and converted to the same spawn
  path (no behaviour change for old files). Play snapshots use the same v5
  data.

### Inspector (`melon_editor`)

- Cards in registry order: Transform (existing custom layout, with the
  Mesh field), then every other registered component on the entity.
- Generic field editors by reflected kind: integers and floats (drag
  values), bool (checkbox), `String`, `Vec2/3/4` and `Quat` (Euler
  degrees) through the remote wrapper type paths, unit-variant enums
  (dropdown), structs and tuple structs (nested collapsible), `Option`
  (checkbox + value), `Vec`/lists (rows with + / −). Anything else is
  read-only text (`Debug`).
- Edits write back only on change (change detection stays quiet), and run
  the same after-edit hooks as today (rapier sync for Transform /
  BodyInfo).
- **Add Component**: button under the cards opens a searchable popup of
  registered components with a default that the entity does not have yet.
  **Remove**: a button in each card header (not on `Name`, `Transform`).

## Tests (headless)

- Registry: engine components registered; re-registering the same layout
  refreshes it; a changed size / alignment is refused; addable list
  excludes types without a default.
- ron 0.12: scenes v1–4, `config/input.ron`, `.melon/content_browser.ron`,
  `.melon/editor_layout.ron` and `.meta` fixtures still load.
- Kept components: unknown type and undeserializable data survive load +
  save byte-for-byte; a type registered later is inserted from them.
- Scene v5: round trip of every engine component; a game-like test
  component registered at runtime round-trips; unknown type path survives
  load + save; v1–4 fixtures still load; runtime-only components absent
  from the file.
- Inspector: a test component with f32, bool, String, Vec3, enum, nested
  struct, Option and Vec fields gets editors; editing one field changes
  only that field; Add Component lists only missing addable components and
  inserts the default; Remove removes it.

## Review focus

- A game component whose plugin is not loaded (ship build without it, or a
  failed reload): its data survives load + save unchanged.
- Re-registering after a live reload with the same type path does not
  duplicate Add Component entries or break existing entities.
- Quat editing in Euler degrees round-trips without drift when nothing
  was changed.
- BodyInfo edits still rebuild the rapier body (and Remove of BodyInfo
  removes it).
- Removing Transform or Name is impossible from the UI.

## Left to the builder

- Exact widget layout inside the theme; the display-name formatting.
- How the remote glam wrappers are named and where the field-kind
  dispatch lives.
- Whether `ComponentRegistry` wraps `AppTypeRegistry` or a plain
  `TypeRegistry` behind an `Arc<RwLock>`.
