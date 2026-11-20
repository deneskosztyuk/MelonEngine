# Component Reflection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One bevy_reflect component registry drives scene files (format v5), a generic Inspector with Add / Remove Component, and registration from the engine and the game plugin.

**Architecture:** `melon_core` re-exports `bevy_reflect`, defines remote reflection for glam 0.33 types, puts an `AppTypeRegistry` and a `RegistryGeneration` in the world, and adds `App::register_component::<T>()` (layout-checked). `melon_editor::scene` writes / reads a per-entity component map through the registry and keeps what it cannot load in `KeptComponents`. A new `melon_editor::inspect` draws any reflected value; the Inspector builds its cards from the registry.

**Tech Stack:** Rust 2024, bevy_ecs / bevy_reflect 0.19, ron 0.12 (bumped from 0.8), egui 0.35, glam 0.33.

**Spec:** `docs/superpowers/specs/2026-11-20-component-reflection-design.md`

## Global Constraints

- ron 0.12 workspace-wide; every existing RON file must still load: scenes v1–4, `config/input.ron`, `.melon/content_browser.ron`, `.melon/editor_layout.ron`, `.meta`.
- `melon_game` depends only on `melon_core` (reflect via `melon_core::bevy_reflect` re-export / `melon_core::prelude`).
- Saved and editable = registered with `register_component` (engine: `Transform`, `Aabb`, `BodyInfo` + `BodyKind`, `Spin`, `Bob`). `Name`, parent and `Handle<Model>` are entity-level scene fields, not in the component map. Never registered: `PreviousTransform`, `PendingDelete`, `GlobalTransform`, `ChildOf`, `Children`, rapier handles.
- Re-registering a `TypeId` is accepted only with the same size and alignment (then `overwrite_registration`); otherwise refused with an error saying to restart (same rule as `App::layout_ok`).
- glam: remote wrappers in `melon_core` for `Vec2`, `Vec3`, `Vec4`, `Quat`; RON form `(x: .., y: .., z: ..)`; `Quat` / `Vec4` wrappers need `#[reflect(Default)]` and go through glam's `Deref`.
- No silent data loss: unknown type paths and undeserializable data are kept (`ron::value::RawValue`) and written back byte-for-byte; Console warning once per type per load.
- Never replace `core.world`; never call egui `Response` / `Context` methods inside `input` closures; theme tokens for colours and sizes; no placeholder controls.
- `just gate` must pass before every commit (never chain a commit after a failing gate); stage files by explicit path; Conventional Commits; no co-author or tool credit lines.

## Review Focus

1. A game component whose plugin is not loaded: its data survives load + save unchanged, and comes back as a real component once the plugin registers it. Tests: `kept_component_round_trips_byte_for_byte`, `kept_component_inserted_when_type_registers` (Task 3).
2. Live reload re-registering the same type: no duplicate Add Component entries; a size change is refused. Tests: `reregister_same_layout_refreshes`, `reregister_changed_layout_refused` (Task 2), `add_component_list_has_no_duplicates_after_reregister` (Task 5).
3. Quat in Euler degrees: drawing the editor without touching it never changes the value. Test: `untouched_quat_editor_reports_no_change` (Task 4).
4. Editing or removing `BodyInfo` rebuilds / removes the rapier body; removing `Spin` stops it spinning. Tests: `editing_body_info_rebuilds_the_body`, `removing_body_info_removes_the_body` (Task 5).
5. `Name` and `Transform` cannot be removed from the UI; Add Component lists only addable types the entity lacks. Tests: `name_and_transform_have_no_remove`, `add_component_lists_only_missing_addable` (Task 5).

---

### Task 1: ron 0.12

**Files:** `Cargo.toml`, `Cargo.lock`, every `ron::` call site (`melon_core/src/{actions,assets}.rs`, `melon_editor/src/{scene,layout,content}.rs`), plus fixture tests.

**Interfaces:** Produces: no new names; `ron` 0.12 everywhere.

- [ ] **Step 1: Write fixture tests first, against ron 0.8** (they must pass before and after the bump): one test per file kind with a literal RON string copied from a real file in the repo or from today's serializer output: a v1, v2, v3 and v4 scene (`scene.rs` already has v1/v2/v3 cases; add a v4 literal with `model_guid`), `config/input.ron` written by today's `InputMap` save, `.melon/content_browser.ron`, `.melon/editor_layout.ron` (from `layout::save_layout` output), a `.meta` file. Each asserts the value loads and equals the expected struct.
- [ ] **Step 2: Run them on ron 0.8** — `cargo test --workspace` → PASS.
- [ ] **Step 3: Bump `ron = "0.12"`** in `[workspace.dependencies]` and fix compile errors (error types, `PrettyConfig` builders) without changing written formats beyond what ron 0.12 does by itself.
- [ ] **Step 4: Run** `just test` → all pass, fixture tests included. If a fixture no longer loads, fix the reader, never the fixture.
- [ ] **Step 5: Gate, then commit** `chore: ron 0.12`.

---

### Task 2: Reflection in `melon_core`

**Files:**
- Create: `crates/melon_core/src/reflect.rs` (glam remote wrappers, registry helpers, tests)
- Modify: `crates/melon_core/src/{lib.rs,components.rs,plugin.rs}`

**Interfaces:**
- Produces:
  - `pub use bevy_reflect;` in `melon_core`; `pub mod prelude { pub use bevy_ecs::prelude::*; pub use bevy_reflect::prelude::*; pub use crate::{Transform, Name, /* ... */}; }` enough for `#[derive(Component, Reflect)]` in `melon_game`.
  - Remote wrappers (names left to the implementer) for `glam::Vec2`, `Vec3`, `Vec4`, `Quat`; component fields use `#[reflect(remote = ...)]`.
  - `#[derive(Reflect)]` + `#[reflect(Component, Default)]` on `Transform`, `Aabb`, `BodyInfo`, `Spin`, `Bob`; `BodyKind` reflected as an enum.
  - `#[derive(Resource, Default)] pub struct RegistryGeneration(pub u64)` — bumped on every successful `register_component`.
  - `Core::new` inserts `AppTypeRegistry` + `RegistryGeneration` and registers the five engine components.
  - `App::register_component::<T: Component + Reflect + TypePath + GetTypeRegistration>(&mut self) -> &mut Self`: same-`TypeId` re-registration allowed only with the same `Layout` (else push an error into `App::errors` like `layout_ok` and skip); bumps `RegistryGeneration`. Also `Core::register_component::<T>()` for engine-side use.
  - `pub struct ComponentInfo { pub type_path: &'static str, pub display_name: String, pub addable: bool }` and `pub fn registered_components(world: &World) -> Vec<ComponentInfo>` (only types with `ReflectComponent`, sorted by display name; `addable` = has `ReflectDefault`). Display name: short type name split at case changes (`BodyInfo` → "Body Info").

- [ ] **Step 1: Write the failing tests** (`reflect.rs`)

```rust
#[test] fn engine_components_are_registered() { /* Core::new(); registered_components names == ["Aabb","Bob","Body Info","Spin","Transform"] */ }
#[test] fn glam_fields_reflect_by_name() { /* Transform reflected as struct; field "translation" is a struct with x,y,z; RON via ReflectSerializer contains "x:" */ }
#[test] fn quat_and_vec4_round_trip_through_reflect_serde() { /* Transform with a non-identity rotation -> TypedReflectSerializer -> RON -> TypedReflectDeserializer -> FromReflect equals original */ }
#[test] fn reregister_same_layout_refreshes() { /* register twice via App -> no error, generation bumped twice */ }
#[test] fn reregister_changed_layout_refused() { /* simulate: register T, then call the layout check with a different Layout for T's TypeId -> error pushed, registration unchanged */ }
#[test] fn display_names_split_case() { assert_eq!(display_name("BodyInfo"), "Body Info"); }
```

- [ ] **Step 2: Run** `cargo test -p melon_core` → compile errors (names missing).
- [ ] **Step 3: Implement the Interfaces.** Keep `Serialize`/`Deserialize` derives that other code still uses.
- [ ] **Step 4: Run** `cargo test -p melon_core` then `just test` → PASS (scenes still use the old format here).
- [ ] **Step 5: Gate, then commit** `feat(core): component reflection registry`.

---

### Task 3: Scene format v5

**Files:** `crates/melon_editor/src/scene.rs` (+ `AGENTS.md` scene-format gotcha), `crates/melon_editor/src/lib.rs` (resolve kept components when `RegistryGeneration` changes).

**Interfaces:**
- Consumes: Task 2 `AppTypeRegistry`, `RegistryGeneration`, `registered_components`.
- Produces:
  - `SCENE_VERSION = 4 → 5`; `SceneEntity { name, parent, model, model_guid, components: BTreeMap<String, Box<RawValue>> }` (versions 1–4 still parse into the old fields, converted on load).
  - `#[derive(Component, Default, Clone)] pub struct KeptComponents(pub BTreeMap<String, Box<ron::value::RawValue>>)` (not registered, not reflected).
  - Save: for each registered component on the entity, `TypedReflectSerializer` → RON text → `RawValue`; plus every entry of `KeptComponents` unchanged.
  - Load: known type path → `TypedReflectDeserializer` → `ReflectComponent::insert` (registry read-locked from a clone of the `AppTypeRegistry` Arc); unknown path or deserialize error → into `KeptComponents` + `tracing::warn!` once per type path per load.
  - `pub fn resolve_kept_components(world: &mut World) -> usize` (inserts kept entries whose type is now registered and deserializes; returns how many). `Editor` calls it in `build_ui` when `RegistryGeneration` differs from the last value it saw.
  - Physics bodies created from `BodyInfo` after load, as today; play snapshots use v5 data.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn v5_round_trips_every_engine_component() { /* entity with all five + Name + parent + model; save -> load -> equal */ }
#[test] fn v5_file_has_no_runtime_components() { /* entity with PreviousTransform, PendingDelete... -> RON text lacks those type paths */ }
#[test] fn runtime_registered_component_round_trips() { /* #[derive(Component, Reflect, Default)] struct TestHealth { hp: f32 } registered through Core::register_component -> round trip */ }
#[test] fn kept_component_round_trips_byte_for_byte() { /* v5 text with "game::Unknown": (hp: 3, kind: Fire) -> load -> save -> the component's RON text identical */ }
#[test] fn undeserializable_known_component_is_kept() { /* "melon_core::components::Spin" with garbage data -> kept, warning, others load */ }
#[test] fn kept_component_inserted_when_type_registers() { /* load with TestHealth unregistered -> kept; register; resolve_kept_components -> 1, TestHealth present, KeptComponents empty */ }
#[test] fn v1_to_v4_fixtures_still_load() { /* the Task 1 fixtures through apply_scene */ }
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib -- scene::` → failures / compile errors.
- [ ] **Step 3: Implement the Interfaces; update AGENTS.md** (Scene format: v5 component map; adding a saved component = derive `Reflect` + `register_component`, no `SceneEntity` edit).
- [ ] **Step 4: Run** `just test` → PASS.
- [ ] **Step 5: Gate, then commit** `feat(editor): scene format v5 saves components through the registry`.

---

### Task 4: Generic reflected value editor

**Files:** Create `crates/melon_editor/src/inspect.rs`; `crates/melon_editor/src/lib.rs` (`mod inspect;`).

**Interfaces:**
- Produces: `pub fn reflect_ui(ui: &mut egui::Ui, id: egui::Id, value: &mut dyn PartialReflect) -> bool` (true when the user changed something). Dispatch by `reflect_ref()` kind and type path:
  - `f32`/`f64` and integer types → `DragValue`; `bool` → checkbox; `String` → text field.
  - glam wrapper type paths → `Vec2/3/4` as x/y/z(/w) drag values; `Quat` as Euler degrees (YXZ), writing back only when an angle actually changed.
  - unit-variant enums → combo box of variant names; other enums → read-only `Debug`.
  - structs / tuple structs → nested `CollapsingHeader` with one grid row per field.
  - `Option<T>` → checkbox + inner editor (enabling inserts `T::default()` via `ReflectDefault` when available; else disabled).
  - lists (`Vec<T>`) → rows with a remove button each and an add button (default element via `ReflectDefault`; add disabled with hover text when none).
  - anything else → read-only `Debug` text.

- [ ] **Step 1: Write the failing tests** (headless egui like other editor tests; a test struct with `f32`, `bool`, `String`, `Vec3` (remote), `Quat` (remote), a unit enum, a nested struct, `Option<f32>`, `Vec<f32>`)

```rust
#[test] fn every_field_kind_gets_an_editor() { /* one frame: the response ids for each field exist (give rows ids from `id.with(field_name)`) */ }
#[test] fn editing_one_field_changes_only_that_field() { /* drag the f32 field -> returns true; other fields equal before */ }
#[test] fn untouched_quat_editor_reports_no_change() { /* rotation from_euler(30°, 45°, 10°); ten frames without input -> false, value bit-identical */ }
#[test] fn enum_combo_sets_the_variant() { /* select second variant -> value changed */ }
#[test] fn list_add_and_remove() { /* + adds default element; × removes it */ }
```

- [ ] **Step 2: Run** → compile errors.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p melon_editor --lib -- inspect::` then `just test` → PASS.
- [ ] **Step 5: Gate, then commit** `feat(editor): generic editor for reflected values`.

---

### Task 5: Registry-driven Inspector, Add / Remove Component

**Files:** `crates/melon_editor/src/lib.rs` (`build_inspector`), `README.md` (Inspector: Add Component, Remove), `docs/ROADMAP.md` (priority 1 checked, prefabs noted as next).

**Interfaces:**
- Consumes: Task 2 `registered_components`, Task 4 `reflect_ui`.
- Produces:
  - Card order: Transform (existing custom layout with the Mesh field, unchanged), then every other registered component on the entity, by display name, each drawn with `reflect_ui` on a clone and written back (`ReflectComponent::apply`) only when changed.
  - After-edit hooks as today: Transform → `after_transform_edit`; `BodyInfo` changed or removed → rebuild / remove the rapier body (reuse the existing Rigidbody card's code path, then delete that card and the Behaviours card).
  - Card header "×" Remove button for every registered component except `Transform` (and `Name`, which is the header, not a card); removal via `ReflectComponent::remove`.
  - "Add Component" button under the cards → popup with a search field and the `addable` components the entity lacks; choosing one inserts `ReflectDefault` → `ReflectComponent::insert`.

- [ ] **Step 1: Write the failing tests** (lib.rs tests, headless `build_inspector`)

```rust
#[test] fn every_registered_component_gets_a_card() { /* entity with Aabb, Spin -> card header ids for "Aabb", "Spin" exist */ }
#[test] fn name_and_transform_have_no_remove() { /* remove button ids absent for Transform; present for Spin */ }
#[test] fn removing_spin_removes_the_component() { /* click Spin's remove -> no Spin */ }
#[test] fn add_component_lists_only_missing_addable() { /* entity with Spin -> list contains "Bob", "Body Info", lacks "Spin", "Transform" */ }
#[test] fn add_component_inserts_the_default() { /* choose "Bob" -> entity has Bob */ }
#[test] fn add_component_list_has_no_duplicates_after_reregister() { /* register_component::<Spin>() again -> list has one "Spin" entry for an entity without Spin */ }
#[test] fn editing_body_info_rebuilds_the_body() { /* change body_type Fixed -> Dynamic -> PhysicsWorld body type is dynamic */ }
#[test] fn removing_body_info_removes_the_body() { /* remove -> no rapier body for the entity */ }
```

Give the ids the tests need via fixed `egui::Id`s on the card header, remove button and Add Component rows (pattern: `id_label` in `content_view.rs`).

- [ ] **Step 2: Run** → failures.
- [ ] **Step 3: Implement; update README and ROADMAP.**
- [ ] **Step 4: Run** `just test` → PASS.
- [ ] **Step 5: Check in the dev build** (`cargo run -p melon_host`): select an entity → cards for its components; edit Aabb half extents; Add Component → Bob makes it bob in Play; remove it; Save, reopen the scene file: v5 component map, readable `(x: .., y: .., z: ..)`.
- [ ] **Step 6: Gate, then commit** `feat(editor): inspector built from the component registry, add and remove components`.
