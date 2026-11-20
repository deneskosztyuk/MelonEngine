//! Scene queries for game code. After each physics step `Snapshot::of`
//! copies the broad phase and colliders, so game systems can call
//! `PhysicsQueries` synchronously without borrowing the `PhysicsWorld`.
//! Cost: one clone per fixed step (shapes are shared, not copied).

use bevy_ecs::entity::Entity;
use glam::{Quat, Vec3};
use melon_core::{QueryBackend, QueryFilter, RayHit};
use rapier3d::dynamics::RigidBodySet;
use rapier3d::geometry::{BroadPhaseBvh, Collider, ColliderHandle, ColliderSet};
use rapier3d::math::Pose;
use rapier3d::parry::query::{DefaultQueryDispatcher, Ray, ShapeCastOptions};
use rapier3d::parry::shape::{Ball, Cuboid};
use rapier3d::pipeline::{QueryFilter as RapierFilter, QueryPipeline};

use crate::PhysicsWorld;

pub(crate) struct Snapshot {
    broad_phase: BroadPhaseBvh,
    colliders: ColliderSet,
    /// Empty: the filters here never look at bodies.
    bodies: RigidBodySet,
}

impl Snapshot {
    pub(crate) fn of(world: &PhysicsWorld) -> Self {
        Self {
            broad_phase: world.broad_phase.clone(),
            colliders: world.colliders.clone(),
            bodies: RigidBodySet::new(),
        }
    }

    fn pipeline<'a>(&'a self, filter: RapierFilter<'a>) -> QueryPipeline<'a> {
        static DISPATCHER: DefaultQueryDispatcher = DefaultQueryDispatcher;
        self.broad_phase
            .as_query_pipeline(&DISPATCHER, &self.bodies, &self.colliders, filter)
    }
}

fn entity_of(c: &Collider) -> Option<Entity> {
    Entity::try_from_bits(c.user_data as u64)
}

/// Layer, sensor and exclusion rules of a `QueryFilter`.
fn accepts(filter: &QueryFilter, c: &Collider) -> bool {
    (filter.sensors || !c.is_sensor())
        && c.collision_groups().memberships.bits() & filter.layers != 0
        && filter.exclude.is_none_or(|e| entity_of(c) != Some(e))
}

impl QueryBackend for Snapshot {
    fn raycast_all(&self, origin: Vec3, dir: Vec3, max: f32, filter: &QueryFilter) -> Vec<RayHit> {
        let Some(dir) = dir.try_normalize() else {
            return Vec::new();
        };
        let pred = |_: ColliderHandle, c: &Collider| accepts(filter, c);
        let pipeline = self.pipeline(RapierFilter::new().predicate(&pred));
        let ray = Ray::new(origin, dir);
        let mut hits: Vec<RayHit> = pipeline
            .intersect_ray(ray, max, true)
            .filter_map(|(_, c, hit)| {
                Some(RayHit {
                    entity: entity_of(c)?,
                    point: origin + dir * hit.time_of_impact,
                    normal: hit.normal,
                    distance: hit.time_of_impact,
                })
            })
            .collect();
        hits.sort_by(|a, b| a.distance.total_cmp(&b.distance));
        hits
    }

    fn sphere_cast(
        &self,
        origin: Vec3,
        radius: f32,
        dir: Vec3,
        max: f32,
        filter: &QueryFilter,
    ) -> Option<RayHit> {
        let dir = dir.try_normalize()?;
        let pred = |_: ColliderHandle, c: &Collider| accepts(filter, c);
        let pipeline = self.pipeline(RapierFilter::new().predicate(&pred));
        let (handle, hit) = pipeline.cast_shape(
            &Pose::from_translation(origin),
            dir,
            &Ball::new(radius),
            ShapeCastOptions::with_max_time_of_impact(max),
        )?;
        Some(RayHit {
            entity: entity_of(self.colliders.get(handle)?)?,
            point: hit.witness1,
            normal: hit.normal1,
            distance: hit.time_of_impact,
        })
    }

    fn overlap_sphere(&self, center: Vec3, radius: f32, filter: &QueryFilter) -> Vec<Entity> {
        self.overlap(Pose::from_translation(center), &Ball::new(radius), filter)
    }

    fn overlap_box(
        &self,
        center: Vec3,
        half_extents: Vec3,
        rotation: Quat,
        filter: &QueryFilter,
    ) -> Vec<Entity> {
        self.overlap(
            Pose::from_parts(center, rotation),
            &Cuboid::new(half_extents),
            filter,
        )
    }
}

impl Snapshot {
    fn overlap(
        &self,
        pose: Pose,
        shape: &dyn rapier3d::parry::shape::Shape,
        filter: &QueryFilter,
    ) -> Vec<Entity> {
        let pred = |_: ColliderHandle, c: &Collider| accepts(filter, c);
        let pipeline = self.pipeline(RapierFilter::new().predicate(&pred));
        pipeline
            .intersect_shape(pose, shape)
            .filter_map(|(_, c)| entity_of(c))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapier3d::dynamics::RigidBodyBuilder;
    use rapier3d::geometry::ColliderBuilder;
    use std::time::Instant;

    /// Prints the per-step cost of the snapshot copy with 1,000 bodies.
    #[test]
    fn snapshot_copy_of_a_thousand_bodies_is_cheap() {
        let mut world = PhysicsWorld::new();
        for i in 0..1000 {
            let p = Vec3::new(
                (i % 10) as f32 * 2.0,
                (i / 100) as f32 * 2.0,
                (i / 10 % 10) as f32 * 2.0,
            );
            let body = world
                .bodies
                .insert(RigidBodyBuilder::dynamic().translation(p).build());
            world.colliders.insert_with_parent(
                ColliderBuilder::cuboid(0.5, 0.5, 0.5).build(),
                body,
                &mut world.bodies,
            );
        }
        world.step();
        let n = 50;
        let start = Instant::now();
        for _ in 0..n {
            std::hint::black_box(Snapshot::of(&world));
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / n as f64;
        println!("snapshot copy, 1000 bodies: {ms:.3} ms per step");
        assert!(ms < 16.0, "{ms} ms is a whole frame");
    }
}
