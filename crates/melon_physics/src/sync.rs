//! Make rapier match the data components: `sync_components` creates, updates
//! and removes bodies, colliders and joints for `RigidBody`, `Collider` and
//! `Joint`. Only what changed is looked at: entities whose `Transform`,
//! physics component, `ChildOf` or model changed (and everything below them,
//! since their world poses moved), plus mesh colliders whose model file
//! changed. Each is compared with what it last built, so rapier is touched
//! only on a real difference. Removals arrive through component hooks
//! (`PhysicsWorld::removals`).

use std::collections::{HashMap, HashSet};

use bevy_ecs::entity::Entity;
use bevy_ecs::query::{Changed, Or, With, Without};
use bevy_ecs::world::World;
use glam::{Mat4, Quat, Vec3};
use rapier3d::dynamics::{
    GenericJointBuilder, ImpulseJointHandle, JointAxesMask, JointAxis, RigidBodyBuilder,
    RigidBodyHandle, RigidBodyType,
};
use rapier3d::geometry::ActiveCollisionTypes;
use rapier3d::geometry::{
    ColliderBuilder, ColliderHandle, Group, InteractionGroups, InteractionTestMode,
};
use rapier3d::math::Pose;
use rapier3d::pipeline::ActiveEvents;
use rapier3d::prelude::MassProperties;

use melon_core::hierarchy::world_matrix;
use melon_core::{
    AssetServer, CharacterController, ChildOf, Children, Collider, ColliderShape, Handle, Joint,
    JointKind, Model, PendingDelete, RigidBody, RigidBodyKind, Transform, Velocity,
};

use crate::mesh;
use crate::{ColliderComponent, JointComponent, PhysicsWorld, RigidBodyComponent};

/// What a collider was last built from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltCollider {
    collider: Collider,
    owner: Option<Entity>,
    scale: Vec3,
    /// Mass this collider gives its body (0 = by density).
    mass: f32,
    /// Which pairs are detected: 0 = rapier default (dynamic with anything);
    /// 1 = also those with fixed and kinematic colliders (sensors, kinematic
    /// bodies), so triggers and events fire for them; 2 = every pair, static
    /// with static too (a character that has no body).
    pairs: u8,
    /// A mesh collider's model (handle id) and file version.
    mesh: Option<(u32, u32)>,
    translation: Vec3,
    rotation: Quat,
}

/// What a joint was last built from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltJoint {
    joint: Joint,
    body: RigidBodyHandle,
    parent: RigidBodyHandle,
    scale: Vec3,
}

/// Smallest size a collider shape gets (metres).
const MIN_SIZE: f32 = 1e-3;

/// A size rapier can take: finite and at least `MIN_SIZE`.
fn size(v: f32) -> f32 {
    if v.is_finite() {
        v.max(MIN_SIZE)
    } else {
        MIN_SIZE
    }
}

/// `v` if finite and not below `floor`, else `fallback`.
fn at_least(v: f32, floor: f32, fallback: f32, dirty: &mut bool) -> f32 {
    if v.is_finite() && v >= floor {
        v
    } else {
        *dirty = true;
        fallback
    }
}

/// The collider with every number rapier cannot take (zero, negative or
/// non-finite sizes, non-finite friction) replaced, and whether any was.
/// Comparing the cleaned copy also stops a NaN from rebuilding every frame.
fn clean_collider(c: &Collider) -> (Collider, bool) {
    let mut dirty = false;
    let mut out = c.clone();
    let mut fix = |v: f32| {
        let s = size(v);
        dirty |= s != v;
        s
    };
    out.shape = match c.shape {
        ColliderShape::Box { half_extents: h } => ColliderShape::Box {
            half_extents: Vec3::new(fix(h.x), fix(h.y), fix(h.z)),
        },
        ColliderShape::Sphere { radius } => ColliderShape::Sphere {
            radius: fix(radius),
        },
        ColliderShape::Capsule {
            half_height,
            radius,
        } => ColliderShape::Capsule {
            half_height: fix(half_height),
            radius: fix(radius),
        },
        ColliderShape::Mesh => ColliderShape::Mesh,
    };
    out.friction = at_least(c.friction, 0.0, 0.5, &mut dirty);
    out.restitution = at_least(c.restitution, 0.0, 0.0, &mut dirty);
    (out, dirty)
}

/// The body with mass, damping and gravity scale rapier can take, and
/// whether any was replaced.
fn clean_body(rb: &RigidBody) -> (RigidBody, bool) {
    let mut dirty = false;
    let mut out = rb.clone();
    out.mass = at_least(rb.mass, 0.0, 0.0, &mut dirty);
    out.linear_damping = at_least(rb.linear_damping, 0.0, 0.0, &mut dirty);
    out.angular_damping = at_least(rb.angular_damping, 0.0, 0.0, &mut dirty);
    if !rb.gravity_scale.is_finite() {
        dirty = true;
        out.gravity_scale = 1.0;
    }
    (out, dirty)
}

fn body_type(kind: RigidBodyKind) -> RigidBodyType {
    match kind {
        RigidBodyKind::Dynamic => RigidBodyType::Dynamic,
        RigidBodyKind::Fixed => RigidBodyType::Fixed,
        RigidBodyKind::Kinematic => RigidBodyType::KinematicPositionBased,
    }
}

/// Translation, rotation and scale of an entity in the world.
fn world_pose(world: &World, e: Entity) -> (Vec3, Quat, Vec3) {
    let (scale, rotation, translation) = world_matrix(world, e).to_scale_rotation_translation();
    (translation, rotation.normalize(), scale)
}

fn parent_of(world: &World, e: Entity) -> Option<Entity> {
    world.get::<ChildOf>(e).map(ChildOf::parent)
}

/// `e` or its nearest ancestor that has a live rapier body.
fn body_owner(world: &World, physics: &PhysicsWorld, e: Entity) -> Option<Entity> {
    let mut current = Some(e);
    for _ in 0..1024 {
        let c = current?;
        if let Some(rb) = world.get::<RigidBodyComponent>(c)
            && physics.bodies.contains(rb.body_handle)
            && !world.entity(c).contains::<PendingDelete>()
        {
            return Some(c);
        }
        current = parent_of(world, c);
    }
    None
}

/// `e` and everything below it.
fn with_descendants(world: &World, e: Entity, out: &mut HashSet<Entity>) {
    let mut stack = vec![e];
    while let Some(c) = stack.pop() {
        if !out.insert(c) {
            continue;
        }
        if let Some(children) = world.get::<Children>(c) {
            stack.extend(children.iter());
        }
    }
}

/// The system: `sync_components` on the world's `PhysicsWorld`.
pub fn sync_components_system(world: &mut World) {
    world.try_resource_scope(|world, mut physics: bevy_ecs::world::Mut<PhysicsWorld>| {
        sync_components(world, &mut physics);
    });
}

/// Make `physics` match the physics components of `world`.
pub fn sync_components(world: &mut World, physics: &mut PhysicsWorld) {
    physics.stats.syncs += 1;
    let orphaned = remove_stale(world, physics);
    let dirty = dirty_entities(world, physics, orphaned);
    physics.stats.examined += dirty.len() as u32;
    if dirty.is_empty() {
        return;
    }
    sync_bodies(world, physics, &dirty);
    sync_colliders(world, physics, &dirty);
    sync_joints(world, physics, &dirty);
}

/// Entities to compare with rapier: what changed, what lies below it (its
/// world pose moved), and mesh colliders whose model file changed.
fn dirty_entities(
    world: &mut World,
    physics: &PhysicsWorld,
    orphaned: Vec<Entity>,
) -> HashSet<Entity> {
    let mut roots: Vec<Entity> = orphaned;
    type Moved = Or<(
        Changed<Transform>,
        Changed<Collider>,
        Changed<RigidBody>,
        Changed<Joint>,
        Changed<CharacterController>,
        Changed<ChildOf>,
        Changed<Handle<Model>>,
    )>;
    roots.extend(
        world
            .query_filtered::<Entity, (
                Or<(
                    With<Collider>,
                    With<RigidBody>,
                    With<Joint>,
                    With<CharacterController>,
                )>,
                Moved,
            )>()
            .iter(world),
    );
    // Something above the physics entities moved or was reparented.
    roots.extend(
        world
            .query_filtered::<Entity, (With<Children>, Or<(Changed<Transform>, Changed<ChildOf>)>)>(
            )
            .iter(world),
    );
    if let Some(server) = world.get_resource::<AssetServer>() {
        for &e in &physics.mesh_colliders {
            let stale = match (
                world.get::<ColliderComponent>(e),
                world.get::<Handle<Model>>(e),
            ) {
                (Some(c), Some(h)) => c.built.mesh != Some((h.id(), server.version(*h))),
                (Some(c), None) => c.built.mesh.is_some(),
                _ => false,
            };
            if stale {
                roots.push(e);
            }
        }
    }
    let mut set = HashSet::new();
    for e in roots {
        if world.get_entity(e).is_ok() {
            with_descendants(world, e, &mut set);
        }
    }
    set
}

/// Drop rapier objects whose component was removed (or whose entity was
/// despawned: the hooks queued them). Returns entities that lost their body,
/// so the colliders below them are looked at again.
fn remove_stale(world: &mut World, physics: &mut PhysicsWorld) -> Vec<Entity> {
    physics.flush_removals();
    let gone_joints: Vec<(Entity, ImpulseJointHandle)> = world
        .query_filtered::<(Entity, &JointComponent), Without<Joint>>()
        .iter(world)
        .map(|(e, j)| (e, j.handle))
        .collect();
    for (e, h) in gone_joints {
        physics.remove_joint(h);
        world.entity_mut(e).remove::<JointComponent>();
        physics.joint_warned.remove(&e);
    }
    let gone_bodies: Vec<(Entity, RigidBodyHandle)> = world
        .query_filtered::<(Entity, &RigidBodyComponent), Without<RigidBody>>()
        .iter(world)
        .map(|(e, b)| (e, b.body_handle))
        .collect();
    let mut orphaned = Vec::new();
    for (e, h) in gone_bodies {
        physics.remove_body(h);
        world
            .entity_mut(e)
            .remove::<(RigidBodyComponent, Velocity)>();
        orphaned.push(e);
    }
    let gone_colliders: Vec<(Entity, Option<ColliderHandle>)> = world
        .query_filtered::<(Entity, &ColliderComponent), Without<Collider>>()
        .iter(world)
        .map(|(e, c)| (e, c.collider_handle))
        .collect();
    for (e, h) in gone_colliders {
        if let Some(h) = h {
            physics.remove_collider(h);
        }
        world.entity_mut(e).remove::<ColliderComponent>();
        physics.mesh_colliders.remove(&e);
    }
    orphaned
}

fn sync_bodies(world: &mut World, physics: &mut PhysicsWorld, dirty: &HashSet<Entity>) {
    let items: Vec<(Entity, RigidBody)> = dirty
        .iter()
        .filter(|e| !world.entity(**e).contains::<PendingDelete>())
        .filter_map(|e| world.get::<RigidBody>(*e).map(|rb| (*e, rb.clone())))
        .collect();
    for (e, rb) in items {
        let (rb, dirty) = clean_body(&rb);
        let existing = world
            .get::<RigidBodyComponent>(e)
            .filter(|c| physics.bodies.contains(c.body_handle))
            .map(|c| (c.body_handle, c.built.clone()));
        match existing {
            Some((_, built)) if built == rb => {}
            Some((handle, built)) => {
                if dirty {
                    tracing::warn!(
                        "rigid body of entity {e}: invalid mass, damping or gravity scale replaced"
                    );
                }
                let body = physics.bodies.get_mut(handle).expect("checked above");
                if built.kind != rb.kind {
                    body.set_body_type(body_type(rb.kind), true);
                }
                body.set_linear_damping(rb.linear_damping);
                body.set_angular_damping(rb.angular_damping);
                body.set_gravity_scale(rb.gravity_scale, true);
                body.enable_ccd(rb.ccd);
                body.set_enabled_rotations(
                    !rb.lock_rotation_x,
                    !rb.lock_rotation_y,
                    !rb.lock_rotation_z,
                    true,
                );
                let mut c = world.get_mut::<RigidBodyComponent>(e).expect("checked");
                c.body_type = body_type(rb.kind);
                c.built = rb.clone();
                if rb.kind == RigidBodyKind::Dynamic {
                    if !world.entity(e).contains::<Velocity>() {
                        world.entity_mut(e).insert(Velocity::default());
                    }
                } else {
                    world.entity_mut(e).remove::<Velocity>();
                }
            }
            None => {
                if dirty {
                    tracing::warn!(
                        "rigid body of entity {e}: invalid mass, damping or gravity scale replaced"
                    );
                }
                physics.stats.body_builds += 1;
                let (translation, rotation, _) = world_pose(world, e);
                let builder = match rb.kind {
                    RigidBodyKind::Dynamic => RigidBodyBuilder::dynamic(),
                    RigidBodyKind::Fixed => RigidBodyBuilder::fixed(),
                    RigidBodyKind::Kinematic => RigidBodyBuilder::kinematic_position_based(),
                };
                let body = builder
                    .pose(Pose::from_parts(translation, rotation))
                    .linear_damping(rb.linear_damping)
                    .angular_damping(rb.angular_damping)
                    .gravity_scale(rb.gravity_scale)
                    .ccd_enabled(rb.ccd)
                    .enabled_rotations(
                        !rb.lock_rotation_x,
                        !rb.lock_rotation_y,
                        !rb.lock_rotation_z,
                    )
                    .user_data(e.to_bits() as u128)
                    .build();
                let body_handle = physics.bodies.insert(body);
                let dynamic = rb.kind == RigidBodyKind::Dynamic;
                let mut entity = world.entity_mut(e);
                entity.insert(RigidBodyComponent {
                    body_handle,
                    body_type: body_type(rb.kind),
                    built: rb,
                });
                if dynamic && !entity.contains::<Velocity>() {
                    entity.insert(Velocity::default());
                }
            }
        }
    }
}

/// A collider's pose (in the world, or in its body's frame) and the world
/// scale its size follows.
fn collider_pose(world: &World, e: Entity, owner: Option<Entity>) -> (Vec3, Quat, Vec3) {
    let (t, r, scale) = world_pose(world, e);
    match owner {
        Some(o) => {
            let (ot, or, _) = world_pose(world, o);
            let rel = Mat4::from_rotation_translation(or, ot).inverse()
                * Mat4::from_rotation_translation(r, t);
            let (_, rr, rt) = rel.to_scale_rotation_translation();
            (rt, rr.normalize(), scale)
        }
        None => (t, r, scale),
    }
}

/// Equal within a tolerance relative to the magnitude (single precision
/// loses absolute accuracy far from the origin).
pub(crate) fn close(a: Vec3, b: Vec3) -> bool {
    let magnitude = a.abs().max_element().max(b.abs().max_element());
    (a - b).abs().max_element() <= 1e-5 * (1.0 + magnitude)
}

/// Rotations equal within the float noise of a quaternion product.
fn close_rotation(a: Quat, b: Quat) -> bool {
    (a.dot(b).abs() - 1.0).abs() <= 1e-6
}

#[allow(clippy::too_many_arguments)]
fn build_collider(
    world: &World,
    e: Entity,
    c: &Collider,
    scale: Vec3,
    mass: f32,
    moving: bool,
    pairs: u8,
    pose: Pose,
) -> Option<rapier3d::geometry::Collider> {
    let s = Vec3::new(
        size(scale.x.abs()),
        size(scale.y.abs()),
        size(scale.z.abs()),
    );
    let min = size;
    let builder = match c.shape {
        ColliderShape::Box { half_extents } => ColliderBuilder::cuboid(
            min(half_extents.x * s.x),
            min(half_extents.y * s.y),
            min(half_extents.z * s.z),
        ),
        ColliderShape::Sphere { radius } => ColliderBuilder::ball(min(radius * s.max_element())),
        ColliderShape::Capsule {
            half_height,
            radius,
        } => ColliderBuilder::capsule_y(min(half_height * s.y), min(radius * s.x.max(s.z))),
        ColliderShape::Mesh => {
            let (vertices, indices) = mesh::triangles(world, e)?;
            let vertices: Vec<Vec3> = vertices.into_iter().map(|v| v * s).collect();
            let (lo, hi) = vertices.iter().fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(lo, hi), v| (lo.min(*v), hi.max(*v)),
            );
            let builder = match ColliderBuilder::trimesh(vertices, indices) {
                Ok(b) => b,
                Err(error) => {
                    tracing::warn!("mesh collider of entity {e}: {error}");
                    return None;
                }
            };
            if moving {
                // A triangle mesh has no volume: mass and inertia come from its box.
                tracing::warn!(
                    "mesh collider of entity {e} on a moving body: mass is taken from its bounding box"
                );
                let half = ((hi - lo) * 0.5).max(Vec3::splat(MIN_SIZE));
                let volume = 8.0 * half.x * half.y * half.z;
                let density = if mass > 0.0 { mass / volume } else { 1.0 };
                builder.mass_properties(
                    MassProperties::from_cuboid(density, half)
                        .transform_by(&Pose::from_translation((lo + hi) * 0.5)),
                )
            } else {
                builder
            }
        }
    };
    let mut builder = builder
        .position(pose)
        .friction(c.friction)
        .restitution(c.restitution)
        .sensor(c.sensor)
        .collision_groups(InteractionGroups::new(
            Group::from_bits_retain(c.membership),
            Group::from_bits_retain(c.filter),
            InteractionTestMode::And,
        ))
        .active_events(ActiveEvents::COLLISION_EVENTS)
        .user_data(e.to_bits() as u128);
    match pairs {
        0 => {}
        1 => {
            builder = builder.active_collision_types(
                ActiveCollisionTypes::all() - ActiveCollisionTypes::FIXED_FIXED,
            )
        }
        _ => builder = builder.active_collision_types(ActiveCollisionTypes::all()),
    }
    if mass > 0.0 && c.shape != ColliderShape::Mesh {
        builder = builder.mass(mass);
    }
    Some(builder.build())
}

fn sync_colliders(world: &mut World, physics: &mut PhysicsWorld, dirty: &HashSet<Entity>) {
    // A body's colliders share its mass, so one changing looks at them all.
    let mut set: HashSet<Entity> = HashSet::new();
    for &e in dirty {
        if world.get::<Collider>(e).is_some() {
            set.insert(e);
            if let Some(o) = body_owner(world, physics, e) {
                with_descendants(world, o, &mut set);
            }
        }
    }
    let items: Vec<(Entity, Collider)> = set
        .iter()
        .filter(|e| !world.entity(**e).contains::<PendingDelete>())
        .filter_map(|e| world.get::<Collider>(*e).map(|c| (*e, c.clone())))
        .collect();
    let owners: Vec<Option<Entity>> = items
        .iter()
        .map(|(e, _)| body_owner(world, physics, *e))
        .collect();
    // Solid colliders per body, over its whole subtree.
    let mut solid: HashMap<Entity, usize> = HashMap::new();
    for o in owners.iter().flatten().collect::<HashSet<_>>() {
        let mut subtree = HashSet::new();
        with_descendants(world, *o, &mut subtree);
        let n = subtree
            .iter()
            .filter(|c| {
                world.get::<Collider>(**c).is_some_and(|c| !c.sensor)
                    && body_owner(world, physics, **c) == Some(*o)
            })
            .count();
        solid.insert(*o, n);
    }
    for ((e, c), owner) in items.into_iter().zip(owners) {
        let (c, dirty) = clean_collider(&c);
        let (translation, rotation, scale) = collider_pose(world, e, owner);
        let owner_kind = owner
            .and_then(|o| world.get::<RigidBodyComponent>(o))
            .map(|rb| rb.built.kind);
        let mass = owner
            .filter(|_| !c.sensor)
            .and_then(|o| {
                let rb = &world.get::<RigidBodyComponent>(o)?.built;
                (rb.kind == RigidBodyKind::Dynamic && rb.mass > 0.0)
                    .then(|| rb.mass / solid[&o].max(1) as f32)
            })
            .unwrap_or(0.0);
        let pairs = if world.get::<CharacterController>(e).is_some() {
            2
        } else if c.sensor || owner_kind == Some(RigidBodyKind::Kinematic) {
            1
        } else {
            0
        };
        let mesh_key = (c.shape == ColliderShape::Mesh).then(|| {
            world
                .get::<Handle<Model>>(e)
                .zip(world.get_resource::<AssetServer>())
                .map_or((u32::MAX, 0), |(h, s)| (h.id(), s.version(*h)))
        });
        let built = BuiltCollider {
            collider: c.clone(),
            owner,
            scale,
            mass,
            pairs,
            mesh: mesh_key,
            translation,
            rotation,
        };
        let existing = world
            .get::<ColliderComponent>(e)
            .map(|cc| (cc.collider_handle, cc.built.clone()));
        let pose = Pose::from_parts(translation, rotation);
        if let Some((handle, old)) = &existing {
            let alive = handle.is_none_or(|h| physics.colliders().contains(h));
            let same_shape = old.collider == built.collider
                && old.owner == built.owner
                && old.mass == built.mass
                && old.pairs == built.pairs
                && old.mesh == built.mesh
                && close(old.scale, built.scale);
            if alive && same_shape {
                if !close(old.translation, translation) || !close_rotation(old.rotation, rotation) {
                    if let Some(h) = handle {
                        physics.set_collider_pose(*h, pose, owner.is_some());
                        physics.stats.collider_moves += 1;
                    }
                    world.get_mut::<ColliderComponent>(e).expect("exists").built = built;
                }
                continue;
            }
            if let Some(h) = handle {
                physics.remove_collider(*h);
            }
        }
        if dirty {
            tracing::warn!("collider of entity {e}: size or friction out of range, replaced");
        }
        physics.stats.collider_builds += 1;
        let moving = owner_kind.is_some_and(|k| k != RigidBodyKind::Fixed);
        let handle = build_collider(world, e, &c, scale, mass, moving, pairs, pose).map(|co| {
            let parent = owner.map(|o| {
                world
                    .get::<RigidBodyComponent>(o)
                    .expect("owner")
                    .body_handle
            });
            physics.insert_collider(co, parent)
        });
        if built.mesh.is_some() {
            physics.mesh_colliders.insert(e);
        } else {
            physics.mesh_colliders.remove(&e);
        }
        world.entity_mut(e).insert(ColliderComponent {
            collider_handle: handle,
            built,
        });
    }
}

fn sync_joints(world: &mut World, physics: &mut PhysicsWorld, dirty: &HashSet<Entity>) {
    let items: Vec<(Entity, Joint)> = dirty
        .iter()
        .filter(|e| !world.entity(**e).contains::<PendingDelete>())
        .filter_map(|e| world.get::<Joint>(*e).map(|j| (*e, j.clone())))
        .collect();
    for (e, joint) in items {
        let Some(body) = world
            .get::<RigidBodyComponent>(e)
            .map(|b| b.body_handle)
            .filter(|h| physics.bodies.contains(*h))
        else {
            if physics.joint_warned.insert(e) {
                tracing::warn!("joint of entity {e} is ignored: the entity has no RigidBody");
            }
            continue;
        };
        physics.joint_warned.remove(&e);
        let owner = parent_of(world, e).and_then(|p| body_owner(world, physics, p));
        let parent_body = owner
            .and_then(|o| world.get::<RigidBodyComponent>(o).map(|b| b.body_handle))
            .unwrap_or_else(|| physics.world_body());
        let (_, _, scale) = world_pose(world, e);
        let parent_scale = owner.map_or(Vec3::ONE, |o| world_pose(world, o).2);
        let built = BuiltJoint {
            joint: joint.clone(),
            body,
            parent: parent_body,
            scale,
        };
        if let Some(existing) = world.get::<JointComponent>(e) {
            if existing.built == built && physics.impulse_joints.contains(existing.handle) {
                continue;
            }
            physics.remove_joint(existing.handle);
        }
        physics.stats.joint_builds += 1;
        let pose_of = |h: RigidBodyHandle| {
            physics
                .bodies
                .get(h)
                .map_or((Vec3::ZERO, Quat::IDENTITY), |b| {
                    (b.translation(), *b.rotation())
                })
        };
        let (pt, pr) = pose_of(parent_body);
        let (ct, cr) = pose_of(body);
        let relative = pr.inverse() * cr;
        let clean = |v: Vec3| if v.is_finite() { v } else { Vec3::ZERO };
        let anchor2 = clean(joint.anchor) * scale;
        let anchor1 = if joint.auto_anchor {
            // The ancestor side of the pivot, so the pair keeps its pose now.
            pr.inverse() * (ct + cr * anchor2 - pt)
        } else {
            clean(joint.parent_anchor) * parent_scale
        };
        let frames = |axis: Vec3| {
            let q = Quat::from_rotation_arc(Vec3::X, axis.try_normalize().unwrap_or(Vec3::Y));
            (
                Pose::from_parts(anchor1, relative * q),
                Pose::from_parts(anchor2, q),
            )
        };
        let generic: rapier3d::dynamics::GenericJoint = match joint.kind {
            JointKind::Fixed => {
                let (f1, f2) = (
                    Pose::from_parts(anchor1, relative),
                    Pose::from_parts(anchor2, Quat::IDENTITY),
                );
                GenericJointBuilder::new(JointAxesMask::LOCKED_FIXED_AXES)
                    .local_frame1(f1)
                    .local_frame2(f2)
                    .build()
            }
            JointKind::Hinge {
                axis,
                limited,
                min,
                max,
            } => {
                let (f1, f2) = frames(axis);
                let mut b = GenericJointBuilder::new(JointAxesMask::LOCKED_REVOLUTE_AXES)
                    .local_frame1(f1)
                    .local_frame2(f2);
                if limited && min.is_finite() && max.is_finite() {
                    b = b.limits(JointAxis::AngX, [min.min(max), min.max(max)]);
                }
                b.build()
            }
            JointKind::Slider {
                axis,
                limited,
                min,
                max,
            } => {
                let (f1, f2) = frames(axis);
                let mut b = GenericJointBuilder::new(JointAxesMask::LOCKED_PRISMATIC_AXES)
                    .local_frame1(f1)
                    .local_frame2(f2);
                if limited && min.is_finite() && max.is_finite() {
                    b = b.limits(JointAxis::LinX, [min.min(max), min.max(max)]);
                }
                b.build()
            }
            JointKind::Ball {
                limited,
                cone_limit,
            } => {
                let (f1, f2) = frames(Vec3::X);
                let mut b = GenericJointBuilder::new(JointAxesMask::LOCKED_SPHERICAL_AXES)
                    .local_frame1(f1)
                    .local_frame2(f2);
                if limited && cone_limit.is_finite() {
                    // Swing about the Y and Z axes bounded; twist about X free.
                    let cone = [-cone_limit.abs(), cone_limit.abs()];
                    b = b
                        .limits(JointAxis::AngY, cone)
                        .limits(JointAxis::AngZ, cone);
                }
                b.build()
            }
        };
        let handle = physics
            .impulse_joints
            .insert(parent_body, body, generic, true);
        world.entity_mut(e).insert(JointComponent { handle, built });
    }
}
