use std::path::Path;

use anyhow::{Context, Result, bail};
use bevy_ecs::entity::Entity;
use bevy_ecs::query::With;
use bevy_ecs::world::World;
use melon_core::assets::split_mesh_fragment;
use melon_core::hierarchy::{depth_first, world_matrix};
use melon_core::{
    Aabb, AssetGuid, AssetServer, Bob, BodyInfo, BodyKind, ChildOf, Handle, Model, Name,
    NameCounter, Spin, Transform,
};
use melon_physics::{ColliderComponent, PhysicsWorld, RigidBodyComponent, RigidBodyType};
use serde::{Deserialize, Serialize};

/// 2: entities store a model path (`model`) instead of `mesh_ref`.
/// 3: entities store a `parent` index; `transform` is local to the parent.
/// 4: entities also store `model_guid`; it wins over `model` when it resolves.
const SCENE_VERSION: u32 = 4;

/// Version 1 `mesh_ref` values, read so old scenes still load.
#[derive(Deserialize, Clone, Copy)]
enum LegacyMeshRef {
    Cube,
    GltfModel,
}

impl LegacyMeshRef {
    fn model_path(self) -> &'static str {
        match self {
            LegacyMeshRef::Cube => "builtin:cube",
            LegacyMeshRef::GltfModel => "assets/models/cube.glb",
        }
    }
}

/// A whole scene in memory: what save files hold, and what Play snapshots.
#[derive(Serialize, Deserialize, Clone)]
pub struct SceneData {
    version: u32,
    entities: Vec<SceneEntity>,
    name_counter: u32,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SceneEntity {
    name: String,
    /// Local to the parent (world for roots).
    transform: Transform,
    /// Index of the parent in `entities` (version 3+).
    #[serde(default)]
    parent: Option<u32>,
    /// Version 1 only.
    #[serde(default, skip_serializing)]
    mesh_ref: Option<LegacyMeshRef>,
    /// Model asset path (project-relative, or `builtin:cube`).
    #[serde(default)]
    model: Option<String>,
    /// GUID of the model file (version 4+); `model` is the fallback.
    #[serde(default)]
    model_guid: Option<AssetGuid>,
    #[serde(default)]
    aabb: Option<Aabb>,
    #[serde(default)]
    body_info: Option<BodyInfo>,
    #[serde(default)]
    spin: Option<Spin>,
    #[serde(default)]
    bob: Option<Bob>,
}

/// Capture all entities with Name + Transform, except ones marked for deletion
/// (deleted this frame but not yet despawned). Depth-first, so parents come
/// before their children and sibling order is kept.
pub fn scene_to_data(world: &mut World) -> SceneData {
    let rows: Vec<Entity> = depth_first(world).into_iter().map(|(e, _)| e).collect();
    let world: &World = world;
    let index: std::collections::HashMap<Entity, u32> = rows
        .iter()
        .enumerate()
        .map(|(i, e)| (*e, i as u32))
        .collect();
    let server = world.get_resource::<AssetServer>();

    let entities: Vec<SceneEntity> = rows
        .iter()
        .map(|&e| SceneEntity {
            name: world
                .get::<Name>(e)
                .map(|n| n.0.clone())
                .unwrap_or_default(),
            transform: world.get::<Transform>(e).copied().unwrap_or_default(),
            parent: world
                .get::<ChildOf>(e)
                .and_then(|c| index.get(&c.parent()).copied()),
            mesh_ref: None,
            model: world
                .get::<Handle<Model>>(e)
                .zip(server)
                .map(|(h, s)| s.path(*h).to_string()),
            model_guid: world
                .get::<Handle<Model>>(e)
                .zip(server)
                .and_then(|(h, s)| s.guid_of(s.path(*h))),
            aabb: world.get::<Aabb>(e).copied(),
            body_info: world.get::<BodyInfo>(e).copied(),
            spin: world.get::<Spin>(e).copied(),
            bob: world.get::<Bob>(e).copied(),
        })
        .collect();

    let name_counter = world
        .get_resource::<NameCounter>()
        .map(|c| c.0)
        .unwrap_or(0);

    SceneData {
        version: SCENE_VERSION,
        entities,
        name_counter,
    }
}

/// Replace the scene in `world` with `data`.
/// Two-phase: validate all entities before mutating the world.
/// Clears all existing scene entities + physics bodies, then spawns from `data`.
pub fn apply_scene(world: &mut World, physics: &mut PhysicsWorld, data: &SceneData) -> Result<()> {
    // Phase 1: Validate (no world mutation)
    if !(1..=SCENE_VERSION).contains(&data.version) {
        bail!(
            "unsupported scene version: expected {}, got {}",
            SCENE_VERSION,
            data.version
        );
    }

    for (i, entity) in data.entities.iter().enumerate() {
        validate_entity(entity, i)?;
    }
    validate_parents(&data.entities)?;

    // Phase 2: Mutate world in place.
    // Despawn only scene entities (those with a Transform). In bevy_ecs 0.19
    // resources are entities too, so despawning everything would delete them.
    // Never swap in a new World: systems are bound to the World they first ran on.
    let scene_entities: Vec<Entity> = world
        .query_filtered::<Entity, With<Transform>>()
        .iter(world)
        .collect();
    for entity in scene_entities {
        // Children may already be gone with their parent (`linked_spawn`).
        if let Ok(e) = world.get_entity_mut(entity) {
            e.despawn();
        }
    }
    world.insert_resource(NameCounter(data.name_counter));

    physics.clear();

    // Spawn everything, attach parents, then create bodies from world poses
    // (which need the parents in place).
    let ids: Vec<Entity> = data
        .entities
        .iter()
        .map(|entity| spawn_scene_entity(world, entity))
        .collect();
    for (entity, &id) in data.entities.iter().zip(&ids) {
        if let Some(p) = entity.parent {
            world.entity_mut(id).insert(ChildOf(ids[p as usize]));
        }
    }
    for (entity, &id) in data.entities.iter().zip(&ids) {
        if let Some(body_info) = entity.body_info {
            spawn_body(world, physics, id, body_info);
        }
    }
    Ok(())
}

/// Every parent index points at another entity in the file, without cycles.
fn validate_parents(entities: &[SceneEntity]) -> Result<()> {
    for (i, entity) in entities.iter().enumerate() {
        let name = &entity.name;
        let mut current = i;
        for _ in 0..=entities.len() {
            let Some(p) = entities[current].parent else {
                break;
            };
            let p = p as usize;
            if p >= entities.len() {
                bail!("entity '{name}' (index {i}): parent index {p} is out of range");
            }
            if p == i {
                bail!("entity '{name}' (index {i}) is its own parent or ancestor");
            }
            current = p;
        }
        if entities[current].parent.is_some() {
            bail!("entity '{name}' (index {i}) is in a parent cycle");
        }
    }
    Ok(())
}

/// Save all entities with Name + Transform to a RON file.
/// Uses atomic write (temp file + rename) to prevent corruption.
pub fn save_scene(world: &mut World, path: &Path) -> Result<()> {
    let data = scene_to_data(world);

    let ron = ron::ser::to_string_pretty(&data, ron::ser::PrettyConfig::default())
        .context("failed to serialize scene to RON")?;

    // Atomic write: temp file + rename
    let tmp_path = path.with_extension("ron.tmp");
    std::fs::write(&tmp_path, &ron)
        .with_context(|| format!("failed to write temp file: {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to rename temp to: {}", path.display()))?;

    tracing::info!(
        "saved scene to {}: {} entities",
        path.display(),
        data.entities.len()
    );
    Ok(())
}

/// Load a scene from a RON file (see `apply_scene`).
pub fn load_scene(world: &mut World, physics: &mut PhysicsWorld, path: &Path) -> Result<()> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read scene file: {}", path.display()))?;

    let data: SceneData =
        ron::from_str(&contents).context("failed to deserialize scene from RON")?;

    apply_scene(world, physics, &data)?;

    tracing::info!(
        "loaded scene from {}: {} entities",
        path.display(),
        data.entities.len()
    );
    Ok(())
}

fn validate_entity(entity: &SceneEntity, index: usize) -> Result<()> {
    let name = &entity.name;

    // Validate scale is non-zero
    let scale = entity.transform.scale;
    if scale.x.abs() < 1e-6 || scale.y.abs() < 1e-6 || scale.z.abs() < 1e-6 {
        bail!("entity '{name}' (index {index}) has zero or near-zero scale: {scale:?}");
    }

    // Validate finite values
    if !scale.is_finite()
        || !entity.transform.translation.is_finite()
        || !entity
            .transform
            .rotation
            .to_array()
            .iter()
            .all(|v| v.is_finite())
    {
        bail!("entity '{name}' (index {index}) has non-finite transform values");
    }

    // Validate body info
    if let Some(body_info) = &entity.body_info {
        let he = body_info.half_extents;
        if he.x <= 0.0 || he.y <= 0.0 || he.z <= 0.0 {
            bail!("entity '{name}' (index {index}) has non-positive half-extents: {he:?}");
        }
    }

    Ok(())
}

/// Spawn one entity without its physics body (see `spawn_body`).
fn spawn_scene_entity(world: &mut World, entity: &SceneEntity) -> Entity {
    let id = world
        .spawn((Name(entity.name.clone()), entity.transform))
        .id();

    let model = entity
        .model
        .clone()
        .or_else(|| entity.mesh_ref.map(|m| m.model_path().to_string()));
    if let Some(path) = model {
        let server = &mut *world.get_resource_or_insert_with(|| AssetServer::new(".".into()));
        let by_guid = entity.model_guid.and_then(|guid| {
            let handle = server.load_guid(guid, split_mesh_fragment(&path).1);
            if handle.is_none() {
                tracing::warn!(
                    "entity '{}': model GUID {guid} not found; using {path}",
                    entity.name
                );
            }
            handle
        });
        let handle: Handle<Model> = by_guid.unwrap_or_else(|| server.load(&path));
        world.entity_mut(id).insert(handle);
    }

    if let Some(aabb) = entity.aabb {
        world.entity_mut(id).insert(aabb);
    }

    if entity.spin.is_some() {
        world.entity_mut(id).insert(Spin);
    }

    if entity.bob.is_some() {
        world.entity_mut(id).insert(Bob);
    }
    id
}

/// Create the rapier body for `id` at its world pose.
fn spawn_body(world: &mut World, physics: &mut PhysicsWorld, id: Entity, body_info: BodyInfo) {
    let pose = Transform::from_mat4(world_matrix(world, id));
    let (kind, (body_h, collider_h)) = match body_info.body_type {
        BodyKind::Dynamic => (
            RigidBodyType::Dynamic,
            physics.spawn_dynamic_cuboid(pose.translation, pose.rotation, body_info.half_extents),
        ),
        BodyKind::Fixed => (
            RigidBodyType::Fixed,
            physics.spawn_fixed_cuboid(pose.translation, pose.rotation, body_info.half_extents),
        ),
        BodyKind::None => {
            world.entity_mut(id).insert(body_info);
            return;
        }
    };
    world.entity_mut(id).insert((
        RigidBodyComponent {
            body_handle: body_h,
            body_type: kind,
        },
        ColliderComponent {
            collider_handle: collider_h,
        },
        body_info,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World;
    use melon_core::glam::{Quat, Vec3};

    fn world_with_entities() -> World {
        let mut world = World::new();
        world.insert_resource(NameCounter(5));
        let mut server = AssetServer::new(".".into());
        let cube: Handle<Model> = server.load("builtin:cube");
        let model: Handle<Model> = server.load("assets/models/cube.glb");
        world.insert_resource(server);
        world.spawn((
            Name("Floor".to_string()),
            Transform::from_translation(Vec3::new(0.0, -1.0, 0.0)),
            cube,
            Aabb {
                half_extents: Vec3::new(10.0, 0.5, 10.0),
            },
        ));
        world.spawn((Name("Cube".to_string()), Transform::default(), model, Spin));
        world
    }

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

    /// Car > (WheelA at local (1,0,0), WheelB).
    fn car_world() -> World {
        let mut world = World::new();
        world.insert_resource(AssetServer::new(".".into()));
        let car = world
            .spawn((
                Name("Car".to_string()),
                Transform::from_translation(Vec3::new(5.0, 0.0, 0.0)),
            ))
            .id();
        for (name, x) in [("WheelA", 1.0), ("WheelB", -1.0)] {
            world.spawn((
                Name(name.to_string()),
                Transform::from_translation(Vec3::new(x, 0.0, 0.0)),
                melon_core::ChildOf(car),
            ));
        }
        world
    }

    fn entity(name: &str, parent: Option<u32>) -> SceneEntity {
        SceneEntity {
            name: name.to_string(),
            transform: Transform::default(),
            parent,
            mesh_ref: None,
            model: None,
            model_guid: None,
            aabb: None,
            body_info: None,
            spin: None,
            bob: None,
        }
    }

    #[test]
    fn v3_roundtrip_keeps_parents_local_transforms_and_order() {
        let mut world = car_world();
        let mut physics = PhysicsWorld::new();
        let data = scene_to_data(&mut world);
        apply_scene(&mut world, &mut physics, &data).unwrap();
        let car = named(&mut world, "Car");
        let children: Vec<String> = world
            .get::<melon_core::Children>(car)
            .unwrap()
            .iter()
            .map(|c| world.get::<Name>(*c).unwrap().0.clone())
            .collect();
        assert_eq!(children, ["WheelA", "WheelB"]);
        let a = named(&mut world, "WheelA");
        assert_eq!(world.get::<Transform>(a).unwrap().translation, Vec3::X);
    }

    #[test]
    fn v2_scene_loads_all_roots() {
        let ron = r#"(version: 2, entities: [
            (name: "A", transform: (translation: (0.0, 0.0, 0.0), rotation: (0.0, 0.0, 0.0, 1.0), scale: (1.0, 1.0, 1.0))),
            (name: "B", transform: (translation: (0.0, 0.0, 0.0), rotation: (0.0, 0.0, 0.0, 1.0), scale: (1.0, 1.0, 1.0))),
        ], name_counter: 0)"#;
        let data: SceneData = ron::from_str(ron).unwrap();
        let mut world = World::new();
        apply_scene(&mut world, &mut PhysicsWorld::new(), &data).unwrap();
        assert_eq!(
            world.query::<&melon_core::ChildOf>().iter(&world).count(),
            0
        );
        assert_eq!(world.query::<&Name>().iter(&world).count(), 2);
    }

    #[test]
    fn bad_parent_index_fails_and_keeps_world() {
        let cases = [
            vec![entity("Lost", Some(99))],
            vec![entity("Selfie", Some(0))],
            vec![entity("Egg", Some(1)), entity("Hen", Some(0))],
        ];
        for entities in cases {
            let mut world = car_world();
            let data = SceneData {
                version: SCENE_VERSION,
                entities,
                name_counter: 0,
            };
            let err = apply_scene(&mut world, &mut PhysicsWorld::new(), &data).unwrap_err();
            let first = &data.entities[0].name;
            assert!(err.to_string().contains(first.as_str()), "{err}");
            assert_eq!(world.query::<&Name>().iter(&world).count(), 3);
        }
    }

    #[test]
    fn reapplying_scene_with_children_does_not_panic() {
        let mut world = car_world();
        let mut physics = PhysicsWorld::new();
        let data = scene_to_data(&mut world);
        apply_scene(&mut world, &mut physics, &data).unwrap();
        apply_scene(&mut world, &mut physics, &data).unwrap();
        assert_eq!(world.query::<&Name>().iter(&world).count(), 3);
    }

    #[test]
    fn child_body_spawns_at_world_pose() {
        let mut parent = entity("Parent", None);
        parent.transform = Transform {
            translation: Vec3::new(5.0, 0.0, 0.0),
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            ..Default::default()
        };
        let mut child = entity("Child", Some(0));
        child.transform = Transform::from_translation(Vec3::X);
        child.body_info = Some(BodyInfo {
            body_type: BodyKind::Fixed,
            half_extents: Vec3::splat(0.5),
        });
        let data = SceneData {
            version: SCENE_VERSION,
            entities: vec![parent, child],
            name_counter: 0,
        };
        let mut world = World::new();
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

    #[test]
    fn v2_roundtrip_keeps_model_paths() {
        let mut world = world_with_entities();
        let mut physics = PhysicsWorld::new();
        let data = scene_to_data(&mut world);
        apply_scene(&mut world, &mut physics, &data).unwrap();
        assert_eq!(model_path_of(&mut world, "Floor"), "builtin:cube");
        assert_eq!(model_path_of(&mut world, "Cube"), "assets/models/cube.glb");
    }

    #[test]
    fn v1_mesh_ref_cube_loads_as_builtin() {
        let ron = r#"(
            version: 1,
            entities: [
                (name: "Old", transform: (translation: (0.0, 0.0, 0.0), rotation: (0.0, 0.0, 0.0, 1.0), scale: (1.0, 1.0, 1.0)),
                 mesh_ref: Some(Cube), model_path: None),
                (name: "OldModel", transform: (translation: (0.0, 0.0, 0.0), rotation: (0.0, 0.0, 0.0, 1.0), scale: (1.0, 1.0, 1.0)),
                 mesh_ref: Some(GltfModel)),
            ],
            name_counter: 0,
        )"#;
        let data: SceneData = ron::from_str(ron).unwrap();
        let mut world = World::new();
        let mut physics = PhysicsWorld::new();
        apply_scene(&mut world, &mut physics, &data).unwrap();
        assert_eq!(model_path_of(&mut world, "Old"), "builtin:cube");
        assert_eq!(
            model_path_of(&mut world, "OldModel"),
            "assets/models/cube.glb"
        );
    }

    #[test]
    fn missing_model_path_still_loads() {
        let mut world = world_with_entities();
        let h: melon_core::Handle<melon_core::Model> = world
            .resource_mut::<melon_core::AssetServer>()
            .load("assets/models/does_not_exist.glb");
        world.spawn((Name("Ghost".to_string()), Transform::default(), h));
        let mut physics = PhysicsWorld::new();
        let data = scene_to_data(&mut world);
        apply_scene(&mut world, &mut physics, &data).unwrap();
        assert_eq!(
            model_path_of(&mut world, "Ghost"),
            "assets/models/does_not_exist.glb"
        );
    }

    #[test]
    fn save_load_roundtrip() {
        let mut world = world_with_entities();
        let mut physics = PhysicsWorld::new();

        let tmp = std::env::temp_dir().join("melon_test_scene.ron");
        save_scene(&mut world, &tmp).expect("save should succeed");
        assert!(tmp.exists(), "scene file should exist");

        // Clear scene entities. Not `clear_entities()`: in bevy 0.19 that also
        // deletes resources and leaves the resource cache stale.
        let named: Vec<Entity> = world
            .query_filtered::<Entity, With<Name>>()
            .iter(&world)
            .collect();
        for e in named {
            world.despawn(e);
        }
        assert_eq!(world.query::<&Name>().iter(&world).count(), 0);

        // Load
        load_scene(&mut world, &mut physics, &tmp).expect("load should succeed");

        let count = world.query::<&Name>().iter(&world).count();
        assert_eq!(count, 2, "should have 2 entities after load");

        // Verify Floor entity
        let floor = world
            .query::<(&Name, &Transform)>()
            .iter(&world)
            .find(|(n, _)| n.0 == "Floor");
        assert!(floor.is_some(), "Floor entity should exist");
        let (_, floor_transform) = floor.unwrap();
        assert!((floor_transform.translation.y - (-1.0)).abs() < 1e-6);

        // Verify name counter
        let counter = world.get_resource::<NameCounter>().unwrap();
        assert_eq!(counter.0, 5, "name counter should be restored");

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn load_validates_zero_scale() {
        let ron = r#"(
            version: 1,
            entities: [
                (
                    name: "BadScale",
                    transform: (
                        translation: (0.0, 0.0, 0.0),
                        rotation: (0.0, 0.0, 0.0, 1.0),
                        scale: (0.0, 1.0, 1.0),
                    ),
                ),
            ],
            name_counter: 1,
        )"#;

        let tmp = std::env::temp_dir().join("melon_test_bad_scale.ron");
        std::fs::write(&tmp, ron).unwrap();

        let mut world = World::new();
        world.insert_resource(NameCounter(0));
        let mut physics = PhysicsWorld::new();

        let result = load_scene(&mut world, &mut physics, &tmp);
        assert!(result.is_err(), "zero scale should fail validation");
        assert!(
            result.unwrap_err().to_string().contains("zero"),
            "error should mention zero scale"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn load_validates_negative_half_extents() {
        let ron = r#"(
            version: 1,
            entities: [
                (
                    name: "BadBody",
                    transform: (
                        translation: (0.0, 0.0, 0.0),
                        rotation: (0.0, 0.0, 0.0, 1.0),
                        scale: (1.0, 1.0, 1.0),
                    ),
                    body_info: Some((
                        body_type: Dynamic,
                        half_extents: (-1.0, 1.0, 1.0),
                    )),
                ),
            ],
            name_counter: 1,
        )"#;

        let tmp = std::env::temp_dir().join("melon_test_bad_he.ron");
        std::fs::write(&tmp, ron).unwrap();

        let mut world = World::new();
        world.insert_resource(NameCounter(0));
        let mut physics = PhysicsWorld::new();

        let result = load_scene(&mut world, &mut physics, &tmp);
        assert!(result.is_err(), "negative half-extents should fail");
        assert!(
            result.unwrap_err().to_string().contains("half-extents"),
            "error should mention half-extents"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn load_validates_unsupported_version() {
        let ron = r#"(
            version: 999,
            entities: [],
            name_counter: 0,
        )"#;

        let tmp = std::env::temp_dir().join("melon_test_bad_version.ron");
        std::fs::write(&tmp, ron).unwrap();

        let mut world = World::new();
        world.insert_resource(NameCounter(0));
        let mut physics = PhysicsWorld::new();

        let result = load_scene(&mut world, &mut physics, &tmp);
        assert!(result.is_err(), "unsupported version should fail");
        assert!(
            result.unwrap_err().to_string().contains("version"),
            "error should mention version"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn load_clears_existing_entities() {
        let mut world = world_with_entities();
        let mut physics = PhysicsWorld::new();

        // Save the 2-entity scene
        let tmp = std::env::temp_dir().join("melon_test_clear.ron");
        save_scene(&mut world, &tmp).unwrap();

        // Add a 3rd entity
        world.spawn((Name("Extra".to_string()), Transform::default()));
        assert_eq!(world.query::<&Name>().iter(&world).count(), 3);

        // Load — should clear all and have only 2
        load_scene(&mut world, &mut physics, &tmp).unwrap();
        assert_eq!(
            world.query::<&Name>().iter(&world).count(),
            2,
            "should have 2 entities after load (not 3)"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    fn floor_y(world: &mut World) -> f32 {
        world
            .query::<(&Name, &Transform)>()
            .iter(world)
            .find(|(n, _)| n.0 == "Floor")
            .map(|(_, t)| t.translation.y)
            .unwrap()
    }

    #[test]
    fn scene_to_data_then_apply_restores_transforms() {
        let mut world = world_with_entities();
        let mut physics = PhysicsWorld::new();
        let data = scene_to_data(&mut world);
        let floor = world
            .query::<(Entity, &Name)>()
            .iter(&world)
            .find(|(_, n)| n.0 == "Floor")
            .map(|(e, _)| e)
            .unwrap();
        world.get_mut::<Transform>(floor).unwrap().translation.y = 5.0;
        apply_scene(&mut world, &mut physics, &data).unwrap();
        assert!((floor_y(&mut world) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn scene_to_data_skips_entities_being_deleted() {
        let mut world = world_with_entities();
        let floor = world
            .query::<(Entity, &Name)>()
            .iter(&world)
            .find(|(_, n)| n.0 == "Floor")
            .map(|(e, _)| e)
            .unwrap();
        world.entity_mut(floor).insert(melon_core::PendingDelete);
        let mut physics = PhysicsWorld::new();
        let data = scene_to_data(&mut world);
        apply_scene(&mut world, &mut physics, &data).unwrap();
        let names: Vec<String> = world
            .query::<&Name>()
            .iter(&world)
            .map(|n| n.0.clone())
            .collect();
        assert_eq!(names, ["Cube"]);
    }

    #[test]
    fn apply_scene_invalid_data_leaves_world_untouched() {
        let mut bad = World::new();
        bad.spawn((
            Name("Flat".to_string()),
            Transform {
                scale: Vec3::ZERO,
                ..Default::default()
            },
        ));
        let data = scene_to_data(&mut bad);
        let mut world = world_with_entities();
        let mut physics = PhysicsWorld::new();
        assert!(apply_scene(&mut world, &mut physics, &data).is_err());
        assert_eq!(world.query::<&Name>().iter(&world).count(), 2);
    }

    #[test]
    fn every_transform_entity_in_default_spawn_paths_has_name() {
        // apply_scene clears every Transform entity but scenes save only named ones.
        let mut world = world_with_entities();
        let with_transform = world.query::<&Transform>().iter(&world).count();
        let named = world.query::<(&Transform, &Name)>().iter(&world).count();
        assert_eq!(with_transform, named);
    }

    fn read_log_interval_system(_interval: bevy_ecs::system::Res<melon_core::LogInterval>) {}

    #[test]
    fn load_keeps_world_so_systems_and_resources_survive() {
        let mut world = world_with_entities();
        world.insert_resource(melon_core::LogInterval { frames_per_log: 7 });
        let mut physics = PhysicsWorld::new();
        // Systems bind to the World they first run on; a load must keep that World.
        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(read_log_interval_system);
        schedule.run(&mut world);
        let world_id = world.id();

        let tmp = std::env::temp_dir().join("melon_test_keep_world.ron");
        save_scene(&mut world, &tmp).unwrap();
        load_scene(&mut world, &mut physics, &tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        assert_eq!(world.id(), world_id, "load must not swap the World");
        assert_eq!(
            world.resource::<melon_core::LogInterval>().frames_per_log,
            7,
            "resources must survive a load"
        );
        schedule.run(&mut world); // panicked with "mismatched World" before the fix
        assert_eq!(world.query::<&Name>().iter(&world).count(), 2);
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

        let mut world = World::new();
        world.insert_resource(NameCounter(0));
        let mut physics = PhysicsWorld::new();

        load_scene(&mut world, &mut physics, &tmp).unwrap();

        assert_eq!(physics.bodies.len(), 1, "should have 1 physics body");
        assert_eq!(physics.colliders.len(), 1, "should have 1 collider");

        let has_rb = world.query::<&RigidBodyComponent>().iter(&world).count();
        assert_eq!(has_rb, 1, "entity should have RigidBodyComponent");

        let _ = std::fs::remove_file(&tmp);
    }

    /// A temp project with `assets/models/a.glb` and its `.meta`, and a world
    /// whose `AssetServer` has scanned it.
    fn guid_world(name: &str) -> (World, std::path::PathBuf, melon_core::AssetGuid) {
        let root = std::env::temp_dir().join(format!("melon_scene_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let file = root.join("assets/models/a.glb");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();
        let guid = melon_core::AssetGuid::new();
        let meta = melon_core::AssetMeta {
            version: melon_core::META_VERSION,
            guid,
        };
        std::fs::write(melon_core::meta_path(&file), meta.to_ron()).unwrap();
        let mut server = AssetServer::new(root.clone());
        server.scan_metas();
        let mut world = World::new();
        world.insert_resource(server);
        (world, root, guid)
    }

    fn one_entity_scene(model: &str, guid: Option<melon_core::AssetGuid>) -> SceneData {
        let guid = guid.map_or("None".to_string(), |g| format!("Some(\"{g}\")"));
        let ron = format!(
            r#"(version: 4, entities: [(name: "A", transform: (translation: (0.0, 0.0, 0.0), rotation: (0.0, 0.0, 0.0, 1.0), scale: (1.0, 1.0, 1.0)), model: Some("{model}"), model_guid: {guid})], name_counter: 0)"#
        );
        ron::from_str(&ron).unwrap()
    }

    #[test]
    fn v4_save_writes_model_guid() {
        let (mut world, root, guid) = guid_world("v4_save");
        let h: Handle<Model> = world
            .resource_mut::<AssetServer>()
            .load("assets/models/a.glb#mesh1");
        world.spawn((Name("A".to_string()), Transform::default(), h));
        let data = scene_to_data(&mut world);
        let ron = ron::to_string(&data).unwrap();
        assert!(
            ron.contains(&format!("model_guid:Some(\"{guid}\")")),
            "{ron}"
        );
        assert!(ron.contains("assets/models/a.glb#mesh1"), "{ron}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn v4_guid_wins_over_stale_path() {
        let (mut world, root, guid) = guid_world("v4_stale");
        let data = one_entity_scene("assets/models/old.glb#mesh1", Some(guid));
        apply_scene(&mut world, &mut PhysicsWorld::new(), &data).unwrap();
        assert_eq!(model_path_of(&mut world, "A"), "assets/models/a.glb#mesh1");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn v4_unknown_guid_falls_back_to_path() {
        let (mut world, root, _) = guid_world("v4_unknown");
        let data = one_entity_scene("assets/models/a.glb", Some(melon_core::AssetGuid::new()));
        apply_scene(&mut world, &mut PhysicsWorld::new(), &data).unwrap();
        assert_eq!(model_path_of(&mut world, "A"), "assets/models/a.glb");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn v4_path_only_entity_loads() {
        let (mut world, root, _) = guid_world("v4_path_only");
        let ron = r#"(version: 4, entities: [(name: "A", transform: (translation: (0.0, 0.0, 0.0), rotation: (0.0, 0.0, 0.0, 1.0), scale: (1.0, 1.0, 1.0)), model: Some("assets/models/a.glb"))], name_counter: 0)"#;
        let data: SceneData = ron::from_str(ron).unwrap();
        apply_scene(&mut world, &mut PhysicsWorld::new(), &data).unwrap();
        assert_eq!(model_path_of(&mut world, "A"), "assets/models/a.glb");
        let _ = std::fs::remove_dir_all(&root);
    }
}
