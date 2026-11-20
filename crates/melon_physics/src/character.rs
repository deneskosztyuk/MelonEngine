//! `CharacterController`: rapier's kinematic character controller moves the
//! entity's `Transform` by the move the game asked for.

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use glam::Vec3;
use melon_core::hierarchy::{set_world_transform, world_matrix};
use melon_core::{CharacterController, Time, Transform};
use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::math::Pose;
use rapier3d::pipeline::QueryFilter;

use crate::{ColliderComponent, PhysicsWorld};

/// System: apply each controller's `move_by` request (sliding along walls,
/// stepping up, snapping to the ground) to its entity's `Transform`, and
/// fill `grounded` and `applied`. Needs a `Collider` on the same entity.
/// Sees colliders as of the last physics step.
pub fn character_controller_system(world: &mut World) {
    let dt = world
        .get_resource::<Time>()
        .map_or(1.0 / 60.0, |t| t.fixed_delta);
    let entities: Vec<Entity> = world
        .query_filtered::<Entity, bevy_ecs::query::With<CharacterController>>()
        .iter(world)
        .collect();
    for e in entities {
        let Some(handle) = world
            .get::<ColliderComponent>(e)
            .and_then(|c| c.collider_handle)
        else {
            continue;
        };
        let (cc, desired) = {
            let mut cc = world.get_mut::<CharacterController>(e).expect("queried");
            let desired = cc.take_desired();
            (
                cc.clone(),
                if desired.is_finite() {
                    desired
                } else {
                    Vec3::ZERO
                },
            )
        };
        let (scale, rotation, translation) = world_matrix(world, e).to_scale_rotation_translation();
        let moved = world.resource_scope(|_, physics: bevy_ecs::prelude::Mut<PhysicsWorld>| {
            let scene = physics.scene_read();
            let collider = scene.colliders.get(handle)?;
            let shape = collider.shared_shape().clone();
            let groups = collider.collision_groups();
            // Hand-typed numbers rapier panics on (a NaN or negative offset) fall back.
            let ok = |v: f32, fallback: f32| {
                if v.is_finite() && v >= 0.0 {
                    v
                } else {
                    fallback
                }
            };
            let cc = CharacterController {
                offset: ok(cc.offset, 0.01),
                max_slope: ok(cc.max_slope, 45.0),
                max_step_height: ok(cc.max_step_height, 0.0),
                snap_to_ground: ok(cc.snap_to_ground, 0.0),
                ..cc
            };
            let controller = KinematicCharacterController {
                offset: CharacterLength::Absolute(cc.offset),
                max_slope_climb_angle: cc.max_slope.to_radians(),
                min_slope_slide_angle: cc.max_slope.to_radians().min(0.5),
                autostep: (cc.max_step_height > 0.0).then_some(CharacterAutostep {
                    max_height: CharacterLength::Absolute(cc.max_step_height),
                    min_width: CharacterLength::Absolute(0.02),
                    include_dynamic_bodies: false,
                }),
                snap_to_ground: (cc.snap_to_ground > 0.0)
                    .then_some(CharacterLength::Absolute(cc.snap_to_ground)),
                ..Default::default()
            };
            let filter = QueryFilter::new()
                .exclude_sensors()
                .exclude_collider(handle)
                .groups(groups);
            let pipeline = scene.broad_phase.as_query_pipeline(
                physics.narrow_phase.query_dispatcher(),
                &physics.bodies,
                &scene.colliders,
                filter,
            );
            // The collider sits at the entity's pose (it is on the entity itself).
            let result = controller.move_shape(
                dt,
                &pipeline,
                shape.as_ref(),
                &Pose::from_parts(translation, rotation),
                desired,
                |_| {},
            );
            Some((result.translation, result.grounded))
        });
        let Some((applied, grounded)) = moved else {
            continue;
        };
        if let Some(mut cc) = world.get_mut::<CharacterController>(e) {
            cc.applied = applied;
            cc.grounded = grounded;
        }
        if applied != Vec3::ZERO {
            set_world_transform(
                world,
                e,
                Transform {
                    translation: translation + applied,
                    rotation,
                    scale,
                },
            );
        }
    }
}
