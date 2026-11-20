//! rapier3d under the ECS. Game data lives in `melon_core::physics`
//! (`RigidBody`, `Collider`, `CharacterController`, `Joint`); `sync` turns it
//! into rapier bodies, colliders and joints, `queries` answers ray / shape
//! queries, and the rapier handles stay internal bookkeeping on the entities.

use std::sync::{Arc, Mutex};

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::message::Messages;
use bevy_ecs::query::{With, Without};
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::{Commands, Query, Res, ResMut};
use bevy_ecs::world::World;
use glam::{Quat, Vec3};

pub use rapier3d;
pub use rapier3d::dynamics::RigidBodyType;
use rapier3d::dynamics::{
    CCDSolver, ImpulseJointHandle, ImpulseJointSet, IslandManager, MultibodyJointSet,
    RigidBodyHandle, RigidBodySet,
};
use rapier3d::geometry::{
    BroadPhaseBvh, ColliderHandle, ColliderSet, CollisionEvent as RapierEvent, CollisionEventFlags,
    ContactPair, NarrowPhase,
};
use rapier3d::math::Vector;
use rapier3d::pipeline::{EventHandler, PhysicsPipeline};
use rapier3d::prelude::IntegrationParameters;
use tracing::instrument;

use melon_core::hierarchy::{propagate_transforms_system, safe_inverse, world_matrix};
use melon_core::{
    ChildOf, CollisionEvent, CollisionEventKind, Core, FixedUpdate, GlobalTransform, PendingDelete,
    PhysicsQueries, PostUpdate, PreviousTransform, RigidBody, SimulationState, Time, Transform,
    store_previous_transforms_system,
};

mod character;
mod mesh;
mod queries;
mod sync;

pub use character::character_controller_system;
pub use sync::{sync_components, sync_components_system};

/// Gravity vector in m/s^2. Default is Earth gravity pointing down (-Y).
const GRAVITY: Vector = Vector::new(0.0, -9.81, 0.0);

/// Bookkeeping: the rapier body of an entity with a `RigidBody`, made and
/// kept in line with it by `sync_components`. `built` is the `RigidBody` it
/// was last made from.
#[derive(Component, Debug, Clone)]
pub struct RigidBodyComponent {
    pub body_handle: RigidBodyHandle,
    pub body_type: RigidBodyType,
    pub(crate) built: RigidBody,
}

/// Bookkeeping: the rapier collider of an entity with a `Collider`.
/// `collider_handle` is `None` when none could be built (a mesh collider
/// whose model is missing).
#[derive(Component, Debug, Clone)]
pub struct ColliderComponent {
    pub collider_handle: Option<ColliderHandle>,
    pub(crate) built: sync::BuiltCollider,
}

/// Bookkeeping: the rapier joint of an entity with a `Joint`.
#[derive(Component, Debug, Clone)]
pub struct JointComponent {
    pub(crate) handle: ImpulseJointHandle,
    pub(crate) built: sync::BuiltJoint,
}

/// A collision event as rapier reports it, before it is mapped to entities.
struct RawEvent {
    a: ColliderHandle,
    b: ColliderHandle,
    started: bool,
    sensor: bool,
    removed: bool,
}

#[derive(Default)]
struct EventCollector(Mutex<Vec<RawEvent>>);

impl EventHandler for EventCollector {
    fn handle_collision_event(
        &self,
        _bodies: &RigidBodySet,
        _colliders: &ColliderSet,
        event: RapierEvent,
        _contact_pair: Option<&ContactPair>,
    ) {
        let (a, b, flags, started) = match event {
            RapierEvent::Started(a, b, f) => (a, b, f, true),
            RapierEvent::Stopped(a, b, f) => (a, b, f, false),
        };
        self.0.lock().unwrap().push(RawEvent {
            a,
            b,
            started,
            sensor: flags.contains(CollisionEventFlags::SENSOR),
            removed: flags.contains(CollisionEventFlags::REMOVED),
        });
    }

    fn handle_contact_force_event(
        &self,
        _dt: rapier3d::math::Real,
        _bodies: &RigidBodySet,
        _colliders: &ColliderSet,
        _contact_pair: &ContactPair,
        _total_force_magnitude: rapier3d::math::Real,
    ) {
    }
}

/// Resource holding the rapier physics world state.
/// One instance per `Core` world, stepped once per fixed step.
#[derive(Resource)]
pub struct PhysicsWorld {
    pub pipeline: PhysicsPipeline,
    pub islands: IslandManager,
    pub broad_phase: BroadPhaseBvh,
    pub narrow_phase: NarrowPhase,
    pub bodies: RigidBodySet,
    pub colliders: ColliderSet,
    pub impulse_joints: ImpulseJointSet,
    pub multibody_joints: MultibodyJointSet,
    pub ccd_solver: CCDSolver,
    pub integration_parameters: IntegrationParameters,
    pub gravity: Vector,
    events: EventCollector,
    /// Fixed body at the origin that joints without an ancestor body attach to.
    world_body: Option<RigidBodyHandle>,
}

impl Default for PhysicsWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl PhysicsWorld {
    #[instrument(skip_all, level = "info")]
    pub fn new() -> Self {
        Self {
            pipeline: PhysicsPipeline::new(),
            islands: IslandManager::new(),
            broad_phase: BroadPhaseBvh::new(),
            narrow_phase: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd_solver: CCDSolver::new(),
            integration_parameters: IntegrationParameters::default(),
            gravity: GRAVITY,
            events: EventCollector::default(),
            world_body: None,
        }
    }

    /// Step the physics simulation by one fixed timestep (1/60s by default).
    pub fn step(&mut self) {
        self.pipeline.step(
            self.gravity,
            &self.integration_parameters,
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.ccd_solver,
            &(),
            &self.events,
        );
    }

    /// Get the world-space translation of a rigid body.
    pub fn body_translation(&self, handle: RigidBodyHandle) -> Option<Vec3> {
        self.bodies.get(handle).map(|b| b.translation())
    }

    /// Get the world-space rotation of a rigid body.
    pub fn body_rotation(&self, handle: RigidBodyHandle) -> Option<Quat> {
        self.bodies.get(handle).map(|b| *b.rotation())
    }

    /// Clear all bodies, colliders, joints, and pipeline state.
    /// Preserves gravity and integration parameters (engine configuration,
    /// not scene data). Used during scene reload.
    #[instrument(skip(self), level = "debug")]
    pub fn clear(&mut self) {
        self.pipeline = PhysicsPipeline::new();
        self.islands = IslandManager::new();
        self.broad_phase = BroadPhaseBvh::new();
        self.narrow_phase = NarrowPhase::new();
        self.bodies = RigidBodySet::new();
        self.colliders = ColliderSet::new();
        self.impulse_joints = ImpulseJointSet::new();
        self.multibody_joints = MultibodyJointSet::new();
        self.ccd_solver = CCDSolver::new();
        self.events.0.lock().unwrap().clear();
        self.world_body = None;
    }

    /// Remove a rigid body and all its attached colliders from the physics world.
    /// Uses remove_attached_colliders=true to clean up colliders automatically.
    #[instrument(skip(self), level = "debug")]
    pub fn remove_body(&mut self, handle: RigidBodyHandle) {
        self.bodies.remove(
            handle,
            &mut self.islands,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            true,
        );
    }

    /// Remove a collider. Wakes up the parent body if one exists.
    #[instrument(skip(self), level = "debug")]
    pub fn remove_collider(&mut self, handle: ColliderHandle) {
        self.colliders
            .remove(handle, &mut self.islands, &mut self.bodies, true);
    }

    /// Remove a joint.
    pub fn remove_joint(&mut self, handle: ImpulseJointHandle) {
        self.impulse_joints.remove(handle, true);
    }

    /// The body joints without an ancestor body attach to.
    fn world_body(&mut self) -> RigidBodyHandle {
        if let Some(h) = self.world_body.filter(|h| self.bodies.contains(*h)) {
            return h;
        }
        let h = self
            .bodies
            .insert(rapier3d::dynamics::RigidBodyBuilder::fixed().build());
        self.world_body = Some(h);
        h
    }

    /// Entity stored in a collider's `user_data`.
    fn collider_entity(&self, handle: ColliderHandle) -> Option<Entity> {
        let bits = self.colliders.get(handle)?.user_data as u64;
        Entity::try_from_bits(bits)
    }
}

/// System: step the physics world by one `Time::fixed_delta`, write the
/// collision events and publish the query snapshot.
pub fn step_system(
    mut world: ResMut<PhysicsWorld>,
    time: Res<Time>,
    mut events: ResMut<Messages<CollisionEvent>>,
    mut queries: ResMut<PhysicsQueries>,
) {
    world.integration_parameters.dt = time.fixed_delta;
    world.step();
    // Two buffers: events live for this step and the next one.
    events.update();
    let raw = std::mem::take(&mut *world.events.0.lock().unwrap());
    events.write_batch(raw.into_iter().filter_map(|e| {
        // A removed collider is gone, and its entity with it.
        if e.removed {
            return None;
        }
        let kind = match (e.sensor, e.started) {
            (false, true) => CollisionEventKind::Started,
            (false, false) => CollisionEventKind::Ended,
            (true, true) => CollisionEventKind::TriggerEntered,
            (true, false) => CollisionEventKind::TriggerExited,
        };
        Some(CollisionEvent {
            a: world.collider_entity(e.a)?,
            b: world.collider_entity(e.b)?,
            kind,
        })
    }));
    queries.set_backend(Arc::new(queries::Snapshot::of(&world)));
}

/// System: give every dynamic body a `PreviousTransform` (equal to its
/// current `Transform`, so it never blends from the origin). One system
/// covers all spawn paths: default scene, scene load, editor.
#[allow(clippy::type_complexity)]
pub fn add_previous_transforms_system(
    mut commands: Commands,
    // Skip entities being deleted: their despawn may apply first in the same
    // PostUpdate, and an insert on a despawned entity is an error.
    query: Query<
        (Entity, &RigidBodyComponent, &Transform),
        (Without<PreviousTransform>, Without<PendingDelete>),
    >,
) {
    for (entity, rb, transform) in &query {
        if rb.body_type == RigidBodyType::Dynamic {
            commands
                .entity(entity)
                .insert(PreviousTransform(*transform));
        }
    }
}

/// Register the physics systems into their frame stages. Fixed-step order:
/// remember previous transforms, update world poses, make rapier match the
/// components, move characters, push ECS edits into rapier, step, pull
/// results back.
pub fn add_physics_systems(core: &mut Core) {
    core.world.init_resource::<PhysicsWorld>();
    core.add_systems(
        FixedUpdate,
        (
            store_previous_transforms_system,
            propagate_transforms_system,
            sync_components_system,
            character_controller_system,
            sync_to_physics_system,
            step_system,
            sync_from_physics_system,
        )
            .chain(),
    );
    core.add_systems(
        PostUpdate,
        (
            cleanup_deleted_system,
            // Edits made in the Inspector or by scripts reach rapier now,
            // also while editing.
            sync_components_system,
            add_previous_transforms_system,
            // Keep dynamic children on their bodies every frame, even when a
            // parent moved outside the fixed step (gizmo, Update, paused).
            sync_from_physics_system
                .run_if(simulating)
                .before(propagate_transforms_system),
        )
            .chain(),
    );
}

/// System: sync rapier body poses back into ECS Transform components.
/// Only updates dynamic bodies, which physics owns in world space: a child's
/// local transform is rewritten so its world pose matches the body (it does
/// not follow its parent). Parents are written before their children, so a
/// dynamic child of a dynamic parent uses the parent's new pose. Fixed and
/// kinematic bodies keep their ECS transform as the source of truth.
pub fn sync_from_physics_system(world: &mut World) {
    let dynamic: Vec<(Entity, RigidBodyHandle)> = world
        .query::<(Entity, &RigidBodyComponent)>()
        .iter(world)
        .filter(|(_, rb)| rb.body_type == RigidBodyType::Dynamic)
        .map(|(e, rb)| (e, rb.body_handle))
        .collect();
    let Some(physics) = world.get_resource::<PhysicsWorld>() else {
        return;
    };
    let depth = |e: Entity| {
        let mut depth = 0;
        let mut current = e;
        while let Some(c) = world.get::<ChildOf>(current) {
            depth += 1;
            current = c.parent();
            if depth > 1024 {
                break;
            }
        }
        depth
    };
    let mut poses: Vec<(usize, Entity, Vec3, Quat)> = dynamic
        .into_iter()
        .filter_map(|(e, h)| {
            let body = physics.bodies.get(h)?;
            Some((depth(e), e, body.translation(), *body.rotation()))
        })
        .collect();
    poses.sort_by_key(|(depth, ..)| *depth);

    for (_, e, translation, rotation) in poses {
        let (translation, rotation) = match world.get::<ChildOf>(e).map(ChildOf::parent) {
            Some(parent) => {
                let local = Transform::from_mat4(
                    safe_inverse(world_matrix(world, parent))
                        * glam::Mat4::from_rotation_translation(rotation, translation),
                );
                if !(local.translation.is_finite() && local.rotation.is_finite()) {
                    continue;
                }
                (local.translation, local.rotation)
            }
            None => (translation, rotation),
        };
        if let Some(mut t) = world.get_mut::<Transform>(e) {
            t.translation = translation;
            t.rotation = rotation;
        }
    }
}

/// Run condition: playing or paused (not editing).
fn simulating(state: Option<Res<SimulationState>>) -> bool {
    state.is_some_and(|s| *s != SimulationState::Editing)
}

/// System: sync ECS world poses into rapier for fixed and kinematic bodies
/// (they follow their entity and its parent). Dynamic bodies are controlled
/// by physics, so their ECS transform is NOT written back to rapier.
pub fn sync_to_physics_system(
    mut physics: ResMut<PhysicsWorld>,
    query: Query<(&RigidBodyComponent, &Transform, Option<&GlobalTransform>)>,
) {
    for (rb, transform, global) in &query {
        if rb.body_type == RigidBodyType::Dynamic {
            continue;
        }
        let world = global.map_or(*transform, |g| Transform::from_mat4(g.0));
        if let Some(body) = physics.bodies.get_mut(rb.body_handle) {
            if rb.body_type == RigidBodyType::KinematicPositionBased {
                // Moves the body over the step, so it pushes what it touches.
                body.set_next_kinematic_translation(world.translation);
                body.set_next_kinematic_rotation(world.rotation);
            } else {
                body.set_translation(world.translation, true);
                body.set_rotation(world.rotation, true);
            }
        }
    }
}

/// System: cleanup entities marked with PendingDelete.
/// Removes rapier joints, bodies and colliders before despawning the ECS
/// entity. Run this after editor operations, before render.
#[allow(clippy::type_complexity)]
pub fn cleanup_deleted_system(
    mut commands: Commands,
    mut physics: ResMut<PhysicsWorld>,
    query: Query<
        (
            Entity,
            Option<&RigidBodyComponent>,
            Option<&ColliderComponent>,
            Option<&JointComponent>,
            Option<&ChildOf>,
        ),
        With<PendingDelete>,
    >,
    marked: Query<(), With<PendingDelete>>,
) {
    for (entity, rb_opt, collider_opt, joint_opt, child_of) in &query {
        if let Some(j) = joint_opt {
            physics.remove_joint(j.handle);
        }
        if let Some(rb) = rb_opt {
            physics.remove_body(rb.body_handle);
        } else if let Some(h) = collider_opt.and_then(|c| c.collider_handle) {
            physics.remove_collider(h);
        }
        // A marked parent's despawn takes its children with it (`linked_spawn`).
        if !child_of.is_some_and(|c| marked.contains(c.parent())) {
            commands.entity(entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::message::MessageReader;
    use bevy_ecs::system::ResMut as BevyResMut;
    use melon_core::{
        CharacterController, Collider, ColliderShape, Joint, JointKind, Name, PhysicsQueries,
        QueryFilter, RigidBodyKind,
    };

    fn core() -> Core {
        let mut core = Core::new();
        core.world.insert_resource(SimulationState::Playing);
        add_physics_systems(&mut core);
        core
    }

    fn frames(core: &mut Core, n: u32) {
        run_frames(core, 1.0 / 60.0, n);
    }

    fn run_frames(core: &mut Core, dt: f32, n: u32) {
        for _ in 0..n {
            core.world.resource_mut::<Time>().update(dt);
            core.update();
        }
    }

    fn cube() -> Collider {
        Collider {
            shape: ColliderShape::Box {
                half_extents: Vec3::splat(0.5),
            },
            ..Default::default()
        }
    }

    fn at(x: f32, y: f32, z: f32) -> Transform {
        Transform::from_translation(Vec3::new(x, y, z))
    }

    /// A static floor whose top face is at y = 0.5.
    fn floor(core: &mut Core) -> Entity {
        core.world
            .spawn((
                at(0.0, 0.0, 0.0),
                Collider {
                    shape: ColliderShape::Box {
                        half_extents: Vec3::new(20.0, 0.5, 20.0),
                    },
                    ..Default::default()
                },
            ))
            .id()
    }

    fn dynamic(core: &mut Core, t: Transform, collider: Collider) -> Entity {
        core.world.spawn((t, RigidBody::default(), collider)).id()
    }

    fn height(core: &Core, e: Entity) -> f32 {
        core.world.get::<Transform>(e).unwrap().translation.y
    }

    fn body_pos(core: &Core, e: Entity) -> Vec3 {
        let h = core.world.get::<RigidBodyComponent>(e).unwrap().body_handle;
        core.world
            .resource::<PhysicsWorld>()
            .body_translation(h)
            .unwrap()
    }

    fn world_pos(core: &Core, e: Entity) -> Vec3 {
        world_matrix(&core.world, e).transform_point3(Vec3::ZERO)
    }

    fn gravity_off(core: &mut Core) {
        core.world.resource_mut::<PhysicsWorld>().gravity = Vector::ZERO;
    }

    #[test]
    fn physics_world_default_has_no_bodies() {
        let world = PhysicsWorld::new();
        assert!(world.bodies.is_empty());
        assert!(world.colliders.is_empty());
        assert!((world.gravity.y + 9.81).abs() < 1e-6);
    }

    #[test]
    fn clear_empties_all_sets_and_keeps_gravity() {
        let mut c = core();
        dynamic(&mut c, at(0.0, 5.0, 0.0), cube());
        c.world.resource_mut::<PhysicsWorld>().gravity = Vector::new(1.0, -20.0, 0.0);
        frames(&mut c, 1);
        let mut pw = c.world.resource_mut::<PhysicsWorld>();
        assert!(!pw.bodies.is_empty() && !pw.colliders.is_empty());
        pw.clear();
        assert!(pw.bodies.is_empty() && pw.colliders.is_empty() && pw.impulse_joints.is_empty());
        assert!((pw.gravity.y + 20.0).abs() < 1e-6 && (pw.gravity.x - 1.0).abs() < 1e-6);
    }

    #[test]
    fn components_make_rapier_objects() {
        let mut c = core();
        let e = dynamic(&mut c, at(0.0, 5.0, 0.0), cube());
        frames(&mut c, 1);
        let pw = c.world.resource::<PhysicsWorld>();
        assert_eq!(pw.bodies.len(), 1);
        assert_eq!(pw.colliders.len(), 1);
        assert!(c.world.get::<RigidBodyComponent>(e).is_some());
        assert!(height(&c, e) < 5.0, "dynamic body should fall");
    }

    #[test]
    fn box_sphere_and_capsule_rest_on_a_box() {
        let mut c = core();
        floor(&mut c);
        let boxy = dynamic(&mut c, at(-4.0, 3.0, 0.0), cube());
        let ball = dynamic(
            &mut c,
            at(0.0, 3.0, 0.0),
            Collider {
                shape: ColliderShape::Sphere { radius: 0.5 },
                ..Default::default()
            },
        );
        let capsule = dynamic(
            &mut c,
            at(4.0, 3.0, 0.0),
            Collider {
                shape: ColliderShape::Capsule {
                    half_height: 0.5,
                    radius: 0.25,
                },
                ..Default::default()
            },
        );
        frames(&mut c, 300);
        // The floor top is at 0.5.
        assert!(
            (height(&c, boxy) - 1.0).abs() < 0.05,
            "{}",
            height(&c, boxy)
        );
        assert!(
            (height(&c, ball) - 1.0).abs() < 0.05,
            "{}",
            height(&c, ball)
        );
        assert!(
            (height(&c, capsule) - 1.25).abs() < 0.05,
            "{}",
            height(&c, capsule)
        );
    }

    #[test]
    fn collider_follows_scale() {
        let mut c = core();
        floor(&mut c);
        let mut t = at(0.0, 3.0, 0.0);
        t.scale = Vec3::splat(2.0);
        let e = dynamic(&mut c, t, cube());
        frames(&mut c, 300);
        // Half extent 0.5 * scale 2 = 1 on a floor top at 0.5.
        assert!((height(&c, e) - 1.5).abs() < 0.05, "{}", height(&c, e));
    }

    #[test]
    fn kinematic_body_follows_transform() {
        let mut c = core();
        let e = c
            .world
            .spawn((
                at(0.0, 0.0, 0.0),
                RigidBody {
                    kind: RigidBodyKind::Kinematic,
                    ..Default::default()
                },
                cube(),
            ))
            .id();
        frames(&mut c, 2);
        c.world.get_mut::<Transform>(e).unwrap().translation = Vec3::new(5.0, 2.0, 0.0);
        frames(&mut c, 3);
        assert!((body_pos(&c, e) - Vec3::new(5.0, 2.0, 0.0)).length() < 1e-3);
        // Gravity never moved it.
        assert!((height(&c, e) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn editing_the_collider_rebuilds_it() {
        let mut c = core();
        let e = c.world.spawn((at(0.0, 0.0, 0.0), cube())).id();
        frames(&mut c, 1);
        let shape_of = |c: &Core| {
            let h = c.world.get::<ColliderComponent>(e).unwrap().collider_handle;
            c.world
                .resource::<PhysicsWorld>()
                .colliders
                .get(h.unwrap())
                .unwrap()
                .shared_shape()
                .clone()
        };
        assert_eq!(shape_of(&c).as_cuboid().unwrap().half_extents.x, 0.5);
        c.world.get_mut::<Collider>(e).unwrap().shape = ColliderShape::Box {
            half_extents: Vec3::new(2.0, 0.5, 0.5),
        };
        frames(&mut c, 1);
        assert_eq!(shape_of(&c).as_cuboid().unwrap().half_extents.x, 2.0);
        {
            let mut col = c.world.get_mut::<Collider>(e).unwrap();
            col.shape = ColliderShape::Sphere { radius: 0.75 };
        }
        frames(&mut c, 1);
        assert_eq!(shape_of(&c).as_ball().unwrap().radius, 0.75);
        assert_eq!(c.world.resource::<PhysicsWorld>().colliders.len(), 1);
    }

    #[test]
    fn editing_the_body_updates_it_without_losing_velocity() {
        let mut c = core();
        let e = dynamic(&mut c, at(0.0, 50.0, 0.0), cube());
        frames(&mut c, 30);
        let h = c.world.get::<RigidBodyComponent>(e).unwrap().body_handle;
        let v = c
            .world
            .resource::<PhysicsWorld>()
            .bodies
            .get(h)
            .unwrap()
            .linvel()
            .y;
        c.world.get_mut::<RigidBody>(e).unwrap().gravity_scale = 0.0;
        frames(&mut c, 1);
        let pw = c.world.resource::<PhysicsWorld>();
        let body = pw.bodies.get(h).unwrap();
        assert_eq!(body.gravity_scale(), 0.0);
        assert!((body.linvel().y - v).abs() < 1e-3, "kept falling speed");
        c.world.get_mut::<RigidBody>(e).unwrap().kind = RigidBodyKind::Fixed;
        frames(&mut c, 1);
        let c_body = c.world.get::<RigidBodyComponent>(e).unwrap();
        assert_eq!(c_body.body_type, RigidBodyType::Fixed);
    }

    #[test]
    fn collider_on_child_attaches_to_the_ancestor_body() {
        let mut c = core();
        floor(&mut c);
        let parent = c
            .world
            .spawn((at(0.0, 3.0, 0.0), RigidBody::default()))
            .id();
        // A child collider offset 1 m to the side: the body rests on it.
        c.world.spawn((at(1.0, 0.0, 0.0), cube(), ChildOf(parent)));
        frames(&mut c, 300);
        assert_eq!(c.world.resource::<PhysicsWorld>().bodies.len(), 1);
        assert!(
            (height(&c, parent) - 1.0).abs() < 0.05,
            "{}",
            height(&c, parent)
        );
    }

    #[test]
    fn removing_the_components_removes_the_rapier_objects() {
        let mut c = core();
        let e = dynamic(&mut c, at(0.0, 5.0, 0.0), cube());
        frames(&mut c, 1);
        c.world.entity_mut(e).remove::<Collider>();
        frames(&mut c, 1);
        assert_eq!(c.world.resource::<PhysicsWorld>().colliders.len(), 0);
        assert!(c.world.get::<ColliderComponent>(e).is_none());
        c.world.entity_mut(e).remove::<RigidBody>();
        frames(&mut c, 1);
        assert_eq!(c.world.resource::<PhysicsWorld>().bodies.len(), 0);
    }

    #[test]
    fn despawned_entity_leaves_no_rapier_objects() {
        let mut c = core();
        let e = dynamic(&mut c, at(0.0, 5.0, 0.0), cube());
        frames(&mut c, 1);
        c.world.despawn(e);
        frames(&mut c, 1);
        let pw = c.world.resource::<PhysicsWorld>();
        assert_eq!((pw.bodies.len(), pw.colliders.len()), (0, 0));
    }

    #[test]
    fn deleting_a_body_entity_removes_its_rapier_objects() {
        let mut c = core();
        let e = dynamic(&mut c, at(0.0, 5.0, 0.0), cube());
        frames(&mut c, 1);
        melon_core::hierarchy::mark_for_delete(&mut c.world, e);
        frames(&mut c, 1);
        let pw = c.world.resource::<PhysicsWorld>();
        assert_eq!((pw.bodies.len(), pw.colliders.len()), (0, 0));
        assert!(c.world.get_entity(e).is_err());
    }

    #[test]
    fn cleanup_handles_entity_without_physics() {
        let mut c = core();
        let e = c
            .world
            .spawn((Name("Test".to_string()), PendingDelete))
            .id();
        frames(&mut c, 1);
        assert!(c.world.get_entity(e).is_err());
    }

    #[test]
    fn deleting_new_dynamic_body_does_not_panic() {
        let mut c = core();
        let e = dynamic(&mut c, at(0.0, 5.0, 0.0), cube());
        c.world.entity_mut(e).insert(PendingDelete);
        frames(&mut c, 2);
        assert!(c.world.get_entity(e).is_err());
    }

    #[test]
    fn fall_height_independent_of_frame_rate() {
        let mut c30 = core();
        let e30 = dynamic(&mut c30, at(0.0, 10.0, 0.0), cube());
        run_frames(&mut c30, 1.0 / 30.0, 30);
        let mut c144 = core();
        let e144 = dynamic(&mut c144, at(0.0, 10.0, 0.0), cube());
        run_frames(&mut c144, 1.0 / 144.0, 144);
        let (y30, y144) = (height(&c30, e30), height(&c144, e144));
        // 0.2 m is about one fixed step of fall after 1 s.
        assert!((y30 - y144).abs() < 0.2, "{y30} vs {y144}");
        // Free fall for 1 s is ~4.9 m, not 2.4x that.
        assert!(y30 < 6.0 && y30 > 4.0, "y30 = {y30}");
    }

    #[test]
    fn paused_body_does_not_fall() {
        let mut c = core();
        let e = dynamic(&mut c, at(0.0, 10.0, 0.0), cube());
        c.world.insert_resource(SimulationState::Paused);
        frames(&mut c, 60);
        assert_eq!(height(&c, e), 10.0);
    }

    #[test]
    fn editing_simulates_nothing_but_builds_the_bodies() {
        let mut c = core();
        let e = dynamic(&mut c, at(0.0, 10.0, 0.0), cube());
        c.world.insert_resource(SimulationState::Editing);
        frames(&mut c, 60);
        assert_eq!(height(&c, e), 10.0);
        assert!(c.world.get::<RigidBodyComponent>(e).is_some());
        assert_eq!(c.world.resource::<PhysicsWorld>().colliders.len(), 1);
    }

    #[test]
    fn dynamic_body_gets_previous_transform_equal_to_current() {
        let mut c = core();
        let e = dynamic(&mut c, at(0.0, 10.0, 0.0), cube());
        run_frames(&mut c, 0.001, 1);
        let prev = c.world.get::<PreviousTransform>(e).expect("none");
        assert_eq!(
            prev.0.translation,
            c.world.get::<Transform>(e).unwrap().translation
        );
    }

    #[test]
    fn fixed_body_gets_no_previous_transform() {
        let mut c = core();
        let e = c
            .world
            .spawn((
                at(0.0, 0.0, 0.0),
                RigidBody {
                    kind: RigidBodyKind::Fixed,
                    ..Default::default()
                },
                cube(),
            ))
            .id();
        frames(&mut c, 2);
        assert!(c.world.get::<PreviousTransform>(e).is_none());
    }

    /// Parent (no body) at the origin with a child body at local (1,0,0).
    /// Gravity off, so only parent motion could move the child.
    fn parented(kind: RigidBodyKind) -> (Core, Entity, Entity) {
        let mut c = core();
        gravity_off(&mut c);
        let parent = c.world.spawn(Transform::default()).id();
        let child = c
            .world
            .spawn((
                at(1.0, 0.0, 0.0),
                RigidBody {
                    kind,
                    ..Default::default()
                },
                cube(),
                ChildOf(parent),
            ))
            .id();
        (c, parent, child)
    }

    #[test]
    fn fixed_child_body_follows_moved_parent() {
        let (mut c, parent, child) = parented(RigidBodyKind::Fixed);
        frames(&mut c, 1);
        c.world.get_mut::<Transform>(parent).unwrap().translation = Vec3::new(5.0, 0.0, 0.0);
        frames(&mut c, 1);
        assert!((body_pos(&c, child) - Vec3::new(6.0, 0.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn dynamic_child_ignores_parent_motion() {
        let (mut c, parent, child) = parented(RigidBodyKind::Dynamic);
        frames(&mut c, 1);
        c.world.get_mut::<Transform>(parent).unwrap().translation = Vec3::new(5.0, 0.0, 0.0);
        frames(&mut c, 1);
        let body = body_pos(&c, child);
        assert!((body - Vec3::X).length() < 1e-4, "body moved to {body}");
        assert!((world_pos(&c, child) - body).length() < 1e-4);
    }

    #[test]
    fn dynamic_sync_under_zero_scale_parent_stays_finite() {
        let (mut c, parent, child) = parented(RigidBodyKind::Dynamic);
        frames(&mut c, 1);
        c.world.get_mut::<Transform>(parent).unwrap().scale = Vec3::new(0.0, 1.0, 1.0);
        frames(&mut c, 1);
        let t = c.world.get::<Transform>(child).unwrap();
        assert!(t.translation.is_finite() && t.rotation.is_finite());
    }

    #[test]
    fn deleting_parent_and_child_same_frame_removes_both_bodies() {
        let (mut c, parent, child) = parented(RigidBodyKind::Dynamic);
        frames(&mut c, 1);
        c.world.entity_mut(parent).insert(PendingDelete);
        c.world.entity_mut(child).insert(PendingDelete);
        run_frames(&mut c, 0.001, 1);
        assert_eq!(c.world.resource::<PhysicsWorld>().bodies.len(), 0);
        assert!(c.world.get_entity(parent).is_err() && c.world.get_entity(child).is_err());
    }

    #[test]
    fn deleting_only_parent_with_mark_for_delete_removes_child_body() {
        let (mut c, parent, child) = parented(RigidBodyKind::Dynamic);
        frames(&mut c, 1);
        melon_core::hierarchy::mark_for_delete(&mut c.world, parent);
        run_frames(&mut c, 0.001, 1);
        assert_eq!(c.world.resource::<PhysicsWorld>().bodies.len(), 0);
        assert!(c.world.get_entity(child).is_err());
    }

    #[test]
    fn dynamic_child_of_dynamic_parent_draws_at_its_body() {
        let mut c = core();
        let parent = dynamic(&mut c, at(0.0, 0.0, 0.0), cube());
        let child = c
            .world
            .spawn((
                at(3.0, 0.0, 0.0),
                RigidBody::default(),
                cube(),
                ChildOf(parent),
            ))
            .id();
        frames(&mut c, 30);
        let (drawn, body) = (world_pos(&c, child), body_pos(&c, child));
        assert!((drawn - body).length() < 1e-4, "drawn {drawn}, body {body}");
    }

    #[test]
    fn paused_dynamic_child_stays_put_when_parent_moves() {
        let (mut c, parent, child) = parented(RigidBodyKind::Dynamic);
        frames(&mut c, 1);
        c.world.insert_resource(SimulationState::Paused);
        c.world.get_mut::<Transform>(parent).unwrap().translation = Vec3::new(5.0, 0.0, 0.0);
        frames(&mut c, 1);
        assert!((world_pos(&c, child) - Vec3::X).length() < 1e-4);
    }

    // ---- queries ----

    /// A floor plus one frame, so the query snapshot exists.
    fn queries_core() -> (Core, Entity) {
        let mut c = core();
        let f = floor(&mut c);
        frames(&mut c, 1);
        (c, f)
    }

    fn queries(c: &Core) -> PhysicsQueries {
        c.world.resource::<PhysicsQueries>().clone()
    }

    #[test]
    fn raycast_hits_and_misses() {
        let (c, f) = queries_core();
        let q = queries(&c);
        let any = QueryFilter::default();
        let o = Vec3::new(0.0, 5.0, 0.0);
        let hit = q.raycast(o, -Vec3::Y, 100.0, &any).unwrap();
        assert_eq!(hit.entity, f);
        assert!((hit.distance - 4.5).abs() < 1e-3, "{}", hit.distance);
        assert!((hit.point - Vec3::new(0.0, 0.5, 0.0)).length() < 1e-3);
        assert!((hit.normal - Vec3::Y).length() < 1e-3);
        assert!(q.raycast(o, Vec3::Y, 100.0, &any).is_none());
        assert!(q.raycast(o, -Vec3::Y, 2.0, &any).is_none());
    }

    #[test]
    fn raycast_all_returns_every_hit_nearest_first() {
        let (mut c, f) = queries_core();
        let upper = c.world.spawn((at(0.0, 3.0, 0.0), cube())).id();
        frames(&mut c, 1);
        let hits = queries(&c).raycast_all(
            Vec3::new(0.0, 10.0, 0.0),
            -Vec3::Y,
            100.0,
            &QueryFilter::default(),
        );
        let entities: Vec<Entity> = hits.iter().map(|h| h.entity).collect();
        assert_eq!(entities, [upper, f]);
    }

    #[test]
    fn raycast_respects_layers_and_excluded_entity() {
        let (mut c, f) = queries_core();
        c.world.get_mut::<Collider>(f).unwrap().membership = 1 << 3;
        frames(&mut c, 1);
        let q = queries(&c);
        let down =
            |filter: QueryFilter| q.raycast(Vec3::new(0.0, 5.0, 0.0), -Vec3::Y, 100.0, &filter);
        let layers = |layers| QueryFilter {
            layers,
            ..Default::default()
        };
        assert!(down(layers(1 << 3)).is_some());
        assert!(down(layers(1 << 2)).is_none());
        let exclude = QueryFilter {
            exclude: Some(f),
            ..Default::default()
        };
        assert!(down(exclude).is_none());
    }

    #[test]
    fn raycast_skips_sensors_unless_asked() {
        let (mut c, f) = queries_core();
        c.world.get_mut::<Collider>(f).unwrap().sensor = true;
        frames(&mut c, 1);
        let q = queries(&c);
        let o = Vec3::new(0.0, 5.0, 0.0);
        assert!(
            q.raycast(o, -Vec3::Y, 100.0, &QueryFilter::default())
                .is_none()
        );
        let with = QueryFilter {
            sensors: true,
            ..Default::default()
        };
        assert!(q.raycast(o, -Vec3::Y, 100.0, &with).is_some());
    }

    #[test]
    fn overlaps_find_colliders() {
        let (c, f) = queries_core();
        let q = queries(&c);
        let any = QueryFilter::default();
        assert_eq!(q.overlap_sphere(Vec3::new(0.0, 0.6, 0.0), 0.3, &any), [f]);
        assert!(
            q.overlap_sphere(Vec3::new(0.0, 3.0, 0.0), 0.3, &any)
                .is_empty()
        );
        let half = Vec3::splat(0.2);
        let b = |y| q.overlap_box(Vec3::new(0.0, y, 0.0), half, Quat::IDENTITY, &any);
        assert_eq!(b(0.5), [f]);
        assert!(b(3.0).is_empty());
        let skip = QueryFilter {
            exclude: Some(f),
            ..Default::default()
        };
        assert!(
            q.overlap_sphere(Vec3::new(0.0, 0.6, 0.0), 0.3, &skip)
                .is_empty()
        );
    }

    #[test]
    fn sphere_cast_stops_at_the_floor() {
        let (c, f) = queries_core();
        let hit = queries(&c)
            .sphere_cast(
                Vec3::new(0.0, 5.0, 0.0),
                0.5,
                -Vec3::Y,
                100.0,
                &QueryFilter::default(),
            )
            .unwrap();
        assert_eq!(hit.entity, f);
        assert!((hit.distance - 4.0).abs() < 1e-2, "{}", hit.distance);
    }

    // ---- events ----

    #[derive(Resource, Default)]
    struct Seen(Vec<CollisionEvent>);

    fn collect(mut reader: MessageReader<CollisionEvent>, mut seen: BevyResMut<Seen>) {
        seen.0.extend(reader.read().copied());
    }

    fn events_core() -> Core {
        let mut c = core();
        c.world.init_resource::<Seen>();
        c.add_systems(FixedUpdate, collect.after(step_system));
        c
    }

    fn kinds(c: &Core, kind: CollisionEventKind) -> Vec<CollisionEvent> {
        c.world
            .resource::<Seen>()
            .0
            .iter()
            .filter(|e| e.kind == kind)
            .copied()
            .collect()
    }

    fn between(e: &CollisionEvent, x: Entity, y: Entity) -> bool {
        (e.a, e.b) == (x, y) || (e.a, e.b) == (y, x)
    }

    #[test]
    fn collision_events_start_and_end_once() {
        let mut c = events_core();
        let f = floor(&mut c);
        let body = dynamic(&mut c, at(0.0, 1.2, 0.0), cube());
        frames(&mut c, 120);
        let started = kinds(&c, CollisionEventKind::Started);
        assert_eq!(started.len(), 1, "{started:?}");
        assert!(between(&started[0], f, body));
        assert!(kinds(&c, CollisionEventKind::Ended).is_empty());
        // The floor leaves.
        c.world.get_mut::<Transform>(f).unwrap().translation.y = -50.0;
        frames(&mut c, 30);
        assert_eq!(kinds(&c, CollisionEventKind::Ended).len(), 1);
        assert_eq!(kinds(&c, CollisionEventKind::Started).len(), 1);
    }

    #[test]
    fn trigger_events_enter_and_exit_once() {
        let mut c = events_core();
        let trigger = c
            .world
            .spawn((
                at(0.0, 0.0, 0.0),
                Collider {
                    shape: ColliderShape::Box {
                        half_extents: Vec3::splat(1.0),
                    },
                    sensor: true,
                    ..Default::default()
                },
            ))
            .id();
        let body = dynamic(&mut c, at(0.0, 4.0, 0.0), cube());
        frames(&mut c, 120);
        let entered = kinds(&c, CollisionEventKind::TriggerEntered);
        let exited = kinds(&c, CollisionEventKind::TriggerExited);
        assert_eq!(entered.len(), 1, "{entered:?}");
        assert_eq!(exited.len(), 1, "{exited:?}");
        for e in entered.iter().chain(&exited) {
            assert!(between(e, trigger, body), "{e:?}");
        }
        assert!(kinds(&c, CollisionEventKind::Started).is_empty());
    }

    // ---- character controller ----

    fn character(c: &mut Core, y: f32) -> Entity {
        c.world
            .spawn((
                at(0.0, y, 0.0),
                Collider {
                    shape: ColliderShape::Capsule {
                        half_height: 0.5,
                        radius: 0.3,
                    },
                    ..Default::default()
                },
                CharacterController::default(),
                RigidBody {
                    kind: RigidBodyKind::Kinematic,
                    ..Default::default()
                },
            ))
            .id()
    }

    /// Ask every controller to walk along +X at 6 m/s, with a little gravity (rapier does
    /// not climb a ledge in slower steps with a round capsule bottom).
    fn walk_east(mut q: bevy_ecs::system::Query<&mut CharacterController>) {
        for mut cc in &mut q {
            cc.move_by(Vec3::new(0.1, -0.02, 0.0));
        }
    }

    fn character_core() -> Core {
        let mut c = core();
        c.add_systems(FixedUpdate, walk_east.before(character_controller_system));
        c
    }

    fn controller(c: &Core, e: Entity) -> CharacterController {
        c.world.get::<CharacterController>(e).unwrap().clone()
    }

    #[test]
    fn character_stands_on_the_floor_and_reports_grounded() {
        let mut c = character_core();
        floor(&mut c);
        // Capsule bottom at y - 0.8; floor top at 0.5.
        let e = character(&mut c, 2.0);
        frames(&mut c, 120);
        let cc = controller(&c, e);
        assert!(cc.grounded);
        assert!((height(&c, e) - 1.31).abs() < 0.05, "{}", height(&c, e));
        assert!(cc.applied.x > 0.0);
        assert!(c.world.get::<Transform>(e).unwrap().translation.x > 1.0);
    }

    #[test]
    fn character_walks_up_a_low_step() {
        let mut c = character_core();
        floor(&mut c);
        // Step top at 0.7: 0.2 above the floor, under the 0.3 limit.
        c.world.spawn((
            at(3.0, 0.6, 0.0),
            Collider {
                shape: ColliderShape::Box {
                    half_extents: Vec3::new(1.0, 0.1, 5.0),
                },
                ..Default::default()
            },
        ));
        let e = character(&mut c, 2.0);
        frames(&mut c, 10);
        let mut top = 0.0f32;
        for _ in 0..240 {
            frames(&mut c, 1);
            top = top.max(height(&c, e));
        }
        assert!(top > 1.45, "never stepped up, top y {top}");
    }

    #[test]
    fn character_stops_at_a_step_over_the_limit() {
        let mut c = character_core();
        floor(&mut c);
        // Step top at 1.5: 1 m above the floor.
        c.world.spawn((
            at(3.0, 1.0, 0.0),
            Collider {
                shape: ColliderShape::Box {
                    half_extents: Vec3::new(1.0, 0.5, 5.0),
                },
                ..Default::default()
            },
        ));
        let e = character(&mut c, 2.0);
        frames(&mut c, 300);
        let t = c.world.get::<Transform>(e).unwrap().translation;
        assert!(t.x < 2.0 && t.y < 1.5, "{t}");
    }

    #[test]
    fn character_stops_at_a_slope_over_the_limit() {
        let mut c = character_core();
        floor(&mut c);
        // A 60 degree ramp (limit 45) starting near x = 3.6.
        let mut ramp = at(4.0, 1.0, 0.0);
        ramp.rotation = Quat::from_rotation_z(60f32.to_radians());
        c.world.spawn((
            ramp,
            Collider {
                shape: ColliderShape::Box {
                    half_extents: Vec3::new(3.0, 0.1, 5.0),
                },
                ..Default::default()
            },
        ));
        let e = character(&mut c, 2.0);
        frames(&mut c, 300);
        let t = c.world.get::<Transform>(e).unwrap().translation;
        assert!(t.x < 3.7 && t.y < 1.5, "climbed to {t}");
    }

    // ---- joints ----

    #[test]
    fn hinge_joint_keeps_its_anchor() {
        let mut c = core();
        let pivot = Vec3::new(0.0, 5.0, 0.0);
        let parent = c
            .world
            .spawn((
                Transform::from_translation(pivot),
                RigidBody {
                    kind: RigidBodyKind::Fixed,
                    ..Default::default()
                },
            ))
            .id();
        // A body 2 m to the side, hinged on the parent about Z.
        let child = c
            .world
            .spawn((
                at(2.0, 0.0, 0.0),
                RigidBody::default(),
                cube(),
                Joint {
                    kind: JointKind::Hinge {
                        axis: Vec3::Z,
                        limited: false,
                        min: 0.0,
                        max: 0.0,
                    },
                    anchor: Vec3::new(-2.0, 0.0, 0.0),
                    ..Default::default()
                },
                ChildOf(parent),
            ))
            .id();
        let mut lowest = f32::MAX;
        for _ in 0..120 {
            frames(&mut c, 1);
            lowest = lowest.min(world_pos(&c, child).y);
            let h = c
                .world
                .get::<RigidBodyComponent>(child)
                .unwrap()
                .body_handle;
            let pw = c.world.resource::<PhysicsWorld>();
            let body = pw.bodies.get(h).unwrap();
            let anchor = body.translation() + *body.rotation() * Vec3::new(-2.0, 0.0, 0.0);
            assert!(
                (anchor - pivot).length() < 0.05,
                "anchor drifted to {anchor}"
            );
        }
        assert!(lowest < 4.0, "it should swing down, lowest {lowest}");
    }

    #[test]
    fn joint_without_ancestor_body_attaches_to_the_world() {
        let mut c = core();
        let e = c
            .world
            .spawn((
                at(0.0, 5.0, 0.0),
                RigidBody::default(),
                cube(),
                Joint {
                    parent_anchor: Vec3::new(0.0, 5.0, 0.0),
                    ..Default::default()
                },
            ))
            .id();
        frames(&mut c, 120);
        assert!((height(&c, e) - 5.0).abs() < 0.05, "fixed to the world");
        assert!(c.world.get::<JointComponent>(e).is_some());
        c.world.entity_mut(e).remove::<Joint>();
        frames(&mut c, 1);
        assert_eq!(c.world.resource::<PhysicsWorld>().impulse_joints.len(), 0);
    }

    // ---- mesh colliders ----

    fn mesh_collider() -> Collider {
        Collider {
            shape: ColliderShape::Mesh,
            ..Default::default()
        }
    }

    #[test]
    fn mesh_collider_of_the_builtin_cube_holds_a_body() {
        let mut c = core();
        let h: melon_core::Handle<melon_core::Model> = c
            .world
            .resource_mut::<melon_core::AssetServer>()
            .load("builtin:cube");
        c.world.spawn((at(0.0, 0.0, 0.0), mesh_collider(), h));
        let ball = dynamic(
            &mut c,
            at(0.0, 3.0, 0.0),
            Collider {
                shape: ColliderShape::Sphere { radius: 0.25 },
                ..Default::default()
            },
        );
        frames(&mut c, 300);
        // Cube top 0.5 + radius 0.25.
        assert!(
            (height(&c, ball) - 0.75).abs() < 0.05,
            "{}",
            height(&c, ball)
        );
    }

    #[test]
    fn mesh_collider_reads_a_glb_mesh() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut c = core();
        c.world.insert_resource(melon_core::AssetServer::new(root));
        let h: melon_core::Handle<melon_core::Model> = c
            .world
            .resource_mut::<melon_core::AssetServer>()
            .load("assets/models/cube.glb");
        let e = c.world.spawn((at(0.0, 0.0, 0.0), mesh_collider(), h)).id();
        frames(&mut c, 1);
        let handle = c.world.get::<ColliderComponent>(e).unwrap().collider_handle;
        let pw = c.world.resource::<PhysicsWorld>();
        let shape = pw
            .colliders
            .get(handle.expect("built"))
            .unwrap()
            .shared_shape();
        assert!(shape.as_trimesh().is_some());
    }

    #[test]
    fn mesh_collider_without_a_model_builds_nothing() {
        let mut c = core();
        let e = c.world.spawn((at(0.0, 0.0, 0.0), mesh_collider())).id();
        frames(&mut c, 3);
        assert_eq!(c.world.resource::<PhysicsWorld>().colliders.len(), 0);
        assert!(
            c.world
                .get::<ColliderComponent>(e)
                .unwrap()
                .collider_handle
                .is_none()
        );
    }

    // ---- edge cases ----

    mod edge {
        use super::*;

        fn handle_of(c: &Core, e: Entity) -> Option<ColliderHandle> {
            c.world.get::<ColliderComponent>(e).unwrap().collider_handle
        }

        fn sized(shape: ColliderShape) -> Collider {
            Collider {
                shape,
                ..Default::default()
            }
        }

        #[test]
        fn zero_negative_nan_and_infinite_sizes_never_panic() {
            let bad = [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
            for v in bad {
                let mut c = core();
                floor(&mut c);
                let shapes = [
                    ColliderShape::Box {
                        half_extents: Vec3::new(v, v, v),
                    },
                    ColliderShape::Sphere { radius: v },
                    ColliderShape::Capsule {
                        half_height: v,
                        radius: v,
                    },
                ];
                let ents: Vec<Entity> = shapes
                    .into_iter()
                    .map(|s| dynamic(&mut c, at(0.0, 3.0, 0.0), sized(s)))
                    .collect();
                frames(&mut c, 30);
                for e in ents {
                    assert!(handle_of(&c, e).is_some(), "size {v} built nothing");
                    let y = height(&c, e);
                    assert!(y.is_finite(), "size {v} gave y {y}");
                }
            }
        }

        #[test]
        fn nan_size_does_not_rebuild_the_collider_every_frame() {
            let mut c = core();
            let e = c
                .world
                .spawn((
                    at(0.0, 0.0, 0.0),
                    sized(ColliderShape::Sphere { radius: f32::NAN }),
                ))
                .id();
            frames(&mut c, 2);
            let first = handle_of(&c, e);
            frames(&mut c, 5);
            assert_eq!(handle_of(&c, e), first, "rebuilt although nothing changed");
        }

        #[test]
        fn bad_mass_damping_and_friction_never_panic() {
            let bad = [0.0, -5.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
            for v in bad {
                let mut c = core();
                floor(&mut c);
                let e = c
                    .world
                    .spawn((
                        at(0.0, 3.0, 0.0),
                        RigidBody {
                            mass: v,
                            linear_damping: v,
                            angular_damping: v,
                            ..Default::default()
                        },
                        Collider {
                            friction: v,
                            restitution: v,
                            ..Default::default()
                        },
                    ))
                    .id();
                frames(&mut c, 60);
                // Edited live too: the update path must not panic either.
                c.world.get_mut::<RigidBody>(e).unwrap().mass = v;
                c.world.get_mut::<RigidBody>(e).unwrap().gravity_scale = v;
                frames(&mut c, 5);
                let y = height(&c, e);
                assert!(y.is_finite(), "value {v} gave y {y}");
            }
        }

        fn turned_scaled_root() -> Transform {
            let mut t = at(10.0, 0.0, 0.0);
            t.scale = Vec3::splat(2.0);
            t.rotation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
            t
        }

        /// root (scale 2, turned 90 degrees about Y) > mid > leaf with a collider.
        fn deep_leaf(c: &mut Core, root_extra: impl bevy_ecs::bundle::Bundle) -> Entity {
            let root = c.world.spawn((turned_scaled_root(), root_extra)).id();
            let mid = c.world.spawn((at(0.0, 1.0, 0.0), ChildOf(root))).id();
            c.world
                .spawn((at(1.0, 0.0, 0.0), cube(), ChildOf(mid)))
                .id()
        }

        fn assert_leaf_pose(c: &Core, leaf: Entity) {
            let h = handle_of(c, leaf).unwrap();
            let pw = c.world.resource::<PhysicsWorld>();
            let co = pw.colliders.get(h).unwrap();
            // Local (1,1,0) * 2 = (2,2,0), turned 90 degrees about Y: (0,2,-2), plus the root.
            let p = co.position().translation;
            assert!((p - Vec3::new(10.0, 2.0, -2.0)).length() < 1e-3, "{p}");
            let he = co.shape().as_cuboid().unwrap().half_extents;
            assert!((he - Vec3::splat(1.0)).length() < 1e-3, "{he}");
            // World pose, also when the collider hangs on a body.
            let r = co.position().rotation;
            let want = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
            assert!(r.angle_between(want) < 1e-3, "{r}");
        }

        #[test]
        fn static_collider_on_a_deep_child_of_a_scaled_rotated_parent() {
            let mut c = core();
            let leaf = deep_leaf(&mut c, ());
            frames(&mut c, 2);
            assert_leaf_pose(&c, leaf);
        }

        #[test]
        fn body_collider_on_a_deep_child_of_a_scaled_rotated_body() {
            let mut c = core();
            gravity_off(&mut c);
            let leaf = deep_leaf(&mut c, RigidBody::default());
            frames(&mut c, 2);
            assert!(
                c.world
                    .resource::<PhysicsWorld>()
                    .colliders
                    .get(handle_of(&c, leaf).unwrap())
                    .unwrap()
                    .parent()
                    .is_some(),
                "attached to the ancestor body"
            );
            assert_leaf_pose(&c, leaf);
        }

        #[test]
        fn removing_the_body_keeps_the_collider_as_a_static_one() {
            let mut c = core();
            let e = dynamic(&mut c, at(0.0, 3.0, 0.0), cube());
            frames(&mut c, 3);
            c.world.entity_mut(e).remove::<RigidBody>();
            let y = height(&c, e);
            frames(&mut c, 60);
            let pw = c.world.resource::<PhysicsWorld>();
            assert_eq!((pw.bodies.len(), pw.colliders.len()), (0, 1));
            let co = pw.colliders.get(handle_of(&c, e).unwrap()).unwrap();
            assert!(co.parent().is_none(), "no body");
            assert!((height(&c, e) - y).abs() < 1e-6, "static: never moves");
            // And it is solid: a ball lands on it.
            let ball = dynamic(
                &mut c,
                at(0.0, y + 4.0, 0.0),
                sized(ColliderShape::Sphere { radius: 0.5 }),
            );
            frames(&mut c, 240);
            let ball_y = height(&c, ball);
            assert!((ball_y - (y + 1.0)).abs() < 0.05, "{ball_y}");
        }

        #[test]
        fn moving_a_static_collider_moves_it_in_rapier() {
            let mut c = core();
            let e = c.world.spawn((at(0.0, 0.0, 0.0), cube())).id();
            frames(&mut c, 2);
            c.world.get_mut::<Transform>(e).unwrap().translation.x = 7.0;
            frames(&mut c, 2);
            let pw = c.world.resource::<PhysicsWorld>();
            let p = pw
                .colliders
                .get(handle_of(&c, e).unwrap())
                .unwrap()
                .position();
            assert!((p.translation.x - 7.0).abs() < 1e-4);
        }

        #[derive(Component)]
        struct Slider;

        fn slide(mut q: Query<&mut Transform, With<Slider>>) {
            for mut t in &mut q {
                t.translation.x += 0.05;
            }
        }

        #[test]
        fn kinematic_body_moved_in_fixed_update_pushes_dynamic_bodies() {
            let mut c = core();
            gravity_off(&mut c);
            c.add_systems(FixedUpdate, slide.before(sync_to_physics_system));
            c.world.spawn((
                at(0.0, 0.0, 0.0),
                RigidBody {
                    kind: RigidBodyKind::Kinematic,
                    ..Default::default()
                },
                cube(),
                Slider,
            ));
            let pushed = dynamic(&mut c, at(1.2, 0.0, 0.0), cube());
            frames(&mut c, 120);
            // The wall travelled 6 m; the box is ahead of it.
            let x = c.world.get::<Transform>(pushed).unwrap().translation.x;
            assert!(x > 4.0, "box was not pushed: x {x}");
        }

        #[test]
        fn sensors_report_only_trigger_events() {
            let mut c = events_core();
            c.world.spawn((
                at(0.0, 0.0, 0.0),
                Collider {
                    shape: ColliderShape::Box {
                        half_extents: Vec3::splat(1.0),
                    },
                    sensor: true,
                    ..Default::default()
                },
            ));
            dynamic(&mut c, at(0.0, 4.0, 0.0), cube());
            frames(&mut c, 120);
            assert!(kinds(&c, CollisionEventKind::Started).is_empty());
            assert!(kinds(&c, CollisionEventKind::Ended).is_empty());
            assert!(!kinds(&c, CollisionEventKind::TriggerEntered).is_empty());
        }

        #[test]
        fn events_of_a_pair_are_not_duplicated_across_substeps() {
            let mut c = events_core();
            let f = floor(&mut c);
            let body = dynamic(&mut c, at(0.0, 1.2, 0.0), cube());
            // Three fixed steps per frame.
            run_frames(&mut c, 3.0 / 60.0, 60);
            let started = kinds(&c, CollisionEventKind::Started);
            assert_eq!(started.len(), 1, "{started:?}");
            assert!(between(&started[0], f, body));
            // Resting for a long time adds nothing.
            run_frames(&mut c, 3.0 / 60.0, 200);
            assert_eq!(kinds(&c, CollisionEventKind::Started).len(), 1);
            assert!(kinds(&c, CollisionEventKind::Ended).is_empty());
        }

        fn layered(membership: u32, filter: u32) -> Collider {
            Collider {
                membership,
                filter,
                ..Default::default()
            }
        }

        /// Height a box ends at over a floor, with the two (membership, filter) masks.
        fn rest_height(floor_mask: (u32, u32), body_mask: (u32, u32)) -> f32 {
            let mut c = core();
            c.world.spawn((
                at(0.0, 0.0, 0.0),
                Collider {
                    shape: ColliderShape::Box {
                        half_extents: Vec3::new(20.0, 0.5, 20.0),
                    },
                    ..layered(floor_mask.0, floor_mask.1)
                },
            ));
            let e = dynamic(&mut c, at(0.0, 3.0, 0.0), layered(body_mask.0, body_mask.1));
            frames(&mut c, 120);
            height(&c, e)
        }

        #[test]
        fn collision_layers_filter_in_both_directions() {
            // Control: both accept each other.
            assert!((rest_height((1, 2), (2, 1)) - 1.0).abs() < 0.05);
            // The floor does not accept the body's layer.
            assert!(rest_height((1, 4), (2, u32::MAX)) < 0.0);
            // The body does not accept the floor's layer.
            assert!(rest_height((1, u32::MAX), (2, 4)) < 0.0);
        }

        #[test]
        fn raycast_from_inside_a_collider_hits_it_at_distance_zero() {
            let (c, f) = queries_core();
            let hit = queries(&c)
                .raycast(Vec3::ZERO, Vec3::X, 10.0, &QueryFilter::default())
                .expect("the ray starts in the floor");
            assert_eq!(hit.entity, f);
            assert_eq!(hit.distance, 0.0);
            assert!(hit.point.is_finite() && hit.normal.is_finite());
        }

        #[test]
        fn raycast_all_orders_many_hits_by_distance() {
            let (mut c, _) = queries_core();
            // Spawned in a scrambled order, well apart.
            let spawned: Vec<(f32, Entity)> = [6.0, 2.0, 8.0, 4.0]
                .iter()
                .map(|y| (*y, c.world.spawn((at(0.0, *y, 0.0), cube())).id()))
                .collect();
            frames(&mut c, 1);
            let hits = queries(&c).raycast_all(
                Vec3::new(0.0, 20.0, 0.0),
                -Vec3::Y,
                100.0,
                &QueryFilter::default(),
            );
            assert_eq!(hits.len(), 5);
            assert!(hits.windows(2).all(|w| w[0].distance <= w[1].distance));
            let mut by_height = spawned;
            by_height.sort_by(|a, b| b.0.total_cmp(&a.0));
            let expected: Vec<Entity> = by_height.into_iter().map(|(_, e)| e).collect();
            let got: Vec<Entity> = hits.iter().take(4).map(|h| h.entity).collect();
            assert_eq!(got, expected);
        }

        #[test]
        fn queries_find_nothing_before_the_first_step() {
            let mut c = core();
            floor(&mut c);
            // The collider exists after a sync, but no fixed step has run yet.
            sync_components_system(&mut c.world);
            let f = QueryFilter::default();
            let from = Vec3::new(0.0, 5.0, 0.0);
            let q = queries(&c);
            assert!(q.raycast(from, -Vec3::Y, 50.0, &f).is_none());
            assert!(q.overlap_sphere(Vec3::ZERO, 5.0, &f).is_empty());
            assert!(q.sphere_cast(from, 0.5, -Vec3::Y, 50.0, &f).is_none());
            assert!(
                q.overlap_box(Vec3::ZERO, Vec3::ONE, Quat::IDENTITY, &f)
                    .is_empty()
            );
            // After one step the floor is visible.
            frames(&mut c, 1);
            assert!(queries(&c).raycast(from, -Vec3::Y, 50.0, &f).is_some());
        }

        #[test]
        fn degenerate_query_arguments_do_not_panic() {
            let (c, _) = queries_core();
            let q = queries(&c);
            let f = QueryFilter::default();
            let o = Vec3::new(0.0, 5.0, 0.0);
            assert!(q.raycast(o, Vec3::ZERO, 10.0, &f).is_none());
            assert!(q.raycast(o, Vec3::splat(f32::NAN), 10.0, &f).is_none());
            let _ = q.raycast(o, -Vec3::Y, f32::NAN, &f);
            let _ = q.sphere_cast(o, 0.0, -Vec3::Y, 10.0, &f);
            let _ = q.sphere_cast(o, -1.0, -Vec3::Y, 10.0, &f);
            let _ = q.overlap_sphere(o, -1.0, &f);
            let _ = q.overlap_sphere(o, f32::NAN, &f);
            let _ = q.overlap_box(o, Vec3::splat(-1.0), Quat::IDENTITY, &f);
        }

        // ---- character controller ----

        fn grounded_run(c: &mut Core, e: Entity, n: u32) -> Vec<bool> {
            (0..n)
                .map(|_| {
                    frames(c, 1);
                    controller(c, e).grounded
                })
                .collect()
        }

        fn stand(mut q: Query<&mut CharacterController>) {
            for mut cc in &mut q {
                cc.move_by(Vec3::new(0.0, -0.02, 0.0));
            }
        }

        #[test]
        fn grounded_does_not_flicker_while_standing_or_walking_on_flat_ground() {
            for walking in [false, true] {
                let mut c = core();
                if walking {
                    c.add_systems(FixedUpdate, walk_east.before(character_controller_system));
                } else {
                    c.add_systems(FixedUpdate, stand.before(character_controller_system));
                }
                floor(&mut c);
                // Starts just above the ground; the floor is 40 m wide, walking is 0.1 m a step.
                let e = character(&mut c, 1.33);
                frames(&mut c, 20);
                let g = grounded_run(&mut c, e, 150);
                assert!(g.iter().all(|g| *g), "walking {walking}: flickered {g:?}");
            }
        }

        #[test]
        fn character_stays_grounded_on_a_moving_kinematic_platform() {
            let mut c = core();
            c.add_systems(FixedUpdate, stand.before(character_controller_system));
            c.add_systems(FixedUpdate, slide.before(sync_to_physics_system));
            c.world.spawn((
                at(0.0, 0.0, 0.0),
                RigidBody {
                    kind: RigidBodyKind::Kinematic,
                    ..Default::default()
                },
                Collider {
                    shape: ColliderShape::Box {
                        half_extents: Vec3::new(30.0, 0.5, 5.0),
                    },
                    ..Default::default()
                },
                Slider,
            ));
            let e = character(&mut c, 2.0);
            frames(&mut c, 60);
            let g = grounded_run(&mut c, e, 120);
            assert!(g.iter().all(|g| *g), "lost the platform: {g:?}");
            let t = c.world.get::<Transform>(e).unwrap().translation;
            assert!(t.is_finite() && (t.y - 1.31).abs() < 0.1, "{t}");
        }

        #[test]
        fn character_walking_off_a_ledge_stops_being_grounded_and_falls() {
            let mut c = character_core();
            c.world.spawn((
                at(0.0, 0.0, 0.0),
                Collider {
                    shape: ColliderShape::Box {
                        half_extents: Vec3::new(4.0, 0.5, 5.0),
                    },
                    ..Default::default()
                },
            ));
            let e = character(&mut c, 1.33);
            frames(&mut c, 10);
            assert!(controller(&c, e).grounded);
            frames(&mut c, 200);
            assert!(!controller(&c, e).grounded, "still grounded past the edge");
            assert!(height(&c, e) < 1.3, "did not fall: {}", height(&c, e));
        }

        #[test]
        fn nan_joint_and_character_parameters_never_panic_or_poison_the_transform() {
            let n = f32::NAN;
            let mut c = character_core();
            floor(&mut c);
            let (parent, child) = jointed_pair(&mut c);
            for kind in [
                JointKind::Hinge {
                    axis: Vec3::splat(n),
                    limited: true,
                    min: n,
                    max: n,
                },
                JointKind::Slider {
                    axis: Vec3::ZERO,
                    limited: true,
                    min: 1.0,
                    max: -1.0,
                },
                JointKind::Ball {
                    limited: true,
                    cone_limit: n,
                },
            ] {
                c.world.get_mut::<Joint>(child).unwrap().kind = kind;
                c.world.get_mut::<Joint>(child).unwrap().anchor = Vec3::splat(n);
                frames(&mut c, 5);
            }
            let _ = parent;
            let e = character(&mut c, 1.33);
            {
                let mut cc = c.world.get_mut::<CharacterController>(e).unwrap();
                cc.max_slope = n;
                cc.offset = n;
                cc.max_step_height = n;
                cc.snap_to_ground = n;
            }
            frames(&mut c, 10);
            {
                let mut cc = c.world.get_mut::<CharacterController>(e).unwrap();
                cc.max_slope = -5.0;
                cc.offset = -1.0;
                cc.max_step_height = -1.0;
                cc.snap_to_ground = -1.0;
            }
            frames(&mut c, 10);
            let t = c.world.get::<Transform>(e).unwrap().translation;
            assert!(t.is_finite(), "{t}");
        }

        fn nan_mover(mut q: Query<&mut CharacterController>) {
            for mut cc in &mut q {
                cc.move_by(Vec3::splat(f32::NAN));
            }
        }

        #[test]
        fn a_nan_move_request_leaves_the_character_where_it_is() {
            let mut c = core();
            c.add_systems(FixedUpdate, nan_mover.before(character_controller_system));
            floor(&mut c);
            let e = character(&mut c, 1.33);
            frames(&mut c, 10);
            let t = c.world.get::<Transform>(e).unwrap().translation;
            assert!(t.is_finite(), "{t}");
        }

        // ---- joints ----

        fn jointed_pair(c: &mut Core) -> (Entity, Entity) {
            let parent = c
                .world
                .spawn((
                    at(0.0, 5.0, 0.0),
                    RigidBody {
                        kind: RigidBodyKind::Fixed,
                        ..Default::default()
                    },
                ))
                .id();
            let child = c
                .world
                .spawn((
                    at(0.0, -1.0, 0.0),
                    RigidBody::default(),
                    cube(),
                    Joint {
                        anchor: Vec3::new(0.0, 1.0, 0.0),
                        ..Default::default()
                    },
                    ChildOf(parent),
                ))
                .id();
            (parent, child)
        }

        #[test]
        fn deleting_the_ancestor_body_of_a_joint_removes_it_without_panic() {
            let mut c = core();
            let (parent, child) = jointed_pair(&mut c);
            frames(&mut c, 5);
            assert_eq!(c.world.resource::<PhysicsWorld>().impulse_joints.len(), 1);
            melon_core::hierarchy::mark_for_delete(&mut c.world, parent);
            frames(&mut c, 3);
            let pw = c.world.resource::<PhysicsWorld>();
            assert_eq!(pw.impulse_joints.len(), 0);
            assert!(pw.colliders.is_empty());
            assert!(c.world.get_entity(child).is_err());
        }

        #[test]
        fn removing_only_the_ancestor_body_reattaches_the_joint_to_the_world() {
            let mut c = core();
            let (parent, child) = jointed_pair(&mut c);
            frames(&mut c, 5);
            c.world.entity_mut(parent).remove::<RigidBody>();
            frames(&mut c, 3);
            assert_eq!(c.world.resource::<PhysicsWorld>().impulse_joints.len(), 1);
            assert!(c.world.get::<JointComponent>(child).is_some());
            frames(&mut c, 60);
            assert!(height(&c, child).is_finite());
        }

        #[test]
        fn despawning_a_jointed_body_without_the_marker_removes_its_joint() {
            let mut c = core();
            let (_, child) = jointed_pair(&mut c);
            frames(&mut c, 5);
            c.world.despawn(child);
            frames(&mut c, 3);
            assert_eq!(c.world.resource::<PhysicsWorld>().impulse_joints.len(), 0);
        }

        // ---- mesh colliders ----

        #[test]
        fn mesh_collider_with_a_missing_or_broken_file_builds_nothing() {
            let dir = std::env::temp_dir().join("melon_physics_broken_mesh");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("broken.glb"), b"glTF this is not a model").unwrap();
            std::fs::write(dir.join("empty.glb"), b"").unwrap();
            let mut c = core();
            c.world.insert_resource(melon_core::AssetServer::new(dir));
            let mut ents = Vec::new();
            for path in ["missing.glb", "broken.glb", "empty.glb", "broken.glb#mesh9"] {
                let h: melon_core::Handle<melon_core::Model> =
                    c.world.resource_mut::<melon_core::AssetServer>().load(path);
                ents.push(c.world.spawn((at(0.0, 0.0, 0.0), mesh_collider(), h)).id());
            }
            frames(&mut c, 5);
            for e in ents {
                assert!(handle_of(&c, e).is_none());
            }
            assert_eq!(c.world.resource::<PhysicsWorld>().colliders.len(), 0);
        }
    }
}
