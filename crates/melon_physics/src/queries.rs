//! Scene queries for game code. `Shared` reads the colliders and broad phase
//! the `PhysicsWorld` steps (behind a lock, no copy), so game systems call
//! `PhysicsQueries` synchronously without borrowing the `PhysicsWorld`. A
//! collider shows up once a step has put it into the broad phase.

use std::sync::{Arc, RwLock};

use bevy_ecs::entity::Entity;
use glam::{Quat, Vec3};
use melon_core::{QueryBackend, QueryFilter, RayHit};
use rapier3d::dynamics::RigidBodySet;
use rapier3d::geometry::{Collider, ColliderHandle};
use rapier3d::math::Pose;
use rapier3d::parry::query::{DefaultQueryDispatcher, Ray, ShapeCastOptions};
use rapier3d::parry::shape::{Ball, Cuboid};
use rapier3d::pipeline::{QueryFilter as RapierFilter, QueryPipeline};

use crate::Scene;

pub(crate) struct Shared(pub(crate) Arc<RwLock<Scene>>);

/// Run `f` with a query pipeline over the scene (no body filters: they are not used).
fn with_pipeline<T>(
    scene: &RwLock<Scene>,
    filter: &QueryFilter,
    f: impl FnOnce(&QueryPipeline, &Scene) -> T,
) -> T {
    static DISPATCHER: DefaultQueryDispatcher = DefaultQueryDispatcher;
    let scene = scene.read().unwrap();
    let bodies = RigidBodySet::new();
    let pred = |_: ColliderHandle, c: &Collider| accepts(filter, c);
    let pipeline = scene.broad_phase.as_query_pipeline(
        &DISPATCHER,
        &bodies,
        &scene.colliders,
        RapierFilter::new().predicate(&pred),
    );
    f(&pipeline, &scene)
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

impl QueryBackend for Shared {
    fn raycast_all(&self, origin: Vec3, dir: Vec3, max: f32, filter: &QueryFilter) -> Vec<RayHit> {
        let Some(dir) = dir.try_normalize() else {
            return Vec::new();
        };
        with_pipeline(&self.0, filter, |pipeline, _| {
            let ray = Ray::new(origin, dir);
            let mut hits: Vec<RayHit> = pipeline
                .intersect_ray(ray, max, true)
                // A ray that starts inside reports the collider at distance 0.
                .filter(|(_, _, hit)| filter.hit_inside || hit.time_of_impact > 1e-6)
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
        })
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
        with_pipeline(&self.0, filter, |pipeline, scene| {
            let (handle, hit) = pipeline.cast_shape(
                &Pose::from_translation(origin),
                dir,
                &Ball::new(radius),
                ShapeCastOptions::with_max_time_of_impact(max),
            )?;
            Some(RayHit {
                entity: entity_of(scene.colliders.get(handle)?)?,
                point: hit.witness1,
                normal: hit.normal1,
                distance: hit.time_of_impact,
            })
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

impl Shared {
    fn overlap(
        &self,
        pose: Pose,
        shape: &dyn rapier3d::parry::shape::Shape,
        filter: &QueryFilter,
    ) -> Vec<Entity> {
        with_pipeline(&self.0, filter, |pipeline, _| {
            pipeline
                .intersect_shape(pose, shape)
                .filter_map(|(_, c)| entity_of(c))
                .collect()
        })
    }
}
