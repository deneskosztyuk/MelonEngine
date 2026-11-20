//! Undo / redo (ADR: docs/superpowers/specs/2026-11-20-undo-redo-design.md).
//!
//! A step stores the saved form (as scene files hold it) of the entities it
//! touched, before and after. Undo writes "before" back, redo "after"; an
//! entity that is absent on one side is created or deleted. Steps name
//! entities by `EditorId`, which survives respawns, Play and scene saves.
//! No egui here.

use std::collections::HashMap;

use bevy_ecs::entity::Entity;
use bevy_ecs::query::{With, Without};
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_ecs::world::World;
use melon_core::hierarchy::{mark_for_delete, move_root, root_index};
use melon_core::{ChildOf, Children, Name, PendingDelete, SimulationState, Transform};
use uuid::Uuid;

use crate::scene::{self, SceneEntity};

/// Steps kept; the oldest is dropped beyond this.
pub const MAX_STEPS: usize = 200;

pub use melon_core::scene::{EditorId, ensure_ids};

fn id_of(world: &mut World, e: Entity) -> Uuid {
    if let Some(id) = world.get::<EditorId>(e) {
        return id.0;
    }
    let id = Uuid::new_v4();
    world.entity_mut(e).insert(EditorId(id));
    id
}

/// Live entities by id (entities marked for deletion do not count).
fn ids_map(world: &mut World) -> HashMap<Uuid, Entity> {
    world
        .query_filtered::<(Entity, &EditorId), Without<PendingDelete>>()
        .iter(world)
        .map(|(e, id)| (id.0, e))
        .collect()
}

/// One entity as a step remembers it.
#[derive(Clone)]
pub struct Snap {
    id: Uuid,
    parent: Option<Uuid>,
    /// Position among its parent's children (roots: among the roots).
    index: usize,
    entity: SceneEntity,
}

impl Snap {
    /// Same name, model and components (parent and position ignored).
    fn same_content(&self, other: &Snap) -> bool {
        let (a, b) = (&self.entity, &other.entity);
        a.name == b.name
            && a.model == b.model
            && a.components.len() == b.components.len()
            && a.components.iter().all(|(path, raw)| {
                b.components
                    .get(path)
                    .is_some_and(|o| o.get_ron() == raw.get_ron())
            })
    }

    fn same(&self, other: &Snap) -> bool {
        self.same_content(other) && self.parent == other.parent && self.index == other.index
    }
}

/// Snapshot of `e`, or `None` if it is gone, marked for deletion or not a
/// scene entity.
pub fn capture(world: &mut World, e: Entity) -> Option<Snap> {
    if world.get::<Transform>(e).is_none() || world.get::<PendingDelete>(e).is_some() {
        return None;
    }
    let id = id_of(world, e);
    let parent_entity = world.get::<ChildOf>(e).map(ChildOf::parent);
    let parent = parent_entity.map(|p| id_of(world, p));
    let index = match parent_entity {
        Some(p) => world
            .get::<Children>(p)
            .and_then(|c| c.iter().position(|&x| x == e))
            .unwrap_or(0),
        None => root_index(world, e).unwrap_or(0),
    };
    let registry = world.get_resource::<AppTypeRegistry>()?.clone();
    let registry = registry.read();
    match scene::capture_entity(world, &registry, e, None) {
        Ok(entity) => Some(Snap {
            id,
            parent,
            index,
            entity,
        }),
        Err(error) => {
            tracing::warn!(?error, "history: cannot snapshot an entity");
            None
        }
    }
}

/// `root` and its descendants, parents first, siblings in order.
pub fn subtree(world: &World, root: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if world.get::<PendingDelete>(e).is_some() {
            continue;
        }
        out.push(e);
        if let Some(children) = world.get::<Children>(e) {
            stack.extend(children.iter().rev());
        }
    }
    out
}

fn snaps(world: &mut World, entities: &[Entity]) -> Vec<Snap> {
    entities.iter().filter_map(|&e| capture(world, e)).collect()
}

struct Change {
    id: Uuid,
    before: Option<Snap>,
    after: Option<Snap>,
}

struct Step {
    label: String,
    changes: Vec<Change>,
    /// Selected entity before / after the step; restored by undo / redo.
    sel_before: Option<Uuid>,
    sel_after: Option<Uuid>,
}

/// The selected entity as of the last frame, for `History::track`.
struct Base {
    entity: Entity,
    snap: Snap,
}

#[derive(Default)]
pub struct History {
    steps: Vec<Step>,
    /// Steps `..cursor` are applied; the rest can be redone.
    cursor: usize,
    base: Option<Base>,
    /// Label of the running gesture (gizmo drag), used when it ends.
    hint: Option<String>,
}

fn editing(world: &World) -> bool {
    world
        .get_resource::<SimulationState>()
        .is_none_or(|s| *s == SimulationState::Editing)
}

fn selection_id(world: &World, e: Option<Entity>) -> Option<Uuid> {
    e.and_then(|e| world.get::<EditorId>(e)).map(|id| id.0)
}

/// Write each target's state: `None` deletes the entity, `Some` spawns it or
/// overwrites it in place. Parents, sibling order and bodies follow.
fn apply(world: &mut World, targets: &[(Uuid, Option<&Snap>)]) {
    let mut map = ids_map(world);
    for (id, snap) in targets {
        if snap.is_none()
            && let Some(e) = map.remove(id)
        {
            mark_for_delete(world, e);
        }
    }
    let mut written: Vec<(Entity, &Snap)> = Vec::new();
    for (id, snap) in targets {
        let Some(snap) = snap else { continue };
        match scene::restore_entity(world, map.get(id).copied(), &snap.entity) {
            Ok(e) => {
                world.entity_mut(e).insert(EditorId(*id));
                map.insert(*id, e);
                written.push((e, snap));
            }
            Err(error) => tracing::warn!(?error, "history: cannot restore an entity"),
        }
    }
    for (e, snap) in &written {
        place(
            world,
            *e,
            snap.parent.and_then(|p| map.get(&p).copied()),
            snap.index,
        );
    }
    melon_physics::sync_components_system(world);
    for (e, _) in &written {
        crate::snap_previous_transform(world, *e);
    }
}

/// Put `e` under `parent` (root for `None`) at `index` among its siblings.
fn place(world: &mut World, e: Entity, parent: Option<Entity>, index: usize) {
    let current = world.get::<ChildOf>(e).map(ChildOf::parent);
    let Some(p) = parent else {
        if current.is_some() {
            world.entity_mut(e).remove::<ChildOf>();
        }
        move_root(world, e, index);
        return;
    };
    let position = world
        .get::<Children>(p)
        .and_then(|c| c.iter().position(|&x| x == e));
    let len = world.get::<Children>(p).map_or(0, |c| c.len());
    if current == Some(p) && position == Some(index.min(len.saturating_sub(1))) {
        return;
    }
    world.entity_mut(e).remove::<ChildOf>();
    let len = world.get::<Children>(p).map_or(0, |c| c.len());
    world.entity_mut(p).insert_children(index.min(len), &[e]);
}

/// "Add Mesh Renderer" style label from what changed between two snapshots.
fn label_for(before: &Snap, after: &Snap) -> String {
    let short = |path: &str| melon_core::display_name(path.rsplit("::").next().unwrap_or(path));
    let (b, a) = (&before.entity.components, &after.entity.components);
    let added: Vec<&String> = a.keys().filter(|k| !b.contains_key(*k)).collect();
    let removed: Vec<&String> = b.keys().filter(|k| !a.contains_key(*k)).collect();
    let changed: Vec<&String> = a
        .iter()
        .filter(|(k, v)| b.get(*k).is_some_and(|o| o.get_ron() != v.get_ron()))
        .map(|(k, _)| k)
        .collect();
    match (added.as_slice(), removed.as_slice(), changed.as_slice()) {
        ([one], [], []) => format!("Add {}", short(one)),
        ([], [one], []) => format!("Remove {}", short(one)),
        ([], [], [one]) if before.entity.model == after.entity.model => {
            format!("Edit {}", short(one))
        }
        ([], [], []) if before.entity.model != after.entity.model => "Change Mesh".to_string(),
        _ => format!("Edit {}", after.entity.name),
    }
}

impl History {
    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor < self.steps.len()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.cursor
            .checked_sub(1)
            .map(|i| self.steps[i].label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.steps.get(self.cursor).map(|s| s.label.as_str())
    }

    /// Step labels, oldest first.
    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.steps.iter().map(|s| s.label.as_str())
    }

    /// How many steps are applied (the History panel marks step `cursor - 1`).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Forget everything (a scene was opened).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn push(&mut self, step: Step) {
        self.steps.truncate(self.cursor);
        self.steps.push(step);
        if self.steps.len() > MAX_STEPS {
            self.steps.remove(0);
        }
        self.cursor = self.steps.len();
        self.base = None;
    }

    /// Record the pending edit of the tracked entity (a gesture still going,
    /// e.g. text typed in an Inspector field) as its own step. Call before
    /// anything that would change or replace that entity.
    fn flush(&mut self, world: &mut World) {
        if !editing(world) {
            return;
        }
        let Some(base) = self.base.take() else { return };
        let hint = self.hint.take();
        let Some(now) = capture(world, base.entity) else {
            return;
        };
        if base.snap.same_content(&now) {
            self.base = Some(base);
            return;
        }
        let label = hint.unwrap_or_else(|| label_for(&base.snap, &now));
        let id = base.snap.id;
        self.push(Step {
            label,
            changes: vec![Change {
                id,
                before: Some(base.snap),
                after: Some(now),
            }],
            sel_before: Some(id),
            sel_after: Some(id),
        });
    }

    /// Snapshots to hand to `commit`: take them before the edit.
    pub fn begin(&self, world: &mut World, entities: &[Entity]) -> Vec<Snap> {
        snaps(world, entities)
    }

    /// Record the edit made since `begin`: `before` entities as they are now
    /// (absent = deleted) plus `created` ones. Nothing is recorded unless the
    /// world is `Editing` and something changed.
    pub fn commit(
        &mut self,
        world: &mut World,
        before: Vec<Snap>,
        created: &[Entity],
        label: String,
        sel_before: Option<Entity>,
        sel_after: Option<Entity>,
    ) {
        if !editing(world) {
            return;
        }
        let mut changes = Vec::new();
        for b in before {
            let after = ids_map(world).get(&b.id).and_then(|&e| capture(world, e));
            if after.as_ref().is_none_or(|a| !b.same(a)) {
                changes.push(Change {
                    id: b.id,
                    before: Some(b),
                    after,
                });
            }
        }
        for s in snaps(world, created) {
            changes.push(Change {
                id: s.id,
                before: None,
                after: Some(s),
            });
        }
        if changes.is_empty() {
            return;
        }
        let step = Step {
            label,
            changes,
            sel_before: selection_id(world, sel_before),
            sel_after: selection_id(world, sel_after),
        };
        self.push(step);
    }

    /// Undo the last step. `Some(selection)` when a step was undone (the
    /// entity to select), `None` when there was nothing to undo or the world
    /// is not `Editing`.
    pub fn undo(&mut self, world: &mut World) -> Option<Option<Entity>> {
        self.flush(world);
        if !editing(world) || self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        let step = &self.steps[self.cursor];
        let targets: Vec<_> = step
            .changes
            .iter()
            .map(|c| (c.id, c.before.as_ref()))
            .collect();
        apply(world, &targets);
        self.base = None;
        Some(
            step.sel_before
                .and_then(|id| ids_map(world).get(&id).copied()),
        )
    }

    /// Redo the step after the cursor; see `undo`.
    pub fn redo(&mut self, world: &mut World) -> Option<Option<Entity>> {
        self.flush(world);
        if !editing(world) || self.cursor >= self.steps.len() {
            return None;
        }
        let step = &self.steps[self.cursor];
        self.cursor += 1;
        let targets: Vec<_> = step
            .changes
            .iter()
            .map(|c| (c.id, c.after.as_ref()))
            .collect();
        apply(world, &targets);
        self.base = None;
        Some(
            step.sel_after
                .and_then(|id| ids_map(world).get(&id).copied()),
        )
    }

    /// Undo or redo until `cursor` steps are applied (History panel click).
    pub fn jump_to(&mut self, world: &mut World, cursor: usize) -> Option<Option<Entity>> {
        let mut selection = None;
        while self.cursor > cursor {
            selection = Some(self.undo(world)?);
        }
        while self.cursor < cursor.min(self.steps.len()) {
            selection = Some(self.redo(world)?);
        }
        selection
    }

    /// Call once per frame after the panels ran: records edits made to the
    /// selected entity (Inspector fields, Add / Remove Component, mesh,
    /// gizmo) as one step per gesture. `active` is true while a gesture is
    /// still going (a drag, a focused text field); the step is recorded when
    /// it ends. `hint` names the gesture ("Move") instead of the diff.
    pub fn track(
        &mut self,
        world: &mut World,
        selected: Option<Entity>,
        active: bool,
        hint: Option<&str>,
    ) {
        if !editing(world) {
            self.base = None;
            self.hint = None;
            return;
        }
        if let Some(h) = hint {
            self.hint = Some(h.to_string());
        }
        if let Some(base) = self.base.take() {
            match capture(world, base.entity) {
                Some(now) if !base.snap.same_content(&now) => {
                    if active && selected == Some(base.entity) {
                        self.base = Some(base);
                        return;
                    }
                    let label = self
                        .hint
                        .take()
                        .unwrap_or_else(|| label_for(&base.snap, &now));
                    let id = base.snap.id;
                    self.push(Step {
                        label,
                        changes: vec![Change {
                            id,
                            before: Some(base.snap),
                            after: Some(now.clone()),
                        }],
                        sel_before: Some(id),
                        sel_after: Some(id),
                    });
                    if selected == Some(base.entity) {
                        self.base = Some(Base {
                            entity: base.entity,
                            snap: now,
                        });
                        return;
                    }
                }
                Some(_) if selected == Some(base.entity) => {
                    self.base = Some(base);
                    if !active {
                        self.hint = None;
                    }
                    return;
                }
                _ => {}
            }
        }
        self.hint = None;
        self.base =
            selected.and_then(|entity| capture(world, entity).map(|snap| Base { entity, snap }));
    }

    /// Record an entity (and its children) that was just created.
    pub fn record_created(
        &mut self,
        world: &mut World,
        root: Entity,
        label: String,
        sel_before: Option<Entity>,
    ) {
        self.flush(world);
        let created = subtree(world, root);
        self.commit(world, Vec::new(), &created, label, sel_before, Some(root));
    }

    /// Delete `entity` with its children, recorded as one step.
    pub fn delete(&mut self, world: &mut World, entity: Entity, selected: Option<Entity>) {
        self.flush(world);
        let label = format!("Delete {}", name_of(world, entity));
        let before = self.begin(world, &subtree(world, entity));
        mark_for_delete(world, entity);
        self.commit(world, before, &[], label, selected, None);
    }

    /// Rename `entity`. `Err` is a status-bar message (empty name).
    pub fn rename(
        &mut self,
        world: &mut World,
        entity: Entity,
        new_name: &str,
        selected: Option<Entity>,
    ) -> Result<(), String> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err("A name can't be empty".to_string());
        }
        self.flush(world);
        let before = self.begin(world, &[entity]);
        let Some(mut name) = world.get_mut::<Name>(entity) else {
            return Ok(());
        };
        if name.0 == new_name {
            return Ok(());
        }
        let label = format!("Rename {}", name.0);
        name.0 = new_name.to_string();
        self.commit(world, before, &[], label, selected, selected);
        Ok(())
    }

    /// Hierarchy drop (see `hierarchy_drop`), recorded as one step.
    pub fn reparent(
        &mut self,
        world: &mut World,
        dragged: Entity,
        target: Option<Entity>,
        selected: Option<Entity>,
    ) -> Result<(), String> {
        self.flush(world);
        let label = format!("Reparent {}", name_of(world, dragged));
        let before = self.begin(world, &[dragged]);
        crate::hierarchy_drop(world, dragged, target)?;
        self.commit(world, before, &[], label, selected, selected);
        Ok(())
    }

    /// Copy `entity` with its subtree and every component, named `Name (N)`,
    /// right after the original among its siblings. Returns the copy's root.
    pub fn duplicate(
        &mut self,
        world: &mut World,
        entity: Entity,
        selected: Option<Entity>,
    ) -> Option<Entity> {
        self.flush(world);
        if !editing(world) {
            return None;
        }
        let originals = snaps(world, &subtree(world, entity));
        let root = originals.first()?;
        let fresh: HashMap<Uuid, Uuid> = originals.iter().map(|s| (s.id, Uuid::new_v4())).collect();
        let name = free_name(world, entity, &root.entity.name);
        let label = format!("Duplicate {}", root.entity.name);
        let copies: Vec<Snap> = originals
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut copy = s.clone();
                copy.id = fresh[&s.id];
                copy.entity.uid = Some(copy.id);
                if i == 0 {
                    copy.index += 1;
                    copy.entity.name = name.clone();
                } else {
                    copy.parent = s.parent.map(|p| fresh[&p]);
                }
                copy
            })
            .collect();
        let new_root = copies[0].id;
        let targets: Vec<_> = copies.iter().map(|c| (c.id, Some(c))).collect();
        apply(world, &targets);
        let copy = ids_map(world).get(&new_root).copied();
        self.push(Step {
            label,
            changes: copies
                .iter()
                .map(|c| Change {
                    id: c.id,
                    before: None,
                    after: Some(c.clone()),
                })
                .collect(),
            sel_before: selection_id(world, selected),
            sel_after: Some(new_root),
        });
        copy
    }
}

pub(crate) fn name_of(world: &World, e: Entity) -> String {
    world
        .get::<Name>(e)
        .map(|n| n.0.clone())
        .unwrap_or_default()
}

/// `Name (N)` with the first N not used by a sibling of `entity`. A trailing
/// ` (N)` on the original is replaced, not stacked.
fn free_name(world: &mut World, entity: Entity, original: &str) -> String {
    let stem = original
        .strip_suffix(')')
        .and_then(|s| s.rsplit_once(" ("))
        .filter(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .map_or(original, |(stem, _)| stem);
    let parent = world.get::<ChildOf>(entity).map(ChildOf::parent);
    unused_name(world, parent, stem, false)
}

/// A name for a new child of `parent` (a root for `None`): `stem` itself if
/// `bare_ok` and free, else `stem (N)` with the first N not used by a sibling.
pub(crate) fn unused_name(
    world: &mut World,
    parent: Option<Entity>,
    stem: &str,
    bare_ok: bool,
) -> String {
    let siblings: Vec<Entity> = match parent {
        Some(p) => world
            .get::<Children>(p)
            .map(|c| c.iter().copied().collect())
            .unwrap_or_default(),
        None => world
            .query_filtered::<Entity, (With<Name>, With<Transform>, Without<ChildOf>)>()
            .iter(world)
            .collect(),
    };
    let taken: Vec<String> = siblings.iter().map(|&e| name_of(world, e)).collect();
    if bare_ok && !taken.iter().any(|t| t == stem) {
        return stem.to_string();
    }
    (1..)
        .map(|n| format!("{stem} ({n})"))
        .find(|candidate| !taken.contains(candidate))
        .expect("an unused number exists")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::schedule::Schedule;
    use melon_core::glam::Vec3;
    use melon_core::{Collider, RigidBody, RigidBodyKind, Spin};
    use melon_physics::PhysicsWorld;

    fn world() -> World {
        let mut world = melon_core::Core::new().world;
        world.insert_resource(PhysicsWorld::new());
        world.insert_resource(SimulationState::Editing);
        world
    }

    fn spawn(world: &mut World, name: &str, x: f32) -> Entity {
        let e = world
            .spawn((
                Name(name.to_string()),
                Transform::from_translation(Vec3::new(x, 0.0, 0.0)),
            ))
            .id();
        // The Hierarchy lists roots every frame, which fixes their order.
        melon_core::hierarchy::root_order(world);
        e
    }

    fn child(world: &mut World, name: &str, parent: Entity) -> Entity {
        let e = spawn(world, name, 0.0);
        world.entity_mut(e).insert(ChildOf(parent));
        e
    }

    /// What the physics PostUpdate does for entities marked for deletion.
    fn cleanup(world: &mut World) {
        let mut schedule = Schedule::default();
        schedule.add_systems(melon_physics::cleanup_deleted_system);
        schedule.run(world);
    }

    fn find(world: &mut World, name: &str) -> Option<Entity> {
        world
            .query::<(Entity, &Name)>()
            .iter(world)
            .find(|(_, n)| n.0 == name)
            .map(|(e, _)| e)
    }

    fn count(world: &mut World) -> usize {
        world.query::<&Name>().iter(world).count()
    }

    fn x_of(world: &World, e: Entity) -> f32 {
        world.get::<Transform>(e).unwrap().translation.x
    }

    fn kids(world: &World, e: Entity) -> Vec<String> {
        world
            .get::<Children>(e)
            .map(|c| c.iter().map(|&k| name_of(world, k)).collect())
            .unwrap_or_default()
    }

    #[test]
    fn field_edit_undoes_and_redoes() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 1.0);
        h.track(&mut world, Some(a), false, None);
        world.get_mut::<Transform>(a).unwrap().translation.x = 5.0;
        h.track(&mut world, Some(a), false, None);
        assert_eq!(h.undo_label(), Some("Edit Transform"));
        h.undo(&mut world).unwrap();
        assert_eq!(x_of(&world, a), 1.0);
        let sel = h.redo(&mut world).unwrap();
        assert_eq!(x_of(&world, a), 5.0);
        assert_eq!(sel, Some(a), "same entity, edited in place");
    }

    #[test]
    fn a_drag_is_one_step() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        h.track(&mut world, Some(a), false, None);
        for x in 1..=10 {
            world.get_mut::<Transform>(a).unwrap().translation.x = x as f32;
            h.track(&mut world, Some(a), true, Some("Move"));
        }
        assert_eq!(h.labels().count(), 0, "nothing until the drag ends");
        h.track(&mut world, Some(a), false, None);
        assert_eq!(h.labels().collect::<Vec<_>>(), ["Move"]);
        h.undo(&mut world).unwrap();
        assert_eq!(x_of(&world, a), 0.0);
        assert!(!h.can_undo());
    }

    #[test]
    fn a_drag_back_to_the_start_records_nothing() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 2.0);
        h.track(&mut world, Some(a), false, None);
        world.get_mut::<Transform>(a).unwrap().translation.x = 9.0;
        h.track(&mut world, Some(a), true, None);
        world.get_mut::<Transform>(a).unwrap().translation.x = 2.0;
        h.track(&mut world, Some(a), false, None);
        assert!(!h.can_undo());
    }

    #[test]
    fn add_and_remove_component() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        h.track(&mut world, Some(a), false, None);
        world.entity_mut(a).insert(Spin);
        h.track(&mut world, Some(a), false, None);
        world.entity_mut(a).remove::<Spin>();
        h.track(&mut world, Some(a), false, None);
        assert_eq!(h.labels().collect::<Vec<_>>(), ["Add Spin", "Remove Spin"]);
        h.undo(&mut world).unwrap();
        assert!(world.get::<Spin>(a).is_some());
        h.undo(&mut world).unwrap();
        assert!(world.get::<Spin>(a).is_none());
        h.redo(&mut world).unwrap();
        assert!(world.get::<Spin>(a).is_some());
    }

    #[test]
    fn undo_does_not_record_itself() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        h.track(&mut world, Some(a), false, None);
        world.get_mut::<Transform>(a).unwrap().translation.x = 3.0;
        h.track(&mut world, Some(a), false, None);
        h.undo(&mut world).unwrap();
        h.track(&mut world, Some(a), false, None);
        h.track(&mut world, Some(a), false, None);
        assert_eq!(h.labels().count(), 1);
        assert!(h.can_redo());
    }

    #[test]
    fn create_undoes_to_nothing_and_redoes() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 4.0);
        h.record_created(&mut world, a, "Create A".into(), None);
        h.undo(&mut world).unwrap();
        cleanup(&mut world);
        assert_eq!(count(&mut world), 0);
        let sel = h.redo(&mut world).unwrap();
        let a = find(&mut world, "A").unwrap();
        assert_eq!(x_of(&world, a), 4.0);
        assert_eq!(sel, Some(a));
    }

    fn family(world: &mut World) -> Entity {
        let root = spawn(world, "Root", 1.0);
        let k1 = child(world, "K1", root);
        child(world, "K2", root);
        child(world, "G", k1);
        world.entity_mut(k1).insert((
            RigidBody {
                kind: RigidBodyKind::Fixed,
                ..Default::default()
            },
            Collider::default(),
        ));
        world
            .entity_mut(root)
            .insert((RigidBody::default(), Collider::default()));
        root
    }

    fn body_count(world: &World) -> usize {
        world.resource::<PhysicsWorld>().bodies.len()
    }

    #[test]
    fn delete_parent_restores_ids_hierarchy_and_physics_through_cycles() {
        let mut world = world();
        let mut h = History::default();
        let root = family(&mut world);
        melon_physics::sync_components_system(&mut world);
        ensure_ids(&mut world);
        let ids: Vec<Uuid> = ["Root", "K1", "K2", "G"]
            .iter()
            .map(|n| {
                let e = find(&mut world, n).unwrap();
                world.get::<EditorId>(e).unwrap().0
            })
            .collect();
        assert_eq!(body_count(&world), 2);

        h.delete(&mut world, root, Some(root));
        cleanup(&mut world);
        assert_eq!(count(&mut world), 0);
        assert_eq!(body_count(&world), 0);

        for _ in 0..3 {
            let sel = h.undo(&mut world).unwrap();
            assert_eq!(count(&mut world), 4);
            assert_eq!(body_count(&world), 2, "bodies recreated");
            let root = find(&mut world, "Root").unwrap();
            assert_eq!(sel, Some(root));
            assert_eq!(kids(&world, root), ["K1", "K2"]);
            let k1 = find(&mut world, "K1").unwrap();
            assert_eq!(kids(&world, k1), ["G"]);
            let now: Vec<Uuid> = ["Root", "K1", "K2", "G"]
                .iter()
                .map(|n| {
                    let e = find(&mut world, n).unwrap();
                    world.get::<EditorId>(e).unwrap().0
                })
                .collect();
            assert_eq!(now, ids, "ids survive the respawn");
            h.redo(&mut world).unwrap();
            cleanup(&mut world);
            assert_eq!(count(&mut world), 0);
            assert_eq!(body_count(&world), 0);
        }
    }

    #[test]
    fn reparent_undoes_with_position_and_order() {
        let mut world = world();
        let mut h = History::default();
        let p = spawn(&mut world, "P", 10.0);
        let a = child(&mut world, "A", p);
        let b = child(&mut world, "B", p);
        let c = child(&mut world, "C", p);
        let other = spawn(&mut world, "Other", 0.0);
        h.reparent(&mut world, b, Some(other), None).unwrap();
        assert_eq!(kids(&world, p), ["A", "C"]);
        assert_eq!(kids(&world, other), ["B"]);
        h.undo(&mut world).unwrap();
        assert_eq!(kids(&world, p), ["A", "B", "C"]);
        assert!(kids(&world, other).is_empty());
        h.redo(&mut world).unwrap();
        assert_eq!(kids(&world, other), ["B"]);
        let _ = (a, c);
    }

    #[test]
    fn rename_undoes_and_empty_is_refused() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        assert!(h.rename(&mut world, a, "  ", None).is_err());
        assert!(!h.can_undo());
        h.rename(&mut world, a, "Hero", None).unwrap();
        assert_eq!(name_of(&world, a), "Hero");
        assert_eq!(h.undo_label(), Some("Rename A"));
        h.undo(&mut world).unwrap();
        assert_eq!(name_of(&world, a), "A");
        h.redo(&mut world).unwrap();
        assert_eq!(name_of(&world, a), "Hero");
    }

    #[test]
    fn duplicate_copies_subtree_after_the_original_with_a_free_name() {
        let mut world = world();
        let mut h = History::default();
        let p = spawn(&mut world, "P", 0.0);
        let a = child(&mut world, "A", p);
        child(&mut world, "B", p);
        child(&mut world, "A (1)", p);
        child(&mut world, "Leaf", a);
        world.entity_mut(a).insert(Spin);

        let copy = h.duplicate(&mut world, a, Some(a)).unwrap();
        assert_eq!(name_of(&world, copy), "A (2)");
        assert_eq!(kids(&world, p), ["A", "A (2)", "B", "A (1)"]);
        assert_eq!(kids(&world, copy), ["Leaf"]);
        assert!(world.get::<Spin>(copy).is_some());
        let ids: Vec<_> = [a, copy]
            .iter()
            .map(|&e| world.get::<EditorId>(e).unwrap().0)
            .collect();
        assert_ne!(ids[0], ids[1]);

        let sel = h.undo(&mut world).unwrap();
        cleanup(&mut world);
        assert_eq!(sel, Some(a));
        assert_eq!(kids(&world, p), ["A", "B", "A (1)"]);
        assert_eq!(count(&mut world), 5);
        h.redo(&mut world).unwrap();
        assert_eq!(kids(&world, p), ["A", "A (2)", "B", "A (1)"]);
    }

    #[test]
    fn duplicate_a_numbered_name_replaces_the_number() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "Cube (1)", 0.0);
        let copy = h.duplicate(&mut world, a, None).unwrap();
        assert_eq!(name_of(&world, copy), "Cube (2)");
    }

    #[test]
    fn duplicate_of_a_physics_entity_makes_a_second_body() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        world
            .entity_mut(a)
            .insert((RigidBody::default(), Collider::default()));
        melon_physics::sync_components_system(&mut world);
        assert_eq!(body_count(&world), 1);
        h.duplicate(&mut world, a, None).unwrap();
        assert_eq!(body_count(&world), 2);
    }

    #[test]
    fn a_new_edit_drops_the_redo_branch() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        h.rename(&mut world, a, "B", None).unwrap();
        h.rename(&mut world, a, "C", None).unwrap();
        h.undo(&mut world).unwrap();
        assert!(h.can_redo());
        h.rename(&mut world, a, "D", None).unwrap();
        assert!(!h.can_redo());
        assert_eq!(h.labels().collect::<Vec<_>>(), ["Rename A", "Rename B"]);
    }

    #[test]
    fn nothing_is_recorded_or_undone_outside_editing() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        h.rename(&mut world, a, "B", None).unwrap();
        world.insert_resource(SimulationState::Playing);
        h.rename(&mut world, a, "C", None).unwrap();
        h.record_created(&mut world, a, "x".into(), None);
        h.track(&mut world, Some(a), false, None);
        world.get_mut::<Transform>(a).unwrap().translation.x = 1.0;
        h.track(&mut world, Some(a), false, None);
        assert_eq!(h.labels().count(), 1);
        assert!(h.undo(&mut world).is_none(), "history waits for Stop");
        world.insert_resource(SimulationState::Paused);
        assert!(h.undo(&mut world).is_none());
        world.insert_resource(SimulationState::Editing);
        assert!(h.undo(&mut world).is_some());
    }

    #[test]
    fn history_is_capped_and_drops_the_oldest() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        for i in 0..MAX_STEPS + 5 {
            h.rename(&mut world, a, &format!("N{i}"), None).unwrap();
        }
        assert_eq!(h.labels().count(), MAX_STEPS);
        assert_eq!(h.labels().next(), Some("Rename N4"));
        for _ in 0..MAX_STEPS {
            h.undo(&mut world).unwrap();
        }
        assert!(!h.can_undo());
        assert_eq!(name_of(&world, a), "N4");
    }

    #[test]
    fn clear_forgets_everything() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        h.rename(&mut world, a, "B", None).unwrap();
        h.clear();
        assert!(!h.can_undo() && !h.can_redo());
    }

    #[test]
    fn jump_to_walks_both_ways() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        for n in ["B", "C", "D"] {
            h.rename(&mut world, a, n, None).unwrap();
        }
        h.jump_to(&mut world, 1).unwrap();
        assert_eq!((name_of(&world, a).as_str(), h.cursor()), ("B", 1));
        h.jump_to(&mut world, 3).unwrap();
        assert_eq!(name_of(&world, a), "D");
        h.jump_to(&mut world, 0).unwrap();
        assert_eq!(name_of(&world, a), "A");
    }

    #[test]
    fn ids_survive_the_play_snapshot() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        h.rename(&mut world, a, "B", None).unwrap();
        let id = world.get::<EditorId>(a).unwrap().0;
        let snapshot = scene::scene_to_data(&mut world).unwrap();
        world
            .try_resource_scope::<PhysicsWorld, _>(|world, mut physics| {
                scene::apply_scene(world, &mut physics, &snapshot)
            })
            .unwrap()
            .unwrap();
        let b = find(&mut world, "B").unwrap();
        assert_eq!(world.get::<EditorId>(b).unwrap().0, id);
        h.undo(&mut world).unwrap();
        assert_eq!(name_of(&world, b), "A", "the step still finds its entity");
    }

    fn root_names(world: &mut World) -> Vec<String> {
        melon_core::hierarchy::depth_first(world)
            .into_iter()
            .filter(|(_, d)| *d == 0)
            .map(|(e, _)| name_of(world, e))
            .collect()
    }

    #[test]
    fn deleted_root_returns_to_its_position() {
        let mut world = world();
        let mut h = History::default();
        let _a = spawn(&mut world, "A", 0.0);
        let b = spawn(&mut world, "B", 0.0);
        let _c = spawn(&mut world, "C", 0.0);
        assert_eq!(root_names(&mut world), ["A", "B", "C"]);
        h.delete(&mut world, b, None);
        cleanup(&mut world);
        assert_eq!(root_names(&mut world), ["A", "C"]);
        h.undo(&mut world).unwrap();
        assert_eq!(root_names(&mut world), ["A", "B", "C"]);
    }

    #[test]
    fn duplicate_of_a_root_goes_right_after_it() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        let _b = spawn(&mut world, "B", 0.0);
        let _c = spawn(&mut world, "C", 0.0);
        h.duplicate(&mut world, a, None).unwrap();
        assert_eq!(root_names(&mut world), ["A", "A (1)", "B", "C"]);
        h.undo(&mut world).unwrap();
        cleanup(&mut world);
        assert_eq!(root_names(&mut world), ["A", "B", "C"]);
    }

    #[test]
    fn reparent_to_root_appends_and_undo_restores_the_old_place() {
        let mut world = world();
        let mut h = History::default();
        let _a = spawn(&mut world, "A", 0.0);
        let p = spawn(&mut world, "P", 0.0);
        let k = child(&mut world, "K", p);
        let _c = spawn(&mut world, "C", 0.0);
        h.reparent(&mut world, k, None, None).unwrap();
        assert_eq!(root_names(&mut world), ["A", "P", "C", "K"]);
        h.undo(&mut world).unwrap();
        assert_eq!(root_names(&mut world), ["A", "P", "C"]);
        h.redo(&mut world).unwrap();
        assert_eq!(root_names(&mut world), ["A", "P", "C", "K"]);
    }

    #[test]
    fn scene_load_keeps_root_order() {
        let mut world = world();
        let _a = spawn(&mut world, "A", 0.0);
        let b = spawn(&mut world, "B", 0.0);
        world.entity_mut(b).insert(Spin); // different archetype
        let _c = spawn(&mut world, "C", 0.0);
        let data = scene::scene_to_data(&mut world).unwrap();
        world
            .try_resource_scope::<PhysicsWorld, _>(|world, mut physics| {
                scene::apply_scene(world, &mut physics, &data)
            })
            .unwrap()
            .unwrap();
        assert_eq!(root_names(&mut world), ["A", "B", "C"]);
    }

    fn physics_world() -> World {
        let mut world = world();
        world.insert_resource(melon_core::Time::default());
        world
    }

    fn dynamic() -> RigidBody {
        RigidBody::default()
    }

    #[test]
    fn undo_of_add_physics_component_removes_the_body() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        h.track(&mut world, Some(a), false, None);
        // What the Inspector's Add Component does for a physics component.
        world.entity_mut(a).insert(dynamic());
        melon_physics::sync_components_system(&mut world);
        h.track(&mut world, Some(a), false, None);
        assert_eq!(h.undo_label(), Some("Add Rigid Body"));
        assert_eq!(body_count(&world), 1);
        h.undo(&mut world).unwrap();
        assert_eq!(body_count(&world), 0, "rapier body removed");
        assert!(world.get::<RigidBody>(a).is_none());
        assert!(world.get::<melon_physics::RigidBodyComponent>(a).is_none());
        h.redo(&mut world).unwrap();
        assert_eq!(body_count(&world), 1);
        assert!(world.get::<melon_physics::RigidBodyComponent>(a).is_some());
    }

    #[test]
    fn undo_of_deleting_a_jointed_chain_restores_bodies_and_joints() {
        use melon_core::{Joint, JointKind};
        let mut world = world();
        let mut h = History::default();
        let root = spawn(&mut world, "Root", 0.0);
        let k1 = child(&mut world, "K1", root);
        let g = child(&mut world, "G", k1);
        world.entity_mut(root).insert((
            RigidBody {
                kind: RigidBodyKind::Fixed,
                ..Default::default()
            },
            Collider::default(),
        ));
        let hinge = JointKind::Hinge {
            axis: melon_core::glam::Vec3::Z,
            limited: true,
            min: -0.5,
            max: 0.5,
        };
        for e in [k1, g] {
            world.entity_mut(e).insert((
                RigidBody::default(),
                Collider::default(),
                Joint {
                    kind: hinge,
                    anchor: melon_core::glam::Vec3::new(0.0, 1.0, 0.0),
                    ..Default::default()
                },
            ));
        }
        melon_physics::sync_components_system(&mut world);
        ensure_ids(&mut world);
        let joints = |w: &World| w.resource::<PhysicsWorld>().impulse_joints.len();
        assert_eq!((body_count(&world), joints(&world)), (3, 2));

        h.delete(&mut world, root, Some(root));
        cleanup(&mut world);
        assert_eq!((body_count(&world), joints(&world)), (0, 0));

        for _ in 0..2 {
            h.undo(&mut world).unwrap();
            assert_eq!(
                (body_count(&world), joints(&world)),
                (3, 2),
                "chain is back"
            );
            let g = find(&mut world, "G").unwrap();
            assert_eq!(world.get::<Joint>(g).unwrap().kind, hinge);
            assert!(world.get::<melon_physics::JointComponent>(g).is_some());
            // It still simulates.
            for _ in 0..10 {
                world.resource_mut::<PhysicsWorld>().step();
            }
            h.redo(&mut world).unwrap();
            cleanup(&mut world);
            assert_eq!((body_count(&world), joints(&world)), (0, 0));
        }
    }

    #[test]
    fn undo_of_delete_keeps_unknown_components() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        let unknown = "(hp: 3, tags: [\"a\"])";
        world.entity_mut(a).insert(scene::KeptComponents(
            [(
                "game::Unknown".to_string(),
                ron::value::RawValue::from_boxed_ron(unknown.to_string().into_boxed_str()).unwrap(),
            )]
            .into(),
        ));
        h.delete(&mut world, a, Some(a));
        cleanup(&mut world);
        h.undo(&mut world).unwrap();
        let a = find(&mut world, "A").unwrap();
        let kept = world.get::<scene::KeptComponents>(a).unwrap();
        assert_eq!(kept.0["game::Unknown"].get_ron(), unknown);
        h.redo(&mut world).unwrap();
        cleanup(&mut world);
        h.undo(&mut world).unwrap();
        let a = find(&mut world, "A").unwrap();
        assert!(world.get::<scene::KeptComponents>(a).is_some());
    }

    #[test]
    fn entities_of_an_old_scene_get_ids_and_save_them() {
        let mut world = world();
        let mut h = History::default();
        let data: scene::SceneData = ron::from_str(
            r#"(version: 5, entities: [(name: "A", parent: None, model: None, model_guid: None, components: {"melon_core::components::Spin": ()})], name_counter: 0)"#,
        )
        .unwrap();
        world
            .try_resource_scope::<PhysicsWorld, _>(|world, mut physics| {
                scene::apply_scene(world, &mut physics, &data)
            })
            .unwrap()
            .unwrap();
        let a = find(&mut world, "A").unwrap();
        assert!(world.get::<EditorId>(a).is_none());
        h.rename(&mut world, a, "B", None).unwrap();
        let id = world.get::<EditorId>(a).unwrap().0;
        let saved = scene::scene_to_data(&mut world).unwrap();
        assert!(ron::to_string(&saved).unwrap().contains(&id.to_string()));
        h.undo(&mut world).unwrap();
        assert_eq!(name_of(&world, a), "A");
    }

    #[test]
    fn rename_to_only_spaces_is_refused() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        assert!(h.rename(&mut world, a, "   ", None).is_err());
        assert_eq!(name_of(&world, a), "A");
        assert!(!h.can_undo());
    }

    #[test]
    fn duplicate_takes_the_first_free_number() {
        let mut world = world();
        let mut h = History::default();
        let c = spawn(&mut world, "Cube", 0.0);
        let _c1 = spawn(&mut world, "Cube (1)", 0.0);
        let c3 = spawn(&mut world, "Cube (3)", 0.0);
        let copy = h.duplicate(&mut world, c, None).unwrap();
        assert_eq!(name_of(&world, copy), "Cube (2)");
        let copy = h.duplicate(&mut world, c3, None).unwrap();
        assert_eq!(name_of(&world, copy), "Cube (4)");
    }

    #[test]
    fn a_duplicated_body_is_independent_of_the_original() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        world.entity_mut(a).insert(dynamic());
        melon_physics::sync_components_system(&mut world);
        let copy = h.duplicate(&mut world, a, None).unwrap();
        let handle = |w: &World, e| {
            w.get::<melon_physics::RigidBodyComponent>(e)
                .unwrap()
                .body_handle
        };
        assert_ne!(handle(&world, a), handle(&world, copy));
        assert_eq!(body_count(&world), 2);
        // Undoing the duplicate removes only the copy's body.
        h.undo(&mut world).unwrap();
        cleanup(&mut world);
        assert_eq!(body_count(&world), 1);
        assert!(
            world
                .resource::<PhysicsWorld>()
                .bodies
                .get(handle(&world, a))
                .is_some(),
            "the original keeps its body"
        );
    }

    #[test]
    fn redo_works_after_the_cap_trimmed_the_oldest() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        for i in 0..MAX_STEPS + 5 {
            h.rename(&mut world, a, &format!("N{i}"), None).unwrap();
        }
        for _ in 0..MAX_STEPS {
            h.undo(&mut world).unwrap();
        }
        for _ in 0..MAX_STEPS {
            h.redo(&mut world).unwrap();
        }
        assert!(!h.can_redo());
        assert_eq!(name_of(&world, a), format!("N{}", MAX_STEPS + 4));
    }

    #[test]
    fn undo_after_play_and_stop_works_with_no_bogus_steps() {
        let mut world = physics_world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        let _b = spawn(&mut world, "B", 0.0);
        world.entity_mut(a).insert(dynamic());
        melon_physics::sync_components_system(&mut world);
        h.rename(&mut world, a, "A2", Some(a)).unwrap();
        h.track(&mut world, Some(a), false, None);
        crate::play::play(&mut world).unwrap();
        // The game spawns an entity (no EditorId) and the Hierarchy lists it.
        let g = spawn(&mut world, "Bullet", 0.0);
        h.track(&mut world, Some(g), false, None);
        crate::play::stop(&mut world).unwrap();
        assert!(find(&mut world, "Bullet").is_none());
        let a = find(&mut world, "A2").unwrap();
        // A stale selection (respawned entity) and the new one are both fine.
        h.track(&mut world, Some(g), false, None);
        h.track(&mut world, Some(a), false, None);
        h.track(&mut world, Some(a), false, None);
        assert_eq!(
            h.labels().collect::<Vec<_>>(),
            ["Rename A"],
            "no bogus step"
        );
        assert_eq!(root_names(&mut world), ["A2", "B"]);
        assert_eq!(body_count(&world), 1);
        h.undo(&mut world).unwrap();
        assert_eq!(root_names(&mut world), ["A", "B"]);
        assert_eq!(body_count(&world), 1);
        h.redo(&mut world).unwrap();
        assert_eq!(root_names(&mut world), ["A2", "B"]);
    }

    #[test]
    fn an_idle_selected_physics_entity_records_nothing() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        world.entity_mut(a).insert(dynamic());
        melon_physics::sync_components_system(&mut world);
        for _ in 0..5 {
            h.track(&mut world, Some(a), false, None);
        }
        assert!(!h.can_undo());
    }

    /// `cargo test -p melon_editor --lib track_cost -- --ignored --nocapture`
    #[test]
    #[ignore = "measurement"]
    fn track_cost() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 0.0);
        world.entity_mut(a).insert((
            dynamic(),
            melon_core::Aabb {
                half_extents: Vec3::ONE,
            },
            Spin,
            melon_core::Bob,
            melon_core::PreviousTransform(Transform::default()),
        ));
        melon_physics::sync_components_system(&mut world);
        h.track(&mut world, Some(a), false, None);
        let n = 2000;
        let start = std::time::Instant::now();
        for _ in 0..n {
            h.track(&mut world, Some(a), false, None);
        }
        let per = start.elapsed().as_secs_f64() * 1e6 / n as f64;
        println!("track: {per:.1} us per frame");
    }

    #[test]
    fn unused_name_starts_with_the_bare_name_then_numbers() {
        let mut world = world();
        assert_eq!(unused_name(&mut world, None, "Cube", true), "Cube");
        spawn(&mut world, "Cube", 0.0);
        assert_eq!(unused_name(&mut world, None, "Cube", true), "Cube (1)");
        spawn(&mut world, "Cube (1)", 0.0);
        assert_eq!(unused_name(&mut world, None, "Cube", true), "Cube (2)");
        let p = spawn(&mut world, "P", 0.0);
        assert_eq!(unused_name(&mut world, Some(p), "Cube", true), "Cube");
        assert_eq!(unused_name(&mut world, Some(p), "Cube", false), "Cube (1)");
    }

    #[test]
    fn undo_keeps_a_pending_edit_as_its_own_step() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 1.0);
        h.track(&mut world, Some(a), false, None);
        world.get_mut::<Transform>(a).unwrap().translation.x = 5.0;
        h.track(&mut world, Some(a), true, None); // still typing
        h.undo(&mut world).unwrap();
        assert_eq!(x_of(&world, a), 1.0, "the pending edit was undone");
        assert!(h.can_redo(), "and is not lost");
        h.redo(&mut world).unwrap();
        assert_eq!(x_of(&world, a), 5.0);
    }

    #[test]
    fn explicit_operations_keep_a_pending_edit_first() {
        let mut world = world();
        let mut h = History::default();
        let a = spawn(&mut world, "A", 1.0);
        h.track(&mut world, Some(a), false, None);
        world.get_mut::<Transform>(a).unwrap().translation.x = 5.0;
        h.track(&mut world, Some(a), true, None);
        h.rename(&mut world, a, "B", Some(a)).unwrap();
        assert_eq!(
            h.labels().collect::<Vec<_>>(),
            ["Edit Transform", "Rename A"]
        );
        h.undo(&mut world).unwrap();
        assert_eq!((x_of(&world, a), name_of(&world, a)), (5.0, "A".into()));
    }
}
