//! Make rapier match the data components: `sync_components` creates, updates
//! and removes bodies, colliders and joints for `RigidBody`, `Collider` and
//! `Joint` (added, edited in the Inspector, reparented, removed, entity
//! despawned). It compares each component with what it last built, so it
//! costs a pass over the physics entities and touches rapier only on change.

use std::collections::{HashMap, HashSet};

use bevy_ecs::entity::Entity;
use bevy_ecs::query::Without;
use bevy_ecs::world::World;
use glam::{Mat4, Quat, Vec3};
use rapier3d::dynamics::{
    ImpulseJointHandle, JointAxis, PrismaticJointBuilder, RevoluteJointBuilder, RigidBodyBuilder,
    RigidBodyHandle, RigidBodyType, SphericalJointBuilder,
};
use rapier3d::geometry::{
    ColliderBuilder, ColliderHandle, Group, InteractionGroups, InteractionTestMode,
};
use rapier3d::math::Pose;
use rapier3d::pipeline::ActiveEvents;
use rapier3d::prelude::FixedJointBuilder;

use melon_core::hierarchy::world_matrix;
use melon_core::{
    ChildOf, Collider, ColliderShape, Joint, JointKind, PendingDelete, RigidBody, RigidBodyKind,
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
    translation: Vec3,
    rotation: Quat,
}

/// What a joint was last built from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltJoint {
    joint: Joint,
    body: RigidBodyHandle,
    parent: RigidBodyHandle,
}

fn body_type(kind: RigidBodyKind) -> RigidBodyType {
    match kind {
        RigidBodyKind::Dynamic => RigidBodyType::Dynamic,
        RigidBodyKind::Fixed => RigidBodyType::Fixed,
        RigidBodyKind::Kinematic => RigidBodyType::KinematicPositionBased,
    }
}

/// Translation and rotation of an entity in the world (scale dropped).
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

/// The system: `sync_components` on the world's `PhysicsWorld`.
pub fn sync_components_system(world: &mut World) {
    world.try_resource_scope(|world, mut physics: bevy_ecs::world::Mut<PhysicsWorld>| {
        sync_components(world, &mut physics);
    });
}

/// Make `physics` match the physics components of `world`.
pub fn sync_components(world: &mut World, physics: &mut PhysicsWorld) {
    remove_stale(world, physics);
    sync_bodies(world, physics);
    sync_colliders(world, physics);
    sync_joints(world, physics);
}

/// Drop rapier objects whose component is gone or whose entity is gone.
fn remove_stale(world: &mut World, physics: &mut PhysicsWorld) {
    let gone_joints: Vec<(Entity, ImpulseJointHandle)> = world
        .query_filtered::<(Entity, &JointComponent), Without<Joint>>()
        .iter(world)
        .map(|(e, j)| (e, j.handle))
        .collect();
    for (e, h) in gone_joints {
        physics.remove_joint(h);
        world.entity_mut(e).remove::<JointComponent>();
    }
    let gone_bodies: Vec<(Entity, RigidBodyHandle)> = world
        .query_filtered::<(Entity, &RigidBodyComponent), Without<RigidBody>>()
        .iter(world)
        .map(|(e, b)| (e, b.body_handle))
        .collect();
    for (e, h) in gone_bodies {
        physics.remove_body(h);
        world.entity_mut(e).remove::<RigidBodyComponent>();
    }
    let gone_colliders: Vec<(Entity, Option<ColliderHandle>)> = world
        .query_filtered::<(Entity, &ColliderComponent), Without<Collider>>()
        .iter(world)
        .map(|(e, c)| (e, c.collider_handle))
        .collect();
    for (e, h) in gone_colliders {
        if let Some(h) = h.filter(|h| physics.colliders.contains(*h)) {
            physics.remove_collider(h);
        }
        world.entity_mut(e).remove::<ColliderComponent>();
    }

    // Objects of despawned entities: nothing refers to them any more.
    let joints: HashSet<ImpulseJointHandle> = world
        .query::<&JointComponent>()
        .iter(world)
        .map(|j| j.handle)
        .collect();
    let orphans: Vec<_> = physics
        .impulse_joints
        .iter()
        .map(|(h, _)| h)
        .filter(|h| !joints.contains(h))
        .collect();
    for h in orphans {
        physics.remove_joint(h);
    }
    let bodies: HashSet<RigidBodyHandle> = world
        .query::<&RigidBodyComponent>()
        .iter(world)
        .map(|b| b.body_handle)
        .collect();
    let world_body = physics.world_body;
    let orphans: Vec<_> = physics
        .bodies
        .iter()
        .map(|(h, _)| h)
        .filter(|h| !bodies.contains(h) && Some(*h) != world_body)
        .collect();
    for h in orphans {
        physics.remove_body(h);
    }
    let colliders: HashSet<ColliderHandle> = world
        .query::<&ColliderComponent>()
        .iter(world)
        .filter_map(|c| c.collider_handle)
        .collect();
    let orphans: Vec<_> = physics
        .colliders
        .iter()
        .map(|(h, _)| h)
        .filter(|h| !colliders.contains(h))
        .collect();
    for h in orphans {
        physics.remove_collider(h);
    }
}

fn sync_bodies(world: &mut World, physics: &mut PhysicsWorld) {
    let items: Vec<(Entity, RigidBody)> = world
        .query_filtered::<(Entity, &RigidBody), Without<PendingDelete>>()
        .iter(world)
        .map(|(e, rb)| (e, rb.clone()))
        .collect();
    for (e, rb) in items {
        let existing = world
            .get::<RigidBodyComponent>(e)
            .filter(|c| physics.bodies.contains(c.body_handle))
            .map(|c| (c.body_handle, c.built.clone()));
        match existing {
            Some((_, built)) if built == rb => {}
            Some((handle, built)) => {
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
                c.built = rb;
            }
            None => {
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
                world.entity_mut(e).insert(RigidBodyComponent {
                    body_handle,
                    body_type: body_type(rb.kind),
                    built: rb,
                });
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

fn close(a: Vec3, b: Vec3) -> bool {
    (a - b).abs().max_element() < 1e-5
}

fn build_collider(
    world: &World,
    e: Entity,
    c: &Collider,
    scale: Vec3,
    mass: f32,
    pose: Pose,
) -> Option<rapier3d::geometry::Collider> {
    let s = scale.abs().max(Vec3::splat(1e-3));
    let min = |v: f32| v.max(1e-3);
    let builder = match c.shape {
        ColliderShape::Box => ColliderBuilder::cuboid(
            min(c.half_extents.x * s.x),
            min(c.half_extents.y * s.y),
            min(c.half_extents.z * s.z),
        ),
        ColliderShape::Sphere => ColliderBuilder::ball(min(c.radius * s.max_element())),
        ColliderShape::Capsule => {
            ColliderBuilder::capsule_y(min(c.half_height * s.y), min(c.radius * s.x.max(s.z)))
        }
        ColliderShape::Mesh => {
            let (vertices, indices) = mesh::triangles(world, e)?;
            let vertices = vertices.into_iter().map(|v| v * s).collect();
            match ColliderBuilder::trimesh(vertices, indices) {
                Ok(b) => b,
                Err(error) => {
                    tracing::warn!("mesh collider of entity {e}: {error}");
                    return None;
                }
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
    if mass > 0.0 {
        builder = builder.mass(mass);
    }
    Some(builder.build())
}

fn sync_colliders(world: &mut World, physics: &mut PhysicsWorld) {
    let items: Vec<(Entity, Collider)> = world
        .query_filtered::<(Entity, &Collider), Without<PendingDelete>>()
        .iter(world)
        .map(|(e, c)| (e, c.clone()))
        .collect();
    let owners: Vec<Option<Entity>> = items
        .iter()
        .map(|(e, _)| body_owner(world, physics, *e))
        .collect();
    // A body's mass is shared by its solid colliders.
    let mut solid: HashMap<Entity, usize> = HashMap::new();
    for ((_, c), owner) in items.iter().zip(&owners) {
        if let (Some(o), false) = (owner, c.sensor) {
            *solid.entry(*o).or_default() += 1;
        }
    }
    for ((e, c), owner) in items.into_iter().zip(owners) {
        let (translation, rotation, scale) = collider_pose(world, e, owner);
        let mass = owner
            .filter(|_| !c.sensor)
            .and_then(|o| {
                let rb = &world.get::<RigidBodyComponent>(o)?.built;
                (rb.kind == RigidBodyKind::Dynamic && rb.mass > 0.0)
                    .then(|| rb.mass / solid[&o] as f32)
            })
            .unwrap_or(0.0);
        let built = BuiltCollider {
            collider: c.clone(),
            owner,
            scale,
            mass,
            translation,
            rotation,
        };
        let existing = world
            .get::<ColliderComponent>(e)
            .map(|cc| (cc.collider_handle, cc.built.clone()));
        let pose = Pose::from_parts(translation, rotation);
        if let Some((handle, old)) = &existing {
            let alive = handle.is_none_or(|h| physics.colliders.contains(h));
            let same_shape = old.collider == built.collider
                && old.owner == built.owner
                && old.mass == built.mass
                && close(old.scale, built.scale);
            if alive && same_shape {
                if !close(old.translation, translation)
                    || (old.rotation.dot(rotation).abs() - 1.0).abs() > 1e-6
                {
                    if let Some(co) = handle.and_then(|h| physics.colliders.get_mut(h)) {
                        if owner.is_some() {
                            co.set_position_wrt_parent(pose);
                        } else {
                            co.set_position(pose);
                        }
                    }
                    world.get_mut::<ColliderComponent>(e).expect("exists").built = built;
                }
                continue;
            }
            if let Some(h) = handle.filter(|h| physics.colliders.contains(*h)) {
                physics.remove_collider(h);
            }
        }
        let handle = build_collider(world, e, &c, scale, mass, pose).map(|co| match owner {
            Some(o) => {
                let body = world
                    .get::<RigidBodyComponent>(o)
                    .expect("owner")
                    .body_handle;
                physics
                    .colliders
                    .insert_with_parent(co, body, &mut physics.bodies)
            }
            None => physics.colliders.insert(co),
        });
        world.entity_mut(e).insert(ColliderComponent {
            collider_handle: handle,
            built,
        });
    }
}

fn sync_joints(world: &mut World, physics: &mut PhysicsWorld) {
    let items: Vec<(Entity, Joint)> = world
        .query_filtered::<(Entity, &Joint), Without<PendingDelete>>()
        .iter(world)
        .map(|(e, j)| (e, j.clone()))
        .collect();
    for (e, joint) in items {
        let Some(body) = world
            .get::<RigidBodyComponent>(e)
            .map(|b| b.body_handle)
            .filter(|h| physics.bodies.contains(*h))
        else {
            continue;
        };
        let parent_body = parent_of(world, e)
            .and_then(|p| body_owner(world, physics, p))
            .and_then(|o| world.get::<RigidBodyComponent>(o).map(|b| b.body_handle))
            .unwrap_or_else(|| physics.world_body());
        let built = BuiltJoint {
            joint: joint.clone(),
            body,
            parent: parent_body,
        };
        if let Some(existing) = world.get::<JointComponent>(e) {
            if existing.built == built && physics.impulse_joints.contains(existing.handle) {
                continue;
            }
            physics.remove_joint(existing.handle);
        }
        let axis = joint.axis.try_normalize().unwrap_or(Vec3::Y);
        let limits = [joint.limit_min, joint.limit_max];
        let generic: rapier3d::dynamics::GenericJoint = match joint.kind {
            JointKind::Fixed => {
                let rot = |h: RigidBodyHandle| {
                    physics
                        .bodies
                        .get(h)
                        .map_or(Quat::IDENTITY, |b| *b.rotation())
                };
                let relative = rot(parent_body).inverse() * rot(body);
                FixedJointBuilder::new()
                    .local_frame1(Pose::from_parts(joint.parent_anchor, relative))
                    .local_frame2(Pose::from_parts(joint.anchor, Quat::IDENTITY))
                    .build()
                    .into()
            }
            JointKind::Hinge => {
                let mut b = RevoluteJointBuilder::new(axis)
                    .local_anchor1(joint.parent_anchor)
                    .local_anchor2(joint.anchor);
                if joint.use_limits {
                    b = b.limits(limits);
                }
                b.build().into()
            }
            JointKind::Slider => {
                let mut b = PrismaticJointBuilder::new(axis)
                    .local_anchor1(joint.parent_anchor)
                    .local_anchor2(joint.anchor);
                if joint.use_limits {
                    b = b.limits(limits);
                }
                b.build().into()
            }
            JointKind::Ball => {
                let mut b = SphericalJointBuilder::new()
                    .local_anchor1(joint.parent_anchor)
                    .local_anchor2(joint.anchor);
                if joint.cone_limit < std::f32::consts::PI {
                    // Swing about the Y and Z axes bounded; twist about X free.
                    let cone = [-joint.cone_limit, joint.cone_limit];
                    b = b
                        .limits(JointAxis::AngY, cone)
                        .limits(JointAxis::AngZ, cone);
                }
                b.build().into()
            }
        };
        let handle = physics
            .impulse_joints
            .insert(parent_body, body, generic, true);
        world.entity_mut(e).insert(JointComponent { handle, built });
    }
}
