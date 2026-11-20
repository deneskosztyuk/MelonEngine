//! Scene save / load lives in `melon_core::scene` (a game without the editor
//! loads scenes too). The editor owns the `PhysicsWorld`, so its apply / load
//! also reset and re-sync the bodies.
use std::path::Path;

use anyhow::Result;
use bevy_ecs::world::World;
pub use melon_core::scene::*;
use melon_physics::PhysicsWorld;

/// `melon_core::scene::apply_scene` plus the physics bodies of the new scene.
pub fn apply_scene(world: &mut World, physics: &mut PhysicsWorld, data: &SceneData) -> Result<()> {
    melon_core::scene::apply_scene(world, data)?;
    physics.clear();
    melon_physics::sync_components(world, physics);
    Ok(())
}

/// Load a scene from a RON file (see `apply_scene`).
pub fn load_scene(world: &mut World, physics: &mut PhysicsWorld, path: &Path) -> Result<()> {
    let data = read_scene(path)?;
    apply_scene(world, physics, &data)?;
    tracing::info!(
        "loaded scene from {}: {} entities",
        path.display(),
        data.entities.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::entity::Entity;
    use melon_core::bevy_reflect::TypeRegistry;
    use melon_core::glam::{Quat, Vec3};
    use melon_core::scene::{BODY_INFO_PATH, component_ron};
    use melon_core::{
        Aabb, AssetServer, Bob, BodyInfo, BodyKind, ChildOf, Collider, ColliderShape, Handle,
        Model, Name, NameCounter, RigidBody, RigidBodyKind, Spin, Transform,
    };
    use melon_physics::RigidBodyComponent;
    use std::collections::BTreeMap;

    fn model_path_of(world: &mut World, name: &str) -> String {
        let handle = *world
            .query::<(&Name, &melon_core::Handle<melon_core::Model>)>()
            .iter(world)
            .find(|(n, _)| n.0 == name)
            .map(|(_, h)| h)
            .unwrap();
        world
            .resource::<melon_core::AssetServer>()
            .path(handle)
            .to_string()
    }
    fn named(world: &mut World, name: &str) -> Entity {
        world
            .query::<(Entity, &Name)>()
            .iter(world)
            .find(|(_, n)| n.0 == name)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no entity named {name}"))
    }
    fn entity(name: &str, parent: Option<u32>) -> SceneEntity {
        SceneEntity {
            name: name.to_string(),
            transform: Some(Transform::default()),
            parent,
            mesh_ref: None,
            components: BTreeMap::new(),
            model: None,
            model_guid: None,
            uid: None,
            aabb: None,
            body_info: None,
            spin: None,
            bob: None,
        }
    }
    #[test]
    fn child_body_spawns_at_world_pose() {
        let mut parent = entity("Parent", None);
        parent.transform = Some(Transform {
            translation: Vec3::new(5.0, 0.0, 0.0),
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            ..Default::default()
        });
        let mut child = entity("Child", Some(0));
        child.transform = Some(Transform::from_translation(Vec3::X));
        child.body_info = Some(BodyInfo {
            body_type: BodyKind::Fixed,
            half_extents: Vec3::splat(0.5),
        });
        let data = SceneData {
            version: SCENE_VERSION,
            entities: vec![parent, child],
            name_counter: 0,
        };
        let mut world = reg_world();
        let mut physics = PhysicsWorld::new();
        apply_scene(&mut world, &mut physics, &data).unwrap();
        let e = named(&mut world, "Child");
        let h = world.get::<RigidBodyComponent>(e).unwrap().body_handle;
        let body = physics.bodies.get(h).unwrap();
        assert!((body.translation() - Vec3::new(5.0, 0.0, -1.0)).length() < 1e-4);
        assert!(
            body.rotation()
                .angle_between(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2))
                < 1e-4
        );
    }
    /// Fixture: `save_scene` output (ron 0.8 pretty) of a version 3 scene.
    const V3_FIXTURE: &str = r#"(
    version: 3,
    entities: [
        (
            name: "Floor",
            transform: (
                translation: (0.0, -1.0, 0.0),
                rotation: (0.0, 0.0, 0.0, 1.0),
                scale: (1.0, 1.0, 1.0),
            ),
            parent: None,
            model: Some("builtin:cube"),
            aabb: Some((
                half_extents: (10.0, 0.5, 10.0),
            )),
            body_info: Some((
                body_type: Fixed,
                half_extents: (10.0, 0.5, 10.0),
            )),
            spin: None,
            bob: None,
        ),
        (
            name: "Cube",
            transform: (
                translation: (0.0, 2.0, 0.0),
                rotation: (0.0, 0.0, 0.0, 1.0),
                scale: (1.0, 1.0, 1.0),
            ),
            parent: Some(0),
            model: Some("assets/models/cube.glb"),
            aabb: None,
            body_info: None,
            spin: Some(()),
            bob: Some(()),
        ),
    ],
    name_counter: 3,
)"#;
    /// Fixture: `save_scene` output (ron 0.8 pretty) of a version 4 scene.
    const V4_FIXTURE: &str = r#"(
    version: 4,
    entities: [
        (
            name: "Floor",
            transform: (
                translation: (0.0, -1.0, 0.0),
                rotation: (0.0, 0.0, 0.0, 1.0),
                scale: (1.0, 1.0, 1.0),
            ),
            parent: None,
            model: Some("builtin:cube"),
            model_guid: None,
            aabb: Some((
                half_extents: (10.0, 0.5, 10.0),
            )),
            body_info: Some((
                body_type: Fixed,
                half_extents: (10.0, 0.5, 10.0),
            )),
            spin: None,
            bob: None,
        ),
        (
            name: "Cube",
            transform: (
                translation: (0.0, 2.0, 0.0),
                rotation: (0.0, 0.0, 0.0, 1.0),
                scale: (1.0, 1.0, 1.0),
            ),
            parent: Some(0),
            model: Some("assets/models/cube.glb"),
            model_guid: Some("334087e7-ba37-4ff4-b11f-bc731349dbfa"),
            aabb: None,
            body_info: None,
            spin: Some(()),
            bob: Some(()),
        ),
    ],
    name_counter: 3,
)"#;
    /// A world with the engine's component registry (as `Core` builds it).
    fn reg_world() -> World {
        melon_core::Core::new().world
    }
    /// Save to RON text, parse it back and apply it to the same world.
    fn round_trip(world: &mut World, physics: &mut PhysicsWorld) -> String {
        let data = scene_to_data(world).unwrap();
        let text = ron::ser::to_string_pretty(&data, ron::ser::PrettyConfig::default()).unwrap();
        let back: SceneData = ron::from_str(&text).unwrap();
        apply_scene(world, physics, &back).unwrap();
        text
    }
    #[test]
    fn v6_round_trips_every_engine_component() {
        let mut world = reg_world();
        let cube: Handle<Model> = world.resource_mut::<AssetServer>().load("builtin:cube");
        let rotation = Quat::from_rotation_y(0.5);
        let root = world
            .spawn((
                Name("Root".to_string()),
                Transform {
                    translation: Vec3::new(1.0, 2.0, 3.0),
                    rotation,
                    scale: Vec3::new(2.0, 1.0, 0.5),
                },
                cube,
                Aabb {
                    half_extents: Vec3::new(0.5, 0.25, 1.0),
                },
                RigidBody {
                    kind: RigidBodyKind::Fixed,
                    ..Default::default()
                },
                Collider::default(),
                Spin,
                Bob,
            ))
            .id();
        world.spawn((
            Name("Kid".to_string()),
            Transform::from_translation(Vec3::X),
            ChildOf(root),
        ));
        let mut physics = PhysicsWorld::new();
        let text = round_trip(&mut world, &mut physics);
        assert!(text.contains("version: 6"), "{text}");

        let root = named(&mut world, "Root");
        let t = world.get::<Transform>(root).unwrap();
        assert_eq!(t.translation, Vec3::new(1.0, 2.0, 3.0));
        assert!(t.rotation.angle_between(rotation) < 1e-6);
        assert_eq!(t.scale, Vec3::new(2.0, 1.0, 0.5));
        assert_eq!(
            world.get::<Aabb>(root).unwrap().half_extents,
            Vec3::new(0.5, 0.25, 1.0)
        );
        assert_eq!(
            world.get::<RigidBody>(root).unwrap().kind,
            RigidBodyKind::Fixed
        );
        assert_eq!(
            world.get::<Collider>(root).unwrap().shape,
            ColliderShape::Box {
                half_extents: Vec3::splat(0.5)
            }
        );
        assert!(world.get::<Spin>(root).is_some() && world.get::<Bob>(root).is_some());
        assert!(world.get::<RigidBodyComponent>(root).is_some());
        assert_eq!(physics.bodies.len(), 1);
        assert_eq!(model_path_of(&mut world, "Root"), "builtin:cube");
        let kid = named(&mut world, "Kid");
        assert_eq!(world.get::<ChildOf>(kid).unwrap().parent(), root);
        assert_eq!(world.get::<Transform>(kid).unwrap().translation, Vec3::X);
        assert!(world.get::<Aabb>(kid).is_none());
    }
    #[test]
    fn v6_file_has_no_runtime_components() {
        let mut world = reg_world();
        let root = world
            .spawn((
                Name("Root".to_string()),
                Transform::default(),
                melon_core::PreviousTransform(Transform::default()),
                melon_core::GlobalTransform(melon_core::glam::Mat4::IDENTITY),
                RigidBody::default(),
                Collider::default(),
            ))
            .id();
        world.spawn((Name("Kid".to_string()), Transform::default(), ChildOf(root)));
        let mut physics = PhysicsWorld::new();
        round_trip(&mut world, &mut physics); // adds rapier components
        let text = round_trip(&mut world, &mut physics);
        for runtime in [
            "PreviousTransform",
            "PendingDelete",
            "GlobalTransform",
            "ChildOf",
            "Children",
            "RigidBodyComponent",
            "ColliderComponent",
            "Name\"",
            "Handle",
        ] {
            assert!(!text.contains(runtime), "{runtime} saved:\n{text}");
        }
        assert!(text.contains("melon_core::physics::RigidBody"), "{text}");
        assert!(!text.contains("BodyInfo"), "{text}");
    }
    #[test]
    fn v1_to_v4_fixtures_still_load() {
        let v1 = r#"(version: 1, entities: [
            (name: "Old", transform: (translation: (0.0, 5.0, 0.0), rotation: (0.0, 0.0, 0.0, 1.0), scale: (1.0, 1.0, 1.0)),
             mesh_ref: Some(Cube), aabb: Some((half_extents: (0.5, 0.5, 0.5))),
             body_info: Some((body_type: Dynamic, half_extents: (0.5, 0.5, 0.5)))),
        ], name_counter: 1)"#;
        let mut world = reg_world();
        let mut physics = PhysicsWorld::new();
        let data: SceneData = ron::from_str(v1).unwrap();
        apply_scene(&mut world, &mut physics, &data).unwrap();
        let old = named(&mut world, "Old");
        assert_eq!(world.get::<Transform>(old).unwrap().translation.y, 5.0);
        assert_eq!(
            world.get::<Aabb>(old).unwrap().half_extents,
            Vec3::splat(0.5)
        );
        assert!(world.get::<RigidBodyComponent>(old).is_some());
        assert_eq!(physics.bodies.len(), 1);
        assert_eq!(model_path_of(&mut world, "Old"), "builtin:cube");

        for fixture in [V3_FIXTURE, V4_FIXTURE] {
            let mut world = reg_world();
            let mut physics = PhysicsWorld::new();
            let data: SceneData = ron::from_str(fixture).unwrap();
            apply_scene(&mut world, &mut physics, &data).unwrap();
            let floor = named(&mut world, "Floor");
            assert_eq!(
                world.get::<Transform>(floor).unwrap().translation,
                Vec3::new(0.0, -1.0, 0.0)
            );
            assert_eq!(
                world.get::<Aabb>(floor).unwrap().half_extents,
                Vec3::new(10.0, 0.5, 10.0)
            );
            assert_eq!(
                world.get::<RigidBody>(floor).unwrap().kind,
                RigidBodyKind::Fixed
            );
            assert_eq!(
                world.get::<Collider>(floor).unwrap().shape,
                ColliderShape::Box {
                    half_extents: Vec3::new(10.0, 0.5, 10.0)
                }
            );
            assert_eq!(physics.bodies.len(), 1);
            let cube = named(&mut world, "Cube");
            assert_eq!(world.get::<ChildOf>(cube).unwrap().parent(), floor);
            assert_eq!(
                world.get::<Transform>(cube).unwrap().translation,
                Vec3::new(0.0, 2.0, 0.0)
            );
            assert!(world.get::<Spin>(cube).is_some() && world.get::<Bob>(cube).is_some());
            assert!(world.get::<Aabb>(cube).is_none());
            assert_eq!(model_path_of(&mut world, "Cube"), "assets/models/cube.glb");
            // Saved again as v6.
            let text = round_trip(&mut world, &mut physics);
            assert!(text.contains("version: 6"), "{text}");
        }
    }
    #[test]
    fn v5_body_info_loads_as_a_v6_body_and_saves_as_v6() {
        let mut local = TypeRegistry::default();
        local.register::<BodyInfo>();
        let info = BodyInfo {
            body_type: BodyKind::Dynamic,
            half_extents: Vec3::new(1.0, 2.0, 3.0),
        };
        let mut old = entity("Old", None);
        old.components.insert(
            BODY_INFO_PATH.to_string(),
            component_ron(&info, &local).unwrap(),
        );
        old.transform = Some(Transform {
            scale: Vec3::splat(2.0),
            ..Default::default()
        });
        let data = SceneData {
            version: 5,
            entities: vec![old],
            name_counter: 0,
        };
        let mut world = reg_world();
        let mut physics = PhysicsWorld::new();
        apply_scene(&mut world, &mut physics, &data).unwrap();
        let e = named(&mut world, "Old");
        assert_eq!(
            world.get::<RigidBody>(e).unwrap().kind,
            RigidBodyKind::Dynamic
        );
        // World-unit half extents become local ones under the entity scale.
        assert_eq!(
            world.get::<Collider>(e).unwrap().shape,
            ColliderShape::Box {
                half_extents: Vec3::new(0.5, 1.0, 1.5)
            }
        );
        assert_eq!((physics.bodies.len(), physics.colliders().len()), (1, 1));
        assert!(world.get::<RigidBodyComponent>(e).is_some());
        let text = round_trip(&mut world, &mut physics);
        assert!(text.contains("version: 6"), "{text}");
        assert!(!text.contains("BodyInfo"), "{text}");
        let e = named(&mut world, "Old");
        assert_eq!(
            world.get::<RigidBody>(e).unwrap().kind,
            RigidBodyKind::Dynamic
        );
        assert_eq!((physics.bodies.len(), physics.colliders().len()), (1, 1));
    }
    #[test]
    fn v5_scaled_entity_keeps_its_world_size_in_rapier() {
        let mut local = TypeRegistry::default();
        local.register::<BodyInfo>();
        let info = BodyInfo {
            body_type: BodyKind::Fixed,
            half_extents: Vec3::new(1.0, 2.0, 3.0),
        };
        let mut old = entity("Old", None);
        old.components.insert(
            BODY_INFO_PATH.to_string(),
            component_ron(&info, &local).unwrap(),
        );
        old.transform = Some(Transform {
            scale: Vec3::new(2.0, 0.5, 4.0),
            ..Default::default()
        });
        let data = SceneData {
            version: 5,
            entities: vec![old],
            name_counter: 0,
        };
        let mut world = reg_world();
        let mut physics = PhysicsWorld::new();
        apply_scene(&mut world, &mut physics, &data).unwrap();
        let e = named(&mut world, "Old");
        let handle = world
            .get::<melon_physics::ColliderComponent>(e)
            .unwrap()
            .collider_handle
            .unwrap();
        let half = physics
            .colliders()
            .get(handle)
            .unwrap()
            .shape()
            .as_cuboid()
            .unwrap()
            .half_extents;
        assert!(
            (half - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-5,
            "world half extents {half}"
        );
        // Saved and loaded as v6: still the same world size.
        let text = round_trip(&mut world, &mut physics);
        assert!(text.contains("version: 6"), "{text}");
        let e = named(&mut world, "Old");
        let handle = world
            .get::<melon_physics::ColliderComponent>(e)
            .unwrap()
            .collider_handle
            .unwrap();
        let half = physics
            .colliders()
            .get(handle)
            .unwrap()
            .shape()
            .as_cuboid()
            .unwrap()
            .half_extents;
        assert!((half - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-5, "{half}");
    }
    #[test]
    fn v5_body_info_keeps_its_world_size_under_a_scaled_parent() {
        let mut local = TypeRegistry::default();
        local.register::<BodyInfo>();
        let info = BodyInfo {
            body_type: BodyKind::Fixed,
            half_extents: Vec3::new(1.0, 2.0, 3.0),
        };
        let mut parent = entity("Parent", None);
        parent.transform = Some(Transform {
            scale: Vec3::splat(2.0),
            ..Default::default()
        });
        let mut child = entity("Child", Some(0));
        child.transform = Some(Transform {
            scale: Vec3::new(1.0, 0.5, 1.0),
            ..Default::default()
        });
        child.components.insert(
            BODY_INFO_PATH.to_string(),
            component_ron(&info, &local).unwrap(),
        );
        let data = SceneData {
            version: 5,
            entities: vec![parent, child],
            name_counter: 0,
        };
        let mut world = reg_world();
        let mut physics = PhysicsWorld::new();
        apply_scene(&mut world, &mut physics, &data).unwrap();
        let e = named(&mut world, "Child");
        // World scale (2, 1, 2): local half = world half / scale.
        let ColliderShape::Box { half_extents } = world.get::<Collider>(e).unwrap().shape else {
            panic!("a box");
        };
        assert!(
            (half_extents - Vec3::new(0.5, 2.0, 1.5)).length() < 1e-5,
            "{half_extents}"
        );
    }
    #[test]
    fn body_info_of_kind_none_is_no_body() {
        let mut local = TypeRegistry::default();
        local.register::<BodyInfo>();
        let info = BodyInfo {
            body_type: BodyKind::None,
            half_extents: Vec3::splat(0.5),
        };
        let mut old = entity("Old", None);
        old.components.insert(
            BODY_INFO_PATH.to_string(),
            component_ron(&info, &local).unwrap(),
        );
        let data = SceneData {
            version: 5,
            entities: vec![old],
            name_counter: 0,
        };
        let mut world = reg_world();
        let mut physics = PhysicsWorld::new();
        apply_scene(&mut world, &mut physics, &data).unwrap();
        assert_eq!(physics.bodies.len(), 0);
        let e = named(&mut world, "Old");
        assert!(world.get::<RigidBody>(e).is_none());
    }
    #[test]
    fn payload_enums_of_physics_components_round_trip() {
        use melon_core::{Joint, JointKind};
        let mut world = reg_world();
        let collider = Collider {
            shape: ColliderShape::Capsule {
                half_height: 0.7,
                radius: 0.2,
            },
            ..Default::default()
        };
        let joint = Joint {
            kind: JointKind::Hinge {
                axis: Vec3::Z,
                limited: true,
                min: -1.0,
                max: 2.0,
            },
            anchor: Vec3::X,
            ..Default::default()
        };
        world.spawn((
            Name("P".to_string()),
            Transform::default(),
            RigidBody::default(),
            collider.clone(),
            joint.clone(),
        ));
        let text = round_trip(&mut world, &mut PhysicsWorld::new());
        assert!(text.contains("version: 6"), "{text}");
        let e = named(&mut world, "P");
        assert_eq!(world.get::<Collider>(e), Some(&collider));
        assert_eq!(world.get::<Joint>(e), Some(&joint));
    }
    #[test]
    fn load_creates_physics_bodies() {
        let ron = r#"(
            version: 1,
            entities: [
                (
                    name: "DynamicCube",
                    transform: (
                        translation: (0.0, 5.0, 0.0),
                        rotation: (0.0, 0.0, 0.0, 1.0),
                        scale: (1.0, 1.0, 1.0),
                    ),
                    aabb: Some((half_extents: (0.5, 0.5, 0.5))),
                    body_info: Some((
                        body_type: Dynamic,
                        half_extents: (0.5, 0.5, 0.5),
                    )),
                ),
            ],
            name_counter: 1,
        )"#;

        let tmp = std::env::temp_dir().join("melon_test_physics.ron");
        std::fs::write(&tmp, ron).unwrap();

        let mut world = reg_world();
        world.insert_resource(NameCounter(0));
        let mut physics = PhysicsWorld::new();

        load_scene(&mut world, &mut physics, &tmp).unwrap();

        assert_eq!(physics.bodies.len(), 1, "should have 1 physics body");
        assert_eq!(physics.colliders().len(), 1, "should have 1 collider");

        let has_rb = world.query::<&RigidBodyComponent>().iter(&world).count();
        assert_eq!(has_rb, 1, "entity should have RigidBodyComponent");

        let _ = std::fs::remove_file(&tmp);
    }
}
