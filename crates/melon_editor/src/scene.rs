use std::path::Path;

use anyhow::{Context, Result, bail};
use bevy_ecs::world::World;
use melon_core::{Aabb, Bob, BodyInfo, BodyKind, MeshRef, Name, NameCounter, Spin, Transform};
use melon_physics::{ColliderComponent, PhysicsWorld, RigidBodyComponent, RigidBodyType};
use serde::{Deserialize, Serialize};

const SCENE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct SceneData {
    version: u32,
    entities: Vec<SceneEntity>,
    name_counter: u32,
}

#[derive(Serialize, Deserialize)]
pub struct SceneEntity {
    name: String,
    transform: Transform,
    #[serde(default)]
    mesh_ref: Option<MeshRef>,
    #[serde(default)]
    aabb: Option<Aabb>,
    #[serde(default)]
    body_info: Option<BodyInfo>,
    #[serde(default)]
    spin: Option<Spin>,
    #[serde(default)]
    bob: Option<Bob>,
    #[serde(default)]
    model_path: Option<String>,
}

/// Save all entities with Name + Transform to a RON file.
/// Uses atomic write (temp file + rename) to prevent corruption.
pub fn save_scene(world: &mut World, path: &Path) -> Result<()> {
    let mut query = world.query::<(
        &Name,
        &Transform,
        Option<&MeshRef>,
        Option<&Aabb>,
        Option<&BodyInfo>,
        Option<&Spin>,
        Option<&Bob>,
    )>();

    let entities: Vec<SceneEntity> = query
        .iter(world)
        .map(
            |(name, transform, mesh_ref, aabb, body_info, spin, bob)| SceneEntity {
                name: name.0.clone(),
                transform: *transform,
                mesh_ref: mesh_ref.copied(),
                aabb: aabb.copied(),
                body_info: body_info.copied(),
                spin: spin.copied(),
                bob: bob.copied(),
                model_path: None,
            },
        )
        .collect();

    let name_counter = world
        .get_resource::<NameCounter>()
        .map(|c| c.0)
        .unwrap_or(0);

    let data = SceneData {
        version: SCENE_VERSION,
        entities,
        name_counter,
    };

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

/// Load a scene from a RON file.
/// Two-phase: validate all entities before mutating the world.
/// Clears all existing entities + physics bodies, then spawns from scene data.
pub fn load_scene(world: &mut World, physics: &mut PhysicsWorld, path: &Path) -> Result<()> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read scene file: {}", path.display()))?;

    let data: SceneData =
        ron::from_str(&contents).context("failed to deserialize scene from RON")?;

    // Phase 1: Validate (no world mutation)
    if data.version != SCENE_VERSION {
        bail!(
            "unsupported scene version: expected {}, got {}",
            SCENE_VERSION,
            data.version
        );
    }

    for (i, entity) in data.entities.iter().enumerate() {
        validate_entity(entity, i)?;
    }

    // Phase 2: Mutate world
    // Create a fresh world with preserved resources, then swap.
    // This avoids the bevy_ecs 0.19 ResourceCache panic that despawn triggers.
    let mut new_world = World::new();

    // Preserve engine resources
    if let Some(c) = world.get_resource::<melon_core::Camera>() {
        new_world.insert_resource(*c);
    }
    if let Some(l) = world.get_resource::<melon_core::Light>() {
        new_world.insert_resource(*l);
    }
    if let Some(i) = world.get_resource::<melon_core::Input>() {
        new_world.insert_resource(i.clone());
    }
    if let Some(t) = world.get_resource::<melon_core::Time>() {
        new_world.insert_resource(*t);
    }
    if let Some(s) = world.get_resource::<melon_core::SimulationState>() {
        new_world.insert_resource(*s);
    }
    if let Some(fc) = world.get_resource::<melon_core::FrameCounter>() {
        new_world.insert_resource(melon_core::FrameCounter { frame: fc.frame });
    }
    new_world.insert_resource(NameCounter(data.name_counter));

    physics.clear();

    // Spawn entities from scene data into the new world
    for entity in &data.entities {
        spawn_scene_entity(&mut new_world, physics, entity);
    }

    // Swap the new world in
    std::mem::swap(world, &mut new_world);

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

fn spawn_scene_entity(world: &mut World, physics: &mut PhysicsWorld, entity: &SceneEntity) {
    let id = world
        .spawn((Name(entity.name.clone()), entity.transform))
        .id();

    if let Some(mesh_ref) = entity.mesh_ref {
        world.entity_mut(id).insert(mesh_ref);
    }

    if let Some(aabb) = entity.aabb {
        world.entity_mut(id).insert(aabb);
    }

    if let Some(body_info) = entity.body_info {
        // Create rapier body based on BodyKind
        match body_info.body_type {
            BodyKind::Dynamic => {
                let (body_h, collider_h) = physics
                    .spawn_dynamic_cuboid(entity.transform.translation, body_info.half_extents);
                world.entity_mut(id).insert((
                    RigidBodyComponent {
                        body_handle: body_h,
                        body_type: RigidBodyType::Dynamic,
                    },
                    ColliderComponent {
                        collider_handle: collider_h,
                    },
                    body_info,
                ));
            }
            BodyKind::Fixed => {
                let (body_h, collider_h) = physics
                    .spawn_fixed_cuboid(entity.transform.translation, body_info.half_extents);
                world.entity_mut(id).insert((
                    RigidBodyComponent {
                        body_handle: body_h,
                        body_type: RigidBodyType::Fixed,
                    },
                    ColliderComponent {
                        collider_handle: collider_h,
                    },
                    body_info,
                ));
            }
            BodyKind::None => {
                world.entity_mut(id).insert(body_info);
            }
        }
    }

    if entity.spin.is_some() {
        world.entity_mut(id).insert(Spin);
    }

    if entity.bob.is_some() {
        world.entity_mut(id).insert(Bob);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World;
    use melon_core::glam::Vec3;

    fn world_with_entities() -> World {
        let mut world = World::new();
        world.insert_resource(NameCounter(5));
        world.spawn((
            Name("Floor".to_string()),
            Transform::from_translation(Vec3::new(0.0, -1.0, 0.0)),
            MeshRef::Cube,
            Aabb {
                half_extents: Vec3::new(10.0, 0.5, 10.0),
            },
        ));
        world.spawn((
            Name("Cube".to_string()),
            Transform::default(),
            MeshRef::GltfModel,
            Spin,
        ));
        world
    }

    #[test]
    fn save_load_roundtrip() {
        let mut world = world_with_entities();
        let mut physics = PhysicsWorld::new();

        let tmp = std::env::temp_dir().join("melon_test_scene.ron");
        save_scene(&mut world, &tmp).expect("save should succeed");
        assert!(tmp.exists(), "scene file should exist");

        // Clear world
        world.clear_entities();
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
}
