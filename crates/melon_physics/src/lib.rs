use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::query::{With, Without};
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::{Commands, Query, Res, ResMut};
use glam::{Quat, Vec3};

pub use rapier3d;
pub use rapier3d::dynamics::RigidBodyType;
use rapier3d::dynamics::{
    CCDSolver, ImpulseJointSet, IslandManager, MultibodyJointSet, RigidBodyHandle, RigidBodySet,
};
use rapier3d::geometry::{
    BroadPhaseBvh, ColliderBuilder, ColliderHandle, ColliderSet, NarrowPhase,
};
use rapier3d::math::Vector;
use rapier3d::pipeline::PhysicsPipeline;
use rapier3d::prelude::IntegrationParameters;
use tracing::instrument;

use melon_core::hierarchy::{propagate_transforms_system, safe_inverse};
use melon_core::{
    ChildOf, Core, FixedUpdate, GlobalTransform, PendingDelete, PostUpdate, PreviousTransform,
    Time, Transform, store_previous_transforms_system,
};

/// Gravity vector in m/s^2. Default is Earth gravity pointing down (-Y).
const GRAVITY: Vector = Vector::new(0.0, -9.81, 0.0);

/// ECS component storing a handle to a rapier rigid body.
/// Paired with a `Transform` component on the same entity.
#[derive(Component, Debug, Clone, Copy)]
pub struct RigidBodyComponent {
    pub body_handle: RigidBodyHandle,
    pub body_type: RigidBodyType,
}

/// ECS component storing a handle to a rapier collider.
#[derive(Component, Debug, Clone, Copy)]
pub struct ColliderComponent {
    pub collider_handle: ColliderHandle,
}

/// Resource holding the rapier physics world state.
/// One instance per `Core` world, stepped once per frame.
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
        }
    }

    /// Spawn a dynamic rigid body with a cuboid collider at the given position.
    /// Returns the handles for attaching to an ECS entity.
    #[instrument(skip(self), level = "debug")]
    pub fn spawn_dynamic_cuboid(
        &mut self,
        translation: Vec3,
        rotation: Quat,
        half_extents: Vec3,
    ) -> (RigidBodyHandle, ColliderHandle) {
        use rapier3d::dynamics::RigidBodyBuilder;
        let body = RigidBodyBuilder::dynamic()
            .pose(rapier3d::math::Pose::from_parts(translation, rotation))
            .build();
        let body_handle = self.bodies.insert(body);
        let collider =
            ColliderBuilder::cuboid(half_extents.x, half_extents.y, half_extents.z).build();
        let collider_handle =
            self.colliders
                .insert_with_parent(collider, body_handle, &mut self.bodies);
        (body_handle, collider_handle)
    }

    /// Spawn a fixed rigid body (static geometry like a floor) with a cuboid collider.
    #[instrument(skip(self), level = "debug")]
    pub fn spawn_fixed_cuboid(
        &mut self,
        translation: Vec3,
        rotation: Quat,
        half_extents: Vec3,
    ) -> (RigidBodyHandle, ColliderHandle) {
        use rapier3d::dynamics::RigidBodyBuilder;
        let body = RigidBodyBuilder::fixed()
            .pose(rapier3d::math::Pose::from_parts(translation, rotation))
            .build();
        let body_handle = self.bodies.insert(body);
        let collider =
            ColliderBuilder::cuboid(half_extents.x, half_extents.y, half_extents.z).build();
        let collider_handle =
            self.colliders
                .insert_with_parent(collider, body_handle, &mut self.bodies);
        (body_handle, collider_handle)
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
            &(),
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

    /// Remove a standalone collider (not attached to a rigid body).
    /// Wakes up the parent body if one exists.
    #[instrument(skip(self), level = "debug")]
    pub fn remove_collider(&mut self, handle: ColliderHandle) {
        self.colliders
            .remove(handle, &mut self.islands, &mut self.bodies, true);
    }
}

/// System: step the physics world by one `Time::fixed_delta`.
/// Runs in `FixedUpdate`, before sync_from_physics_system.
pub fn step_system(mut world: ResMut<PhysicsWorld>, time: Res<Time>) {
    world.integration_parameters.dt = time.fixed_delta;
    world.step();
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
/// remember previous transforms, update world poses, push ECS edits into
/// rapier, step, pull results back.
pub fn add_physics_systems(core: &mut Core) {
    core.add_systems(
        FixedUpdate,
        (
            store_previous_transforms_system,
            propagate_transforms_system,
            sync_to_physics_system,
            step_system,
            sync_from_physics_system,
        )
            .chain(),
    );
    core.add_systems(
        PostUpdate,
        (cleanup_deleted_system, add_previous_transforms_system),
    );
}

/// System: sync rapier body poses back into ECS Transform components.
/// Only updates dynamic bodies, which physics owns in world space: a child's
/// local transform is rewritten so its world pose matches the body (it does
/// not follow its parent). Fixed bodies keep their ECS transform as the
/// source of truth.
pub fn sync_from_physics_system(
    physics: Res<PhysicsWorld>,
    parents: Query<&GlobalTransform>,
    mut query: Query<(&RigidBodyComponent, &mut Transform, Option<&ChildOf>)>,
) {
    for (rb, mut transform, child_of) in &mut query {
        if rb.body_type != RigidBodyType::Dynamic {
            continue;
        }
        let Some(body) = physics.bodies.get(rb.body_handle) else {
            continue;
        };
        let (translation, rotation) = (body.translation(), *body.rotation());
        match child_of.and_then(|c| parents.get(c.parent()).ok()) {
            Some(parent) => {
                let inv = safe_inverse(parent.0);
                let local = Transform::from_mat4(
                    inv * glam::Mat4::from_rotation_translation(rotation, translation),
                );
                if local.translation.is_finite() && local.rotation.is_finite() {
                    transform.translation = local.translation;
                    transform.rotation = local.rotation;
                }
            }
            None => {
                transform.translation = translation;
                transform.rotation = rotation;
            }
        }
    }
}

/// System: sync ECS world poses into rapier for fixed bodies (they follow
/// their parent). Dynamic bodies are controlled by physics, so their ECS
/// transform is NOT written back to rapier.
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
            body.set_translation(world.translation, true);
            body.set_rotation(world.rotation, true);
        }
    }
}

/// System: cleanup entities marked with PendingDelete.
/// Removes rapier rigid bodies and colliders before despawning the ECS entity.
/// Uses RigidBodySet::remove with remove_attached_colliders=true to clean up
/// colliders automatically (rapier3d 0.34 API).
/// Run this after editor operations, before render.
#[allow(clippy::type_complexity)]
pub fn cleanup_deleted_system(
    mut commands: Commands,
    mut physics: ResMut<PhysicsWorld>,
    query: Query<
        (
            Entity,
            Option<&RigidBodyComponent>,
            Option<&ColliderComponent>,
            Option<&ChildOf>,
        ),
        With<PendingDelete>,
    >,
    marked: Query<(), With<PendingDelete>>,
) {
    for (entity, rb_opt, collider_opt, child_of) in &query {
        if let Some(rb) = rb_opt {
            physics.remove_body(rb.body_handle);
        } else if let Some(col) = collider_opt {
            physics.remove_collider(col.collider_handle);
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
    use bevy_ecs::schedule::Schedule;
    use bevy_ecs::world::World;
    use melon_core::Name;

    #[test]
    fn physics_world_default_has_no_bodies() {
        let world = PhysicsWorld::new();
        assert!(world.bodies.is_empty());
        assert!(world.colliders.is_empty());
        assert!((world.gravity.y + 9.81).abs() < 1e-6);
    }

    #[test]
    fn spawn_dynamic_cuboid_creates_body_and_collider() {
        let mut pw = PhysicsWorld::new();
        let (body_h, collider_h) = pw.spawn_dynamic_cuboid(
            Vec3::new(0.0, 5.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(0.5, 0.5, 0.5),
        );
        assert!(pw.bodies.get(body_h).is_some());
        assert!(pw.colliders.get(collider_h).is_some());
        assert_eq!(pw.bodies.len(), 1);
        assert_eq!(pw.colliders.len(), 1);
    }

    #[test]
    fn spawn_fixed_cuboid_is_fixed_type() {
        let mut pw = PhysicsWorld::new();
        let (body_h, _) = pw.spawn_fixed_cuboid(
            Vec3::new(0.0, -1.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(10.0, 0.5, 10.0),
        );
        let body = pw.bodies.get(body_h).unwrap();
        assert_eq!(body.body_type(), RigidBodyType::Fixed);
    }

    #[test]
    fn step_makes_dynamic_body_fall() {
        let mut pw = PhysicsWorld::new();
        let (body_h, _) = pw.spawn_dynamic_cuboid(
            Vec3::new(0.0, 10.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(0.5, 0.5, 0.5),
        );
        let y_before = pw.bodies.get(body_h).unwrap().translation().y;
        pw.step();
        let y_after = pw.bodies.get(body_h).unwrap().translation().y;
        assert!(
            y_after < y_before,
            "dynamic body should fall (y_before={y_before}, y_after={y_after})"
        );
    }

    #[test]
    fn fixed_body_does_not_fall() {
        let mut pw = PhysicsWorld::new();
        let (body_h, _) = pw.spawn_fixed_cuboid(
            Vec3::new(0.0, 0.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(10.0, 0.5, 10.0),
        );
        let y_before = pw.bodies.get(body_h).unwrap().translation().y;
        pw.step();
        let y_after = pw.bodies.get(body_h).unwrap().translation().y;
        assert!(
            (y_after - y_before).abs() < 1e-6,
            "fixed body should not move"
        );
    }

    #[test]
    fn dynamic_body_lands_on_floor() {
        let mut pw = PhysicsWorld::new();
        // Floor at y=0, half-extent 0.5 thick, so top surface at y=0.5
        let _ = pw.spawn_fixed_cuboid(
            Vec3::new(0.0, 0.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(10.0, 0.5, 10.0),
        );
        // Cube at y=5, half-extent 0.5, so bottom at y=4.5
        let (body_h, _) = pw.spawn_dynamic_cuboid(
            Vec3::new(0.0, 5.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(0.5, 0.5, 0.5),
        );

        // Step many times to let it fall and settle
        for _ in 0..300 {
            pw.step();
        }

        let y_final = pw.bodies.get(body_h).unwrap().translation().y;
        // Cube should rest on floor: cube center at y=1.0 (floor top 0.5 + cube half 0.5)
        assert!(
            (y_final - 1.0).abs() < 0.1,
            "cube should rest on floor at y~1.0, got y={y_final}"
        );
    }

    #[test]
    fn sync_from_physics_updates_transform() {
        let mut pw = PhysicsWorld::new();
        let (body_h, _) = pw.spawn_dynamic_cuboid(
            Vec3::new(0.0, 10.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(0.5, 0.5, 0.5),
        );
        pw.step();

        let mut world = World::new();
        world.insert_resource(pw);
        let entity = world
            .spawn((
                RigidBodyComponent {
                    body_handle: body_h,
                    body_type: RigidBodyType::Dynamic,
                },
                Transform::default(),
            ))
            .id();

        let mut schedule = Schedule::default();
        schedule.add_systems(sync_from_physics_system);
        schedule.run(&mut world);

        let transform = world.get::<Transform>(entity).unwrap();
        let physics = world.get_resource::<PhysicsWorld>().unwrap();
        let body_y = physics.body_translation(body_h).unwrap().y;
        assert!(
            (transform.translation.y - body_y).abs() < 1e-6,
            "transform should match physics body y={body_y}, got {}",
            transform.translation.y
        );
    }

    #[test]
    fn sync_to_physics_updates_kinematic_body() {
        let mut pw = PhysicsWorld::new();
        let (body_h, _) = pw.spawn_fixed_cuboid(
            Vec3::new(0.0, 0.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(1.0, 1.0, 1.0),
        );

        let mut world = World::new();
        world.insert_resource(pw);
        world.spawn((
            RigidBodyComponent {
                body_handle: body_h,
                body_type: RigidBodyType::Fixed,
            },
            Transform {
                translation: Vec3::new(5.0, 5.0, 5.0),
                ..Default::default()
            },
        ));

        let mut schedule = Schedule::default();
        schedule.add_systems(sync_to_physics_system);
        schedule.run(&mut world);

        let physics = world.get_resource::<PhysicsWorld>().unwrap();
        let body = physics.bodies.get(body_h).unwrap();
        assert!(
            (body.translation().y - 5.0).abs() < 1e-6,
            "fixed body should be moved to y=5.0, got y={}",
            body.translation().y
        );
    }

    #[test]
    fn clear_empties_all_sets() {
        let mut pw = PhysicsWorld::new();
        pw.spawn_dynamic_cuboid(
            Vec3::new(0.0, 5.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(0.5, 0.5, 0.5),
        );
        pw.spawn_fixed_cuboid(
            Vec3::new(0.0, 0.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(1.0, 1.0, 1.0),
        );
        assert!(!pw.bodies.is_empty());
        assert!(!pw.colliders.is_empty());

        pw.clear();

        assert!(pw.bodies.is_empty(), "bodies should be empty after clear");
        assert!(
            pw.colliders.is_empty(),
            "colliders should be empty after clear"
        );
        assert!(
            pw.impulse_joints.is_empty(),
            "impulse joints should be empty after clear"
        );
    }

    #[test]
    fn clear_preserves_gravity() {
        let mut pw = PhysicsWorld::new();
        pw.gravity = Vector::new(1.0, -20.0, 0.0);
        pw.spawn_dynamic_cuboid(
            Vec3::new(0.0, 5.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(0.5, 0.5, 0.5),
        );

        pw.clear();

        assert!(
            (pw.gravity.y - (-20.0)).abs() < 1e-6,
            "gravity should be preserved after clear, got y={}",
            pw.gravity.y
        );
        assert!(
            (pw.gravity.x - 1.0).abs() < 1e-6,
            "gravity x should be preserved after clear, got x={}",
            pw.gravity.x
        );
    }

    #[test]
    fn cleanup_removes_physics_body() {
        let mut pw = PhysicsWorld::new();
        let (body_h, collider_h) = pw.spawn_dynamic_cuboid(
            Vec3::new(0.0, 5.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(0.5, 0.5, 0.5),
        );

        let mut world = World::new();
        world.insert_resource(pw);
        let entity = world
            .spawn((
                RigidBodyComponent {
                    body_handle: body_h,
                    body_type: RigidBodyType::Dynamic,
                },
                ColliderComponent {
                    collider_handle: collider_h,
                },
                Transform::default(),
                PendingDelete,
            ))
            .id();

        let mut schedule = Schedule::default();
        schedule.add_systems(cleanup_deleted_system);
        schedule.run(&mut world);

        let physics = world.get_resource::<PhysicsWorld>().unwrap();
        assert!(
            physics.bodies.get(body_h).is_none(),
            "body should be removed from physics world"
        );
        assert!(
            physics.colliders.get(collider_h).is_none(),
            "collider should be removed from physics world"
        );
        assert!(
            world.get_entity(entity).is_err(),
            "entity should be despawned"
        );
    }

    #[test]
    fn cleanup_handles_entity_without_physics() {
        let pw = PhysicsWorld::new();
        let mut world = World::new();
        world.insert_resource(pw);
        let entity = world.spawn((Name("Test".to_string()), PendingDelete)).id();

        let mut schedule = Schedule::default();
        schedule.add_systems(cleanup_deleted_system);
        schedule.run(&mut world);

        assert!(
            world.get_entity(entity).is_err(),
            "entity without physics should still be despawned"
        );
    }

    fn drop_test_core() -> (melon_core::Core, Entity, Entity) {
        let mut core = melon_core::Core::new();
        let mut pw = PhysicsWorld::new();
        let start = Vec3::new(0.0, 10.0, 0.0);
        let (body, _) = pw.spawn_dynamic_cuboid(start, Quat::IDENTITY, Vec3::splat(0.5));
        let (fixed_body, _) =
            pw.spawn_fixed_cuboid(Vec3::new(0.0, -50.0, 0.0), Quat::IDENTITY, Vec3::splat(1.0));
        core.world.insert_resource(pw);
        let dynamic = core
            .world
            .spawn((
                Transform::from_translation(start),
                RigidBodyComponent {
                    body_handle: body,
                    body_type: RigidBodyType::Dynamic,
                },
            ))
            .id();
        let fixed = core
            .world
            .spawn((
                Transform::from_translation(Vec3::new(0.0, -50.0, 0.0)),
                RigidBodyComponent {
                    body_handle: fixed_body,
                    body_type: RigidBodyType::Fixed,
                },
            ))
            .id();
        core.world
            .insert_resource(melon_core::SimulationState::Playing);
        add_physics_systems(&mut core);
        (core, dynamic, fixed)
    }

    fn run_frames(core: &mut melon_core::Core, dt: f32, n: u32) {
        for _ in 0..n {
            core.world.resource_mut::<melon_core::Time>().update(dt);
            core.update();
        }
    }

    fn height(core: &melon_core::Core, e: Entity) -> f32 {
        core.world.get::<Transform>(e).unwrap().translation.y
    }

    #[test]
    fn fall_height_independent_of_frame_rate() {
        let (mut core30, e30, _) = drop_test_core();
        run_frames(&mut core30, 1.0 / 30.0, 30);
        let (mut core144, e144, _) = drop_test_core();
        run_frames(&mut core144, 1.0 / 144.0, 144);
        let (y30, y144) = (height(&core30, e30), height(&core144, e144));
        // 0.2 m is about one fixed step of fall after 1 s.
        assert!((y30 - y144).abs() < 0.2, "{y30} vs {y144}");
        // Free fall for 1 s is ~4.9 m, not 2.4x that.
        assert!(y30 < 6.0 && y30 > 4.0, "y30 = {y30}");
    }

    #[test]
    fn paused_body_does_not_fall() {
        let (mut core, e, _) = drop_test_core();
        core.world
            .insert_resource(melon_core::SimulationState::Paused);
        run_frames(&mut core, 1.0 / 60.0, 60);
        assert_eq!(height(&core, e), 10.0);
    }

    #[test]
    fn dynamic_body_gets_previous_transform_equal_to_current() {
        let (mut core, e, _) = drop_test_core();
        run_frames(&mut core, 0.001, 1);
        let prev = core
            .world
            .get::<PreviousTransform>(e)
            .expect("no PreviousTransform");
        let cur = core.world.get::<Transform>(e).unwrap();
        assert_eq!(prev.0.translation, cur.translation);
    }

    #[test]
    fn fixed_body_gets_no_previous_transform() {
        let (mut core, _, fixed) = drop_test_core();
        run_frames(&mut core, 1.0 / 60.0, 2);
        assert!(core.world.get::<PreviousTransform>(fixed).is_none());
    }

    /// Parent (no body) at the origin with a child body at local (1,0,0).
    /// Gravity off, so only parent motion could move the child.
    fn parented_core(kind: RigidBodyType) -> (melon_core::Core, Entity, Entity) {
        let mut core = melon_core::Core::new();
        let mut pw = PhysicsWorld::new();
        pw.gravity = Vector::ZERO;
        let at = Vec3::X;
        let (body, _) = match kind {
            RigidBodyType::Dynamic => pw.spawn_dynamic_cuboid(at, Quat::IDENTITY, Vec3::splat(0.5)),
            _ => pw.spawn_fixed_cuboid(at, Quat::IDENTITY, Vec3::splat(0.5)),
        };
        core.world.insert_resource(pw);
        let parent = core.world.spawn(Transform::default()).id();
        let child = core
            .world
            .spawn((
                Transform::from_translation(at),
                RigidBodyComponent {
                    body_handle: body,
                    body_type: kind,
                },
                melon_core::ChildOf(parent),
            ))
            .id();
        core.world
            .insert_resource(melon_core::SimulationState::Playing);
        add_physics_systems(&mut core);
        (core, parent, child)
    }

    fn body_pos(core: &melon_core::Core, e: Entity) -> Vec3 {
        let h = core.world.get::<RigidBodyComponent>(e).unwrap().body_handle;
        core.world
            .resource::<PhysicsWorld>()
            .body_translation(h)
            .unwrap()
    }

    #[test]
    fn fixed_child_body_follows_moved_parent() {
        let (mut core, parent, child) = parented_core(RigidBodyType::Fixed);
        core.world.get_mut::<Transform>(parent).unwrap().translation = Vec3::new(5.0, 0.0, 0.0);
        run_frames(&mut core, 1.0 / 60.0, 1);
        assert!((body_pos(&core, child) - Vec3::new(6.0, 0.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn dynamic_child_ignores_parent_motion() {
        let (mut core, parent, child) = parented_core(RigidBodyType::Dynamic);
        core.world.get_mut::<Transform>(parent).unwrap().translation = Vec3::new(5.0, 0.0, 0.0);
        run_frames(&mut core, 1.0 / 60.0, 1);
        let body = body_pos(&core, child);
        assert!((body - Vec3::X).length() < 1e-4, "body moved to {body}");
        let world = melon_core::hierarchy::world_matrix(&core.world, child);
        assert!((world.transform_point3(Vec3::ZERO) - body).length() < 1e-4);
    }

    #[test]
    fn dynamic_sync_under_zero_scale_parent_stays_finite() {
        let (mut core, parent, child) = parented_core(RigidBodyType::Dynamic);
        core.world.get_mut::<Transform>(parent).unwrap().scale = Vec3::new(0.0, 1.0, 1.0);
        run_frames(&mut core, 1.0 / 60.0, 1);
        let t = core.world.get::<Transform>(child).unwrap();
        assert!(t.translation.is_finite() && t.rotation.is_finite());
    }

    #[test]
    fn deleting_parent_and_child_same_frame_removes_both_bodies() {
        let (mut core, parent, child) = parented_core(RigidBodyType::Dynamic);
        core.world.entity_mut(parent).insert(PendingDelete);
        core.world.entity_mut(child).insert(PendingDelete);
        run_frames(&mut core, 0.001, 1);
        assert_eq!(core.world.resource::<PhysicsWorld>().bodies.len(), 0);
        assert!(core.world.get_entity(parent).is_err());
        assert!(core.world.get_entity(child).is_err());
    }

    #[test]
    fn deleting_only_parent_with_mark_for_delete_removes_child_body() {
        let (mut core, parent, child) = parented_core(RigidBodyType::Dynamic);
        melon_core::hierarchy::mark_for_delete(&mut core.world, parent);
        run_frames(&mut core, 0.001, 1);
        assert_eq!(core.world.resource::<PhysicsWorld>().bodies.len(), 0);
        assert!(core.world.get_entity(child).is_err());
    }

    #[test]
    fn deleting_new_dynamic_body_does_not_panic() {
        let (mut core, e, _) = drop_test_core();
        core.world.entity_mut(e).insert(PendingDelete);
        run_frames(&mut core, 0.001, 2);
        assert!(
            core.world.get_entity(e).is_err(),
            "entity should be despawned"
        );
    }
}
