//! Physics data and the game-facing API: components the Inspector and scene
//! files edit, ray / shape queries and collision events. `melon_physics`
//! turns the components into rapier objects and fills `PhysicsQueries`.

use std::sync::Arc;

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::message::Message;
use bevy_ecs::reflect::ReflectComponent;
use bevy_ecs::resource::Resource;
use bevy_reflect::{Reflect, std_traits::ReflectDefault};
use glam::{Quat, Vec3};

use crate::reflect::{ReflectVariantDefaults, VariantDefaults, Vec3Reflect};

#[derive(Reflect, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[reflect(Default)]
pub enum RigidBodyKind {
    /// Moved by physics.
    #[default]
    Dynamic,
    /// Never moves by itself; follows its `Transform`.
    Fixed,
    /// Follows its `Transform` and pushes dynamic bodies.
    Kinematic,
}

/// A rigid body. A `Collider` without one is static.
#[derive(Component, Reflect, Debug, Clone, PartialEq)]
#[reflect(Component, Default)]
pub struct RigidBody {
    pub kind: RigidBodyKind,
    /// Total mass in kg; 0 takes it from the colliders' volume.
    pub mass: f32,
    pub linear_damping: f32,
    pub angular_damping: f32,
    pub gravity_scale: f32,
    pub lock_rotation_x: bool,
    pub lock_rotation_y: bool,
    pub lock_rotation_z: bool,
    /// Continuous collision detection (fast bodies do not tunnel).
    pub ccd: bool,
}

impl Default for RigidBody {
    fn default() -> Self {
        Self {
            kind: RigidBodyKind::Dynamic,
            mass: 0.0,
            linear_damping: 0.0,
            angular_damping: 0.0,
            gravity_scale: 1.0,
            lock_rotation_x: false,
            lock_rotation_y: false,
            lock_rotation_z: false,
            ccd: false,
        }
    }
}

#[derive(Reflect, Debug, Clone, Copy, PartialEq)]
#[reflect(Default, VariantDefaults)]
pub enum ColliderShape {
    /// Scaled by the entity's world scale.
    Box {
        #[reflect(remote = Vec3Reflect)]
        half_extents: Vec3,
    },
    Sphere {
        radius: f32,
    },
    /// `half_height` of the straight part along Y.
    Capsule {
        half_height: f32,
        radius: f32,
    },
    /// Triangle mesh of the entity's model (`x.glb#meshN` included).
    Mesh,
}

impl Default for ColliderShape {
    fn default() -> Self {
        Self::Box {
            half_extents: Vec3::splat(0.5),
        }
    }
}

impl VariantDefaults for ColliderShape {
    fn variant_default(name: &str) -> Option<Self> {
        Some(match name {
            "Box" => Self::default(),
            "Sphere" => Self::Sphere { radius: 0.5 },
            "Capsule" => Self::Capsule {
                half_height: 0.5,
                radius: 0.25,
            },
            "Mesh" => Self::Mesh,
            _ => return None,
        })
    }
}

/// A collider on the entity. With a `RigidBody` on the entity or on its
/// nearest ancestor it is part of that body; else it is static.
#[derive(Component, Reflect, Debug, Clone, PartialEq)]
#[reflect(Component, Default)]
pub struct Collider {
    pub shape: ColliderShape,
    pub friction: f32,
    pub restitution: f32,
    /// A trigger: reports overlaps, never pushes.
    pub sensor: bool,
    /// Layers this collider is on (bit mask).
    pub membership: u32,
    /// Layers it collides with (bit mask).
    pub filter: u32,
}

impl Default for Collider {
    fn default() -> Self {
        Self {
            shape: ColliderShape::default(),
            friction: 0.5,
            restitution: 0.0,
            sensor: false,
            membership: u32::MAX,
            filter: u32::MAX,
        }
    }
}

/// A walking character (rapier's kinematic character controller). Needs a
/// `Collider` on the same entity. Call `move_by` each fixed step; the
/// physics step moves the `Transform` and fills `grounded` and `applied`.
#[derive(Component, Reflect, Debug, Clone, PartialEq)]
#[reflect(Component, Default)]
pub struct CharacterController {
    /// Gap kept between the collider and what it touches (metres).
    pub offset: f32,
    /// Steepest slope that can be climbed, in degrees.
    pub max_slope: f32,
    /// Highest step that is walked over automatically (metres, 0 = off).
    pub max_step_height: f32,
    /// Distance the character is pulled down to stay on the ground (metres, 0 = off).
    pub snap_to_ground: f32,
    #[reflect(ignore)]
    /// The pending move (set it with `move_by`).
    pub desired: Vec3,
    /// On the ground after the last move.
    #[reflect(ignore)]
    pub grounded: bool,
    /// World-space translation the last move applied.
    #[reflect(ignore)]
    pub applied: Vec3,
}

impl Default for CharacterController {
    fn default() -> Self {
        Self {
            offset: 0.01,
            max_slope: 45.0,
            max_step_height: 0.3,
            snap_to_ground: 0.2,
            desired: Vec3::ZERO,
            grounded: false,
            applied: Vec3::ZERO,
        }
    }
}

impl CharacterController {
    /// Ask for a world-space move in the next fixed step (replaces an
    /// earlier request). Include gravity in `delta` yourself.
    pub fn move_by(&mut self, delta: Vec3) {
        self.desired = delta;
    }

    /// The pending request.
    pub fn desired(&self) -> Vec3 {
        self.desired
    }

    /// Clear the request (the physics step does this after moving).
    pub fn take_desired(&mut self) -> Vec3 {
        std::mem::take(&mut self.desired)
    }
}

#[derive(Reflect, Debug, Clone, Copy, PartialEq, Default)]
#[reflect(Default, VariantDefaults)]
pub enum JointKind {
    /// No relative motion.
    #[default]
    Fixed,
    /// Rotates about `axis` (local to this body); with `limited`, the angle
    /// stays within `min..max` radians.
    Hinge {
        #[reflect(remote = Vec3Reflect)]
        axis: Vec3,
        limited: bool,
        min: f32,
        max: f32,
    },
    /// Rotates freely about the anchor; with `limited`, the Y and Z swing
    /// stays within `cone_limit` radians.
    Ball { limited: bool, cone_limit: f32 },
    /// Slides along `axis` (local to this body); with `limited`, the offset
    /// stays within `min..max` metres.
    Slider {
        #[reflect(remote = Vec3Reflect)]
        axis: Vec3,
        limited: bool,
        min: f32,
        max: f32,
    },
}

impl VariantDefaults for JointKind {
    fn variant_default(name: &str) -> Option<Self> {
        Some(match name {
            "Fixed" => Self::Fixed,
            "Hinge" => Self::Hinge {
                axis: Vec3::Y,
                limited: false,
                min: -std::f32::consts::FRAC_PI_2,
                max: std::f32::consts::FRAC_PI_2,
            },
            "Ball" => Self::Ball {
                limited: false,
                cone_limit: std::f32::consts::FRAC_PI_4,
            },
            "Slider" => Self::Slider {
                axis: Vec3::X,
                limited: false,
                min: -1.0,
                max: 1.0,
            },
            _ => return None,
        })
    }
}

/// Connects this entity's body to its nearest ancestor body (or the world
/// when it has none). Anchors are local to each body.
#[derive(Component, Reflect, Debug, Clone, PartialEq)]
#[reflect(Component, Default)]
pub struct Joint {
    pub kind: JointKind,
    #[reflect(remote = Vec3Reflect)]
    pub anchor: Vec3,
    /// Anchor in the ancestor body's space (world space with no ancestor body).
    #[reflect(remote = Vec3Reflect)]
    pub parent_anchor: Vec3,
    /// Keep the pair where it is when the joint is made: `anchor` is the pivot on
    /// this body, and the ancestor side is worked out from the current poses
    /// (`parent_anchor` is then ignored). Off: both anchors are used as given.
    pub auto_anchor: bool,
}
impl Default for Joint {
    fn default() -> Self {
        Self {
            kind: JointKind::Fixed,
            anchor: Vec3::ZERO,
            parent_anchor: Vec3::ZERO,
            auto_anchor: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionEventKind {
    /// Two solid colliders began touching.
    Started,
    /// Two solid colliders stopped touching.
    Ended,
    /// A collider entered a sensor.
    TriggerEntered,
    /// A collider left a sensor.
    TriggerExited,
}

/// Written during the physics step (read with `MessageReader` in
/// `FixedUpdate`, after the physics systems). `a` / `b` are in no
/// particular order; for triggers one of them is the sensor.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollisionEvent {
    pub a: Entity,
    pub b: Entity,
    pub kind: CollisionEventKind,
}

/// Which colliders a query sees.
#[derive(Debug, Clone, Copy)]
pub struct QueryFilter {
    /// Hits colliders whose `membership` shares a bit with this mask.
    pub layers: u32,
    /// An entity the query ignores (the caster itself).
    pub exclude: Option<Entity>,
    /// Include sensors (triggers).
    pub sensors: bool,
    /// Count a collider the ray starts inside as hit (at distance 0). Off by
    /// default, like Unity and Godot.
    pub hit_inside: bool,
}

impl Default for QueryFilter {
    fn default() -> Self {
        Self {
            layers: u32::MAX,
            exclude: None,
            sensors: false,
            hit_inside: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    pub entity: Entity,
    pub point: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

/// Implemented by `melon_physics`.
pub trait QueryBackend: Send + Sync {
    fn raycast_all(&self, origin: Vec3, dir: Vec3, max: f32, filter: &QueryFilter) -> Vec<RayHit>;
    fn sphere_cast(
        &self,
        origin: Vec3,
        radius: f32,
        dir: Vec3,
        max: f32,
        filter: &QueryFilter,
    ) -> Option<RayHit>;
    fn overlap_sphere(&self, center: Vec3, radius: f32, filter: &QueryFilter) -> Vec<Entity>;
    fn overlap_box(
        &self,
        center: Vec3,
        half_extents: Vec3,
        rotation: Quat,
        filter: &QueryFilter,
    ) -> Vec<Entity>;
}

/// Ray, shape-cast and overlap queries for game systems (`Res<PhysicsQueries>`).
/// They see the world as of the end of the last physics step; with no
/// physics plugin they find nothing.
#[derive(Resource, Default, Clone)]
pub struct PhysicsQueries {
    backend: Option<Arc<dyn QueryBackend>>,
}

impl PhysicsQueries {
    /// Used by the physics step.
    pub fn set_backend(&mut self, backend: Arc<dyn QueryBackend>) {
        self.backend = Some(backend);
    }

    /// First hit along the ray; `dir` need not be normalised.
    pub fn raycast(
        &self,
        origin: Vec3,
        dir: Vec3,
        max: f32,
        filter: &QueryFilter,
    ) -> Option<RayHit> {
        self.raycast_all(origin, dir, max, filter)
            .into_iter()
            .next()
    }

    /// Every hit along the ray, nearest first.
    pub fn raycast_all(
        &self,
        origin: Vec3,
        dir: Vec3,
        max: f32,
        filter: &QueryFilter,
    ) -> Vec<RayHit> {
        let Some(b) = &self.backend else {
            return Vec::new();
        };
        b.raycast_all(origin, dir, max, filter)
    }

    /// Sweep a sphere; `distance` of the hit is how far it travelled.
    pub fn sphere_cast(
        &self,
        origin: Vec3,
        radius: f32,
        dir: Vec3,
        max: f32,
        filter: &QueryFilter,
    ) -> Option<RayHit> {
        self.backend
            .as_ref()?
            .sphere_cast(origin, radius, dir, max, filter)
    }

    pub fn overlap_sphere(&self, center: Vec3, radius: f32, filter: &QueryFilter) -> Vec<Entity> {
        self.backend
            .as_ref()
            .map_or_else(Vec::new, |b| b.overlap_sphere(center, radius, filter))
    }

    pub fn overlap_box(
        &self,
        center: Vec3,
        half_extents: Vec3,
        rotation: Quat,
        filter: &QueryFilter,
    ) -> Vec<Entity> {
        self.backend.as_ref().map_or_else(Vec::new, |b| {
            b.overlap_box(center, half_extents, rotation, filter)
        })
    }
}

/// Linear and angular velocity (world space, m/s and rad/s) of a dynamic
/// body. Physics adds it to every dynamic body and writes it after each step;
/// a value the game writes (`velocity.linear = ..`) is applied to the body
/// before the next step.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct Velocity {
    pub linear: Vec3,
    pub angular: Vec3,
}

/// One request for a body, applied at the start of the next physics step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PhysicsRequest {
    /// Instant change of linear momentum (N s).
    Impulse(Entity, Vec3),
    /// Instant change of angular momentum.
    TorqueImpulse(Entity, Vec3),
    /// A force (N) for the next step only.
    Force(Entity, Vec3),
    /// A torque for the next step only.
    Torque(Entity, Vec3),
    /// Put the entity at a world pose, keeping its velocity. This is how
    /// game code moves a dynamic body: its `Transform` belongs to physics.
    Teleport(Entity, Vec3, Option<Quat>),
}

/// Requests from game code to physics (`ResMut<PhysicsRequests>`), drained
/// every fixed step. Requests for an entity without a body are dropped.
#[derive(Resource, Default, Debug)]
pub struct PhysicsRequests {
    queue: Vec<PhysicsRequest>,
}

impl PhysicsRequests {
    pub fn apply_impulse(&mut self, e: Entity, impulse: Vec3) {
        self.queue.push(PhysicsRequest::Impulse(e, impulse));
    }

    pub fn apply_torque_impulse(&mut self, e: Entity, impulse: Vec3) {
        self.queue.push(PhysicsRequest::TorqueImpulse(e, impulse));
    }

    pub fn apply_force(&mut self, e: Entity, force: Vec3) {
        self.queue.push(PhysicsRequest::Force(e, force));
    }

    pub fn apply_torque(&mut self, e: Entity, torque: Vec3) {
        self.queue.push(PhysicsRequest::Torque(e, torque));
    }

    /// Move the entity to `position` (and `rotation` when given), in world space.
    pub fn teleport(&mut self, e: Entity, position: Vec3, rotation: Option<Quat>) {
        self.queue
            .push(PhysicsRequest::Teleport(e, position, rotation));
    }

    /// Take every pending request (the physics step does this).
    pub fn drain(&mut self) -> Vec<PhysicsRequest> {
        std::mem::take(&mut self.queue)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use bevy_reflect::ReflectRef;

    #[test]
    fn queries_without_backend_find_nothing() {
        let q = PhysicsQueries::default();
        let f = QueryFilter::default();
        assert!(q.raycast(Vec3::ZERO, Vec3::X, 10.0, &f).is_none());
        assert!(q.overlap_sphere(Vec3::ZERO, 1.0, &f).is_empty());
    }

    #[test]
    fn character_move_request_is_taken_once() {
        let mut c = CharacterController::default();
        c.move_by(Vec3::X);
        assert_eq!(c.take_desired(), Vec3::X);
        assert_eq!(c.desired(), Vec3::ZERO);
    }

    #[test]
    fn every_collider_and_joint_variant_has_usable_defaults_in_the_registry() {
        use bevy_reflect::{TypeInfo, TypeRegistry, Typed};
        let mut registry = TypeRegistry::new();
        registry.register::<Collider>();
        registry.register::<Joint>();
        for (id, info) in [
            (
                std::any::TypeId::of::<ColliderShape>(),
                ColliderShape::type_info(),
            ),
            (std::any::TypeId::of::<JointKind>(), JointKind::type_info()),
        ] {
            let data = registry
                .get_type_data::<ReflectVariantDefaults>(id)
                .expect("registered with the type");
            let TypeInfo::Enum(e) = info else { panic!() };
            for name in e.variant_names() {
                let v = data.variant_default(name).expect(name);
                let ReflectRef::Enum(en) = v.as_partial_reflect().reflect_ref() else {
                    panic!()
                };
                assert_eq!(en.variant_name(), *name);
            }
        }
        // No zero-size shape comes out.
        for (name, ok) in [
            ("Box", true),
            ("Sphere", true),
            ("Capsule", true),
            ("Mesh", true),
        ] {
            let s = ColliderShape::variant_default(name).unwrap();
            let sized = match s {
                ColliderShape::Box { half_extents } => half_extents.min_element() > 0.0,
                ColliderShape::Sphere { radius } => radius > 0.0,
                ColliderShape::Capsule {
                    half_height,
                    radius,
                } => half_height > 0.0 && radius > 0.0,
                ColliderShape::Mesh => true,
            };
            assert_eq!(sized, ok, "{name}");
        }
        assert!(ColliderShape::variant_default("Nope").is_none());
    }
}
