//! Parent / child entities. A child's `Transform` is local to its parent;
//! every world-pose question goes through this module.

use std::collections::HashMap;

use bevy_ecs::component::Component;
pub use bevy_ecs::entity::Entity;
pub use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::query::{With, Without};
use bevy_ecs::world::World;
use glam::Mat4;

use crate::{Name, PendingDelete, PreviousTransform, Transform};

/// World matrix, written from the `Transform` chain by
/// `propagate_transforms_system`.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct GlobalTransform(pub Mat4);

impl Default for GlobalTransform {
    fn default() -> Self {
        Self(Mat4::IDENTITY)
    }
}

/// Inverse, or identity for a non-invertible matrix (zero scale).
pub fn safe_inverse(m: Mat4) -> Mat4 {
    if m.determinant().abs() < 1e-12 {
        Mat4::IDENTITY
    } else {
        m.inverse()
    }
}

fn parent_of(world: &World, entity: Entity) -> Option<Entity> {
    world.get::<ChildOf>(entity).map(ChildOf::parent)
}

/// World matrix computed now by walking `ChildOf` up. Identity for an entity
/// without `Transform`.
pub fn world_matrix(world: &World, entity: Entity) -> Mat4 {
    let mut m = Mat4::IDENTITY;
    let mut current = Some(entity);
    // Bounded so a malformed cycle can't hang the editor.
    for _ in 0..1024 {
        let Some(e) = current else { break };
        if let Some(t) = world.get::<Transform>(e) {
            m = t.to_mat4() * m;
        }
        current = parent_of(world, e);
    }
    m
}

/// Local transform that puts `entity` at `world_pose`, given its parent.
/// `None` if the result isn't finite.
fn local_for(world: &World, parent: Option<Entity>, world_pose: Mat4) -> Option<Transform> {
    let parent_world = parent.map_or(Mat4::IDENTITY, |p| world_matrix(world, p));
    let t = Transform::from_mat4(safe_inverse(parent_world) * world_pose);
    (t.translation.is_finite() && t.rotation.is_finite() && t.scale.is_finite()).then_some(t)
}

/// Move `entity` to `world_pose` by writing its local `Transform`.
pub fn set_world_transform(world: &mut World, entity: Entity, world_pose: Transform) {
    let parent = parent_of(world, entity);
    if let Some(local) = local_for(world, parent, world_pose.to_mat4())
        && let Some(mut t) = world.get_mut::<Transform>(entity)
    {
        *t = local;
    }
}

fn is_ancestor(world: &World, ancestor: Entity, entity: Entity) -> bool {
    let mut current = Some(entity);
    for _ in 0..1024 {
        match current {
            Some(e) if e == ancestor => return true,
            Some(e) => current = parent_of(world, e),
            None => return false,
        }
    }
    true
}

/// Make `child` a child of `new_parent` (or a root for `None`) without moving
/// it in the world. Refuses a parent that is the child itself or below it.
pub fn set_parent_keep_world(
    world: &mut World,
    child: Entity,
    new_parent: Option<Entity>,
) -> Result<(), String> {
    let name = |e: Entity| {
        world
            .get::<Name>(e)
            .map_or_else(|| format!("{e:?}"), |n| n.0.clone())
    };
    if let Some(p) = new_parent
        && is_ancestor(world, child, p)
    {
        return Err(format!(
            "can't put {} under itself or one of its children",
            name(child)
        ));
    }
    let local = local_for(world, new_parent, world_matrix(world, child))
        .ok_or_else(|| format!("can't put {} under a parent with zero scale", name(child)))?;
    let mut entity = world
        .get_entity_mut(child)
        .map_err(|_| "entity no longer exists".to_string())?;
    entity.insert(local);
    match new_parent {
        Some(p) => entity.insert(ChildOf(p)),
        None => entity.remove::<ChildOf>(),
    };
    Ok(())
}

/// Every `Transform` entity reachable from a root, with its world matrix.
/// `local` gives each link's local matrix.
fn walk(world: &mut World, local: impl Fn(&World, Entity) -> Option<Mat4>) -> Vec<(Entity, Mat4)> {
    let roots: Vec<Entity> = world
        .query_filtered::<Entity, (With<Transform>, Without<ChildOf>)>()
        .iter(world)
        .collect();
    let world: &World = world;
    let mut stack: Vec<(Entity, Mat4)> = roots
        .into_iter()
        .rev()
        .map(|e| (e, Mat4::IDENTITY))
        .collect();
    let mut out = Vec::new();
    while let Some((e, parent)) = stack.pop() {
        let Some(l) = local(world, e) else { continue };
        let m = parent * l;
        out.push((e, m));
        if let Some(children) = world.get::<Children>(e) {
            stack.extend(children.iter().rev().map(|&c| (c, m)));
        }
    }
    out
}

/// Write `GlobalTransform` for every entity with a `Transform`.
pub fn propagate_transforms_system(world: &mut World) {
    let matrices = walk(world, |w, e| w.get::<Transform>(e).map(Transform::to_mat4));
    for (e, m) in matrices {
        world.entity_mut(e).insert(GlobalTransform(m));
    }
}

/// World matrices to draw this frame. With `blend`, every link of the chain
/// is blended with its `PreviousTransform` by `alpha`.
pub fn render_matrices(world: &mut World, alpha: f32, blend: bool) -> HashMap<Entity, Mat4> {
    walk(world, |w, e| {
        let t = w.get::<Transform>(e)?;
        Some(match w.get::<PreviousTransform>(e) {
            Some(prev) if blend => t.interpolate(&prev.0, alpha).to_mat4(),
            _ => t.to_mat4(),
        })
    })
    .into_iter()
    .collect()
}

/// Order of the scene's root entities (the Hierarchy's top level and the
/// scene file's root order). Healed by `root_order`: dead or non-root
/// entities drop out, unlisted roots are appended.
#[derive(bevy_ecs::resource::Resource, Default)]
pub struct RootOrder(pub Vec<Entity>);

/// Scene roots (`Name` + `Transform`, no parent, not being deleted) in order.
pub fn root_order(world: &mut World) -> Vec<Entity> {
    let found: Vec<Entity> = world
        .query_filtered::<Entity, (
            With<Name>,
            With<Transform>,
            Without<ChildOf>,
            Without<PendingDelete>,
        )>()
        .iter(world)
        .collect();
    let live: std::collections::HashSet<Entity> = found.iter().copied().collect();
    let mut order = world.remove_resource::<RootOrder>().unwrap_or_default().0;
    order.retain(|e| live.contains(e));
    let known: std::collections::HashSet<Entity> = order.iter().copied().collect();
    order.extend(found.into_iter().filter(|e| !known.contains(e)));
    world.insert_resource(RootOrder(order.clone()));
    order
}

/// Position of a root among the roots (`None` if it is not one).
pub fn root_index(world: &mut World, entity: Entity) -> Option<usize> {
    root_order(world).iter().position(|&e| e == entity)
}

/// Put root `entity` at `index` among the roots (clamped).
pub fn move_root(world: &mut World, entity: Entity, index: usize) {
    let mut order = root_order(world);
    order.retain(|&e| e != entity);
    order.insert(index.min(order.len()), entity);
    world.insert_resource(RootOrder(order));
}

/// Scene entities (`Name` + `Transform`, not being deleted) depth-first:
/// parents before children, siblings in `Children` order. `usize` is depth.
pub fn depth_first(world: &mut World) -> Vec<(Entity, usize)> {
    let roots = root_order(world);
    let world: &World = world;
    let listed = |e: Entity| {
        world.get::<Name>(e).is_some()
            && world.get::<Transform>(e).is_some()
            && world.get::<PendingDelete>(e).is_none()
    };
    let mut stack: Vec<(Entity, usize)> = roots.into_iter().rev().map(|e| (e, 0)).collect();
    let mut out = Vec::new();
    while let Some((e, depth)) = stack.pop() {
        out.push((e, depth));
        if let Some(children) = world.get::<Children>(e) {
            stack.extend(
                children
                    .iter()
                    .rev()
                    .filter(|&&c| listed(c))
                    .map(|&c| (c, depth + 1)),
            );
        }
    }
    out
}

/// Mark `entity` and every descendant `PendingDelete`.
pub fn mark_for_delete(world: &mut World, entity: Entity) {
    let mut stack = vec![entity];
    while let Some(e) = stack.pop() {
        let Ok(mut entity_mut) = world.get_entity_mut(e) else {
            continue;
        };
        entity_mut.insert(PendingDelete);
        if let Some(children) = world.get::<Children>(e) {
            stack.extend(children.iter());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Name, PendingDelete, PreviousTransform, Transform};
    use bevy_ecs::world::World;
    use glam::{Mat4, Quat, Vec3};
    use std::f32::consts::FRAC_PI_2;

    fn approx(a: Vec3, b: Vec3) -> bool {
        (a - b).abs().max_element() < 1e-4
    }

    fn spawn(world: &mut World, name: &str, t: Transform, parent: Option<Entity>) -> Entity {
        let e = world.spawn((Name(name.to_string()), t)).id();
        if let Some(p) = parent {
            world.entity_mut(e).insert(ChildOf(p));
        }
        e
    }

    fn chain(world: &mut World) -> (Entity, Entity, Entity) {
        let root = spawn(world, "Root", Transform::from_translation(Vec3::X), None);
        let child = spawn(
            world,
            "Child",
            Transform {
                rotation: Quat::from_rotation_y(FRAC_PI_2),
                ..Default::default()
            },
            Some(root),
        );
        let grand = spawn(
            world,
            "Grand",
            Transform::from_translation(Vec3::new(0.0, 0.0, 2.0)),
            Some(child),
        );
        (root, child, grand)
    }

    fn origin(m: Mat4) -> Vec3 {
        m.transform_point3(Vec3::ZERO)
    }

    #[test]
    fn grandchild_world_is_product_of_chain() {
        let mut world = World::new();
        let (_, _, grand) = chain(&mut world);
        assert!(approx(
            origin(world_matrix(&world, grand)),
            Vec3::new(3.0, 0.0, 0.0)
        ));
    }

    #[test]
    fn propagation_follows_moved_root() {
        let mut world = World::new();
        let (root, _, grand) = chain(&mut world);
        propagate_transforms_system(&mut world);
        world.get_mut::<Transform>(root).unwrap().translation = Vec3::new(5.0, 0.0, 0.0);
        propagate_transforms_system(&mut world);
        let g = world.get::<GlobalTransform>(grand).unwrap().0;
        assert!(approx(g.w_axis.truncate(), Vec3::new(7.0, 0.0, 0.0)));
    }

    #[test]
    fn reparent_keeps_world_pose() {
        let mut world = World::new();
        let parent = spawn(
            &mut world,
            "Parent",
            Transform {
                translation: Vec3::new(2.0, 0.0, 0.0),
                rotation: Quat::from_rotation_y(FRAC_PI_2 / 2.0),
                scale: Vec3::splat(2.0),
            },
            None,
        );
        let child = spawn(
            &mut world,
            "Child",
            Transform::from_translation(Vec3::ONE),
            None,
        );
        let before = world_matrix(&world, child);
        set_parent_keep_world(&mut world, child, Some(parent)).unwrap();
        assert_eq!(world.get::<ChildOf>(child).unwrap().parent(), parent);
        let after = world_matrix(&world, child);
        assert!(before.abs_diff_eq(after, 1e-4), "{before:?} vs {after:?}");
    }

    #[test]
    fn reparent_to_none_keeps_world_pose_and_removes_childof() {
        let mut world = World::new();
        let (_, _, grand) = chain(&mut world);
        let before = world_matrix(&world, grand);
        set_parent_keep_world(&mut world, grand, None).unwrap();
        assert!(world.get::<ChildOf>(grand).is_none());
        assert!(before.abs_diff_eq(world_matrix(&world, grand), 1e-4));
    }

    #[test]
    fn reparent_onto_self_or_descendant_errors_and_changes_nothing() {
        let mut world = World::new();
        let (root, child, grand) = chain(&mut world);
        let root_t = *world.get::<Transform>(root).unwrap();
        assert!(set_parent_keep_world(&mut world, root, Some(root)).is_err());
        assert!(set_parent_keep_world(&mut world, root, Some(grand)).is_err());
        assert!(world.get::<ChildOf>(root).is_none());
        assert_eq!(
            world.get::<Transform>(root).unwrap().translation,
            root_t.translation
        );
        assert_eq!(world.get::<ChildOf>(grand).unwrap().parent(), child);
    }

    #[test]
    fn reparent_under_zero_scale_parent_writes_finite_values() {
        let mut world = World::new();
        let parent = spawn(
            &mut world,
            "Flat",
            Transform {
                scale: Vec3::new(0.0, 1.0, 1.0),
                ..Default::default()
            },
            None,
        );
        let child = spawn(
            &mut world,
            "Child",
            Transform::from_translation(Vec3::ONE),
            None,
        );
        let _ = set_parent_keep_world(&mut world, child, Some(parent));
        let t = world.get::<Transform>(child).unwrap();
        assert!(t.translation.is_finite() && t.scale.is_finite() && t.rotation.is_finite());
    }

    #[test]
    fn depth_first_lists_parents_before_children_in_sibling_order() {
        let mut world = World::new();
        let a = spawn(&mut world, "A", Transform::default(), None);
        let b = spawn(&mut world, "B", Transform::default(), Some(a));
        let c = spawn(&mut world, "C", Transform::default(), Some(a));
        let d = spawn(&mut world, "D", Transform::default(), Some(b));
        assert_eq!(depth_first(&mut world), [(a, 0), (b, 1), (d, 2), (c, 1)]);
    }

    #[test]
    fn mark_for_delete_marks_subtree_only() {
        let mut world = World::new();
        let (root, child, grand) = chain(&mut world);
        let other = spawn(&mut world, "Other", Transform::default(), None);
        mark_for_delete(&mut world, child);
        assert!(world.get::<PendingDelete>(root).is_none());
        assert!(world.get::<PendingDelete>(child).is_some());
        assert!(world.get::<PendingDelete>(grand).is_some());
        assert!(world.get::<PendingDelete>(other).is_none());
    }

    #[test]
    fn render_matrices_interpolate_each_link() {
        let mut world = World::new();
        let parent = spawn(
            &mut world,
            "P",
            Transform::from_translation(Vec3::new(2.0, 0.0, 0.0)),
            None,
        );
        world
            .entity_mut(parent)
            .insert(PreviousTransform(Transform::default()));
        let child = spawn(
            &mut world,
            "C",
            Transform::from_translation(Vec3::new(0.0, 2.0, 0.0)),
            Some(parent),
        );
        world
            .entity_mut(child)
            .insert(PreviousTransform(Transform::default()));
        let blended = render_matrices(&mut world, 0.5, true);
        assert!(approx(origin(blended[&child]), Vec3::new(1.0, 1.0, 0.0)));
        let current = render_matrices(&mut world, 0.5, false);
        assert!(approx(origin(current[&child]), Vec3::new(2.0, 2.0, 0.0)));
    }
}
