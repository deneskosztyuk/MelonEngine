# Undo / Redo, rename, duplicate (ROADMAP priority 2)

## Intent

Every editor change can be undone and redone, the way Unity (`Undo.RecordObject`, Edit ▸ Undo History), Godot (`UndoRedo`, History dock) and Unreal (transactions, Edit ▸ Undo History) do it. Plus inline rename and duplicate, which every engine has.

## Goals

- One generic mechanism covers every edit: Inspector field edits (any registered component, through the registry), Add / Remove Component, gizmo move / rotate / scale, create, delete, reparent (hierarchy drag), rename, duplicate.
- Ctrl+Z undo; Ctrl+Y and Ctrl+Shift+Z redo; Edit ▸ Undo "<step>" / Redo "<step>" menu items that name the step and are disabled when nothing is left.
- A **History** panel (new `EditorTab`): the steps in order, the current one marked; clicking a step undoes / redoes up to it.
- One step per gesture: a whole gizmo drag, a whole Inspector drag-value drag, one committed text edit, one Add Component.
- Rename: F2 or double-click in the Hierarchy opens an inline text field; Enter commits, Esc cancels; empty names are refused.
- Duplicate: Ctrl+D and Edit ▸ Duplicate copy the selected entity with its whole subtree and every component (physics and kept components included), named `Name (1)`, `Name (2)` (the first free number among its siblings), placed right after the original; the copy becomes the selection.

## Non-goals

- Undo of asset file operations (Content Browser rename / move / delete): Unity and Godot don't undo those either.
- Undo of Project Settings edits (they already have Apply / Revert).
- Recording during Play or Pause: nothing is recorded while not `Editing`; the history is kept through Play and Stop (Play's snapshot restores the edit state on Stop, as now).

## Decisions

- **Snapshot commands, not one command type per action.** A step stores the serialized state (through the component registry, as scene saving does) of the entities it touched, before and after. Undo writes "before" back; redo writes "after". Create and delete are steps whose "before" or "after" is "absent". This is Unity's model, and it covers components added later with no new code.
- **Stable editor ids.** Undoing a delete respawns entities with new `Entity` ids, so steps must refer to entities by a stable id (a uuid component the editor adds to every scene entity; reuse the scene loader's spawning). A step must still work after any number of delete / undo cycles.
- Physics bodies: restoring an entity with physics state recreates its body through the same path the scene loader uses, so physics stays consistent.
- History cap 200 steps (oldest dropped). New scene / load scene clears the history. A new edit after an undo drops the redo branch.
- Selection is restored with each step (the step's entities become selected).
- Shortcuts only fire while the editor has input (not while the game has it), and not while a text field has keyboard focus (egui's own text undo wins there).

## Constraints

- Follow AGENTS.md: editor panels via `layout::EditorTab`, `theme` tokens, menu items with working actions only, `mark_for_delete` for deleting, never despawn physics entities directly, tests in `#[cfg(test)] mod tests`.
- No egui `Response` / `Context` calls inside `ui.input` / `ctx.input` closures (deadlocks).
- Keep the history logic UI-independent (its own module, e.g. `melon_editor/src/history.rs`), tested with a `World` built from `Core::new()`.

## Done when

- Tests cover: undo / redo of a field edit, add / remove component, move (one step per drag), create, delete of a parent with children (ids, hierarchy and physics restored), reparent, rename, duplicate; delete -> undo -> redo -> undo cycles; the redo branch dropped after a new edit; nothing recorded while Playing; cap; clear on scene load.
- `just test` green; AGENTS.md (editor section) and ROADMAP (priority 2 ticked) updated.
