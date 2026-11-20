//! Extraction: the ECS world turned into plain data the renderer draws.

use std::collections::HashMap;

use crate::lights::LocalLight;
use glam::{Mat4, Vec3};
use melon_core::bevy_ecs::component::Component;
use melon_core::bevy_ecs::entity::Entity;
use melon_core::bevy_ecs::prelude::Mut;
use melon_core::bevy_ecs::world::World;
use melon_core::hierarchy::render_matrices;
use melon_core::{
    AssetServer, Camera, Environment, Handle, Light, Lod, LodState, Material, MeshMaterials, Model,
    PointLight, RenderQuality, SimulationState, SpotLight, Time,
};

/// Everything one view needs to draw a frame.
#[derive(Default)]
pub struct FrameData {
    pub view_proj: Mat4,
    /// Camera position (overlay shading).
    pub eye: glam::Vec3,
    pub light: Light,
    /// The look: sky, fog, pass strengths.
    pub environment: Environment,
    /// Which passes run and at what cost (GI mode, AO, bloom, shadow map
    /// size, MSAA).
    pub quality: RenderQuality,
    /// World matrix and model of every entity with a `Handle<Model>`.
    pub models: Vec<(Mat4, Handle<Model>)>,
    /// Material per mesh part for the `models` entries (by index) that have
    /// `MeshMaterials`; `None` keeps the part's glTF material.
    pub overrides: HashMap<usize, Vec<Option<Handle<Material>>>>,
    /// Every point and spot light, in world space.
    pub lights: Vec<LocalLight>,
}

/// Blend fixed-step entities while Playing and Paused (alpha is frozen while
/// paused, so the pose holds still). In edit mode draw the current pose.
pub fn blends(state: SimulationState) -> bool {
    state != SimulationState::Editing
}

/// `Lod` levels resolved to handles, kept on the entity until the levels
/// or the GUID index change (resolving parses GUID strings).
#[derive(Component)]
struct ResolvedLod {
    epoch: u32,
    levels: Vec<melon_core::LodLevel>,
    handles: Vec<Option<Handle<Model>>>,
}

/// `MeshMaterials` slots resolved to handles, likewise.
#[derive(Component)]
struct ResolvedMaterials {
    epoch: u32,
    parts: Vec<melon_core::MaterialSlot>,
    handles: Vec<Option<Handle<Material>>>,
}

/// Draw each `Lod` entity at the level its screen height picks (the model
/// bounds come from the `AssetServer`, a unit cube until loaded), and
/// remember the level in `LodState` (when it changes) so it holds on its
/// thresholds. `slots` collects the entities whose `MeshMaterials` apply to
/// the model entries they push.
fn add_lod_models(
    world: &mut World,
    matrices: &HashMap<Entity, Mat4>,
    view_proj: Mat4,
    models: &mut Vec<(Mat4, Handle<Model>)>,
    slots: &mut Vec<(usize, Entity)>,
) {
    if !world.contains_resource::<AssetServer>() {
        return;
    }
    let mut states = Vec::new();
    let mut resolved = Vec::new();
    world.resource_scope(|world, mut server: Mut<AssetServer>| {
        let epoch = server.index_epoch();
        let mut thresholds = Vec::new();
        let mut query = world.query::<(
            Entity,
            &Lod,
            Option<&LodState>,
            Option<&MeshMaterials>,
            Option<&ResolvedLod>,
        )>();
        for (e, lod, state, materials, cached) in query.iter(world) {
            let Some(&matrix) = matrices.get(&e) else {
                continue;
            };
            if lod.levels.is_empty() {
                continue;
            }
            let fresh;
            let handles = match cached {
                Some(c) if c.epoch == epoch && c.levels == lod.levels => &c.handles,
                _ => {
                    fresh = lod
                        .levels
                        .iter()
                        .map(|l| l.resolve(&mut server))
                        .collect::<Vec<_>>();
                    resolved.push((
                        e,
                        ResolvedLod {
                            epoch,
                            levels: lod.levels.clone(),
                            handles: fresh.clone(),
                        },
                    ));
                    &fresh
                }
            };
            let (min, max) = handles
                .iter()
                .flatten()
                .find_map(|h| server.bounds(*h))
                .unwrap_or((Vec3::splat(-0.5), Vec3::splat(0.5)));
            let scale = matrix
                .x_axis
                .length()
                .max(matrix.y_axis.length())
                .max(matrix.z_axis.length());
            let centre = matrix.transform_point3((min + max) * 0.5);
            let height = melon_core::lod::screen_height(
                view_proj,
                centre,
                (max - min).length() * 0.5 * scale,
            );
            thresholds.clear();
            thresholds.extend(lod.levels.iter().map(|l| l.screen_height));
            let level = melon_core::lod::select_level(
                &thresholds,
                lod.hysteresis,
                height,
                state.map(|s| s.0),
            );
            let level_state = level.unwrap_or(lod.levels.len());
            if state.map(|s| s.0) != Some(level_state) {
                states.push((e, level_state));
            }
            if let Some(handle) = level.and_then(|i| handles[i]) {
                // The same part slots apply to every level.
                if materials.is_some() {
                    slots.push((models.len(), e));
                }
                models.push((matrix, handle));
            }
        }
    });
    for (e, level) in states {
        world.entity_mut(e).insert(LodState(level));
    }
    for (e, cache) in resolved {
        world.entity_mut(e).insert(cache);
    }
}

/// Read the camera, the light, the environment and every model entity's world matrix
/// (interpolated along its parent chain) from `world`.
pub fn extract(world: &mut World) -> FrameData {
    let view_proj = world
        .get_resource::<Camera>()
        .map(Camera::view_proj)
        .unwrap_or_default();
    let eye = world
        .get_resource::<Camera>()
        .map_or(glam::Vec3::ZERO, |c| c.eye);
    let light = world.get_resource::<Light>().copied().unwrap_or_default();
    let environment = world
        .get_resource::<Environment>()
        .copied()
        .unwrap_or_default();
    let quality = world
        .get_resource::<RenderQuality>()
        .copied()
        .unwrap_or_default();
    let alpha = world.get_resource::<Time>().map_or(1.0, Time::alpha);
    let state = world
        .get_resource::<SimulationState>()
        .copied()
        .unwrap_or(SimulationState::Editing);
    let matrices = render_matrices(world, alpha, blends(state));
    let mut models = Vec::new();
    let mut slots: Vec<(usize, Entity)> = Vec::new();
    let mut query = world.query::<(Entity, &Handle<Model>, Option<&MeshMaterials>, Option<&Lod>)>();
    for (e, h, materials, lod) in query.iter(world) {
        // An entity with LOD levels draws the chosen level instead.
        if lod.is_some_and(|lod| !lod.levels.is_empty()) {
            continue;
        }
        if let Some(m) = matrices.get(&e) {
            if materials.is_some() {
                slots.push((models.len(), e));
            }
            models.push((*m, *h));
        }
    }
    add_lod_models(world, &matrices, view_proj, &mut models, &mut slots);
    let mut overrides = HashMap::new();
    if world.contains_resource::<AssetServer>() {
        let mut resolved = Vec::new();
        world.resource_scope(|world, mut server: Mut<AssetServer>| {
            let epoch = server.index_epoch();
            for (index, e) in slots {
                let Some(materials) = world.get::<MeshMaterials>(e) else {
                    continue;
                };
                match world.get::<ResolvedMaterials>(e) {
                    Some(c) if c.epoch == epoch && c.parts == materials.parts => {
                        overrides.insert(index, c.handles.clone());
                    }
                    _ => {
                        let handles: Vec<_> = materials
                            .parts
                            .iter()
                            .map(|s| s.resolve(&mut server))
                            .collect();
                        overrides.insert(index, handles.clone());
                        resolved.push((
                            e,
                            ResolvedMaterials {
                                epoch,
                                parts: materials.parts.clone(),
                                handles,
                            },
                        ));
                    }
                }
            }
        });
        for (e, cache) in resolved {
            world.entity_mut(e).insert(cache);
        }
    }
    let mut lights = Vec::new();
    let mut points = world.query::<(Entity, &PointLight)>();
    for (e, light) in points.iter(world) {
        if let Some(m) = matrices.get(&e) {
            lights.push(LocalLight {
                position: m.w_axis.truncate(),
                direction: Vec3::NEG_Z,
                color: light.color * light.intensity,
                range: light.range,
                spot: None,
                cast_shadows: light.cast_shadows,
            });
        }
    }
    let mut spots = world.query::<(Entity, &SpotLight)>();
    for (e, light) in spots.iter(world) {
        if let Some(m) = matrices.get(&e) {
            lights.push(LocalLight {
                position: m.w_axis.truncate(),
                direction: (-m.z_axis.truncate()).normalize_or(Vec3::NEG_Y),
                color: light.color * light.intensity,
                range: light.range,
                spot: Some((light.inner_angle, light.outer_angle.max(light.inner_angle))),
                cast_shadows: light.cast_shadows,
            });
        }
    }
    FrameData {
        view_proj,
        eye,
        light,
        environment,
        quality,
        models,
        overrides,
        lights,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use melon_core::bevy_ecs::world::World;
    use melon_core::{
        AssetServer, Camera, ChildOf, Environment, Handle, Light, Model, RenderQuality,
        SimulationState, Transform,
    };

    fn cube(world: &mut World) -> Handle<Model> {
        world
            .get_resource_or_insert_with(|| AssetServer::new(".".into()))
            .load("builtin:cube")
    }

    fn translation(m: glam::Mat4) -> Vec3 {
        m.w_axis.truncate()
    }

    #[test]
    fn one_model_entity_gives_one_entry() {
        let mut world = World::new();
        let h = cube(&mut world);
        world.spawn((Transform::from_translation(Vec3::new(1.0, 2.0, 3.0)), h));
        let frame = extract(&mut world);
        assert_eq!(frame.models.len(), 1);
        assert_eq!(translation(frame.models[0].0), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(frame.models[0].1, h);
    }

    #[test]
    fn mesh_materials_resolve_to_handles_by_model_index() {
        use melon_core::{MaterialSlot, MeshMaterials};
        let mut world = World::new();
        let h = cube(&mut world);
        world.spawn((Transform::default(), h));
        world.spawn((
            Transform::default(),
            h,
            MeshMaterials {
                parts: vec![
                    MaterialSlot::default(),
                    MaterialSlot {
                        guid: String::new(),
                        path: "assets/m.mat".into(),
                    },
                ],
            },
        ));
        let frame = extract(&mut world);
        assert_eq!(frame.overrides.len(), 1);
        let parts = frame.overrides.values().next().unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].is_none());
        let server = world.resource::<AssetServer>();
        assert_eq!(server.path(parts[1].unwrap()), "assets/m.mat");
    }

    #[test]
    fn lod_entities_draw_the_level_their_distance_picks_with_hysteresis() {
        use melon_core::{Lod, LodLevel, LodState};
        let level = |path: &str, screen_height| LodLevel {
            guid: String::new(),
            path: path.into(),
            screen_height,
        };
        let mut world = World::new();
        world.insert_resource(AssetServer::new(".".into()));
        let camera = Camera {
            eye: Vec3::ZERO,
            center: Vec3::new(0.0, 0.0, -1.0),
            aspect: 1.0,
            ..Default::default()
        };
        // The unit cube (radius 0.866) covers this share of the view at 8 m.
        let t0 =
            melon_core::lod::screen_height(camera.view_proj(), Vec3::new(0.0, 0.0, -8.0), 0.866);
        world.insert_resource(camera);
        let e = world
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 0.0, -5.0)),
                Lod {
                    levels: vec![level("builtin:cube", t0), level("assets/far.glb", 0.01)],
                    hysteresis: 0.1,
                },
            ))
            .id();
        let drawn = |world: &mut World, z: f32| {
            world.get_mut::<Transform>(e).unwrap().translation.z = z;
            let frame = extract(world);
            let server = world.resource::<AssetServer>();
            frame
                .models
                .iter()
                .map(|(_, h)| server.path(*h).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(drawn(&mut world, -5.0), ["builtin:cube"]);
        assert_eq!(drawn(&mut world, -20.0), ["assets/far.glb"]);
        assert!(drawn(&mut world, -400.0).is_empty(), "culled when tiny");
        assert_eq!(world.get::<LodState>(e), Some(&LodState(2)));
        // The level-0 threshold sits at 8 m. Coming from far, the switch
        // back needs 10% more height; going away, 10% less.
        let _ = drawn(&mut world, -20.0);
        assert_eq!(drawn(&mut world, -7.9), ["assets/far.glb"], "held");
        assert_eq!(drawn(&mut world, -6.0), ["builtin:cube"]);
        assert_eq!(drawn(&mut world, -8.5), ["builtin:cube"], "held");
    }

    #[test]
    fn lod_state_is_written_only_when_the_level_changes() {
        use melon_core::{Lod, LodLevel, LodState};
        let mut world = World::new();
        world.insert_resource(AssetServer::new(".".into()));
        world.insert_resource(Camera {
            eye: Vec3::ZERO,
            center: Vec3::new(0.0, 0.0, -1.0),
            aspect: 1.0,
            ..Default::default()
        });
        let e = world
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 0.0, -5.0)),
                Lod {
                    levels: vec![LodLevel {
                        guid: String::new(),
                        path: "builtin:cube".into(),
                        screen_height: 0.001,
                    }],
                    hysteresis: 0.1,
                },
            ))
            .id();
        extract(&mut world);
        world.increment_change_tick();
        let since = world.read_change_tick();
        world.increment_change_tick();
        extract(&mut world);
        let ticks = world.entity(e).get_change_ticks::<LodState>().unwrap();
        assert!(
            !ticks.is_changed(since, world.read_change_tick()),
            "an unchanged level was written again"
        );
        // Moving far away changes the level: written once.
        world.get_mut::<Transform>(e).unwrap().translation.z = -3000.0;
        world.increment_change_tick();
        let since = world.read_change_tick();
        world.increment_change_tick();
        extract(&mut world);
        let ticks = world.entity(e).get_change_ticks::<LodState>().unwrap();
        assert!(ticks.is_changed(since, world.read_change_tick()));
    }

    #[test]
    fn resolved_handles_follow_edits_and_the_guid_index() {
        use melon_core::{MaterialSlot, MeshMaterials};
        let mut world = World::new();
        let h = cube(&mut world);
        let slot = |path: &str| MaterialSlot {
            guid: String::new(),
            path: path.into(),
        };
        let e = world
            .spawn((
                Transform::default(),
                h,
                MeshMaterials {
                    parts: vec![slot("assets/a.mat")],
                },
            ))
            .id();
        let path_of = |world: &mut World| {
            let frame = extract(world);
            let handle = frame.overrides[&0][0].unwrap();
            world.resource::<AssetServer>().path(handle).to_string()
        };
        assert_eq!(path_of(&mut world), "assets/a.mat");
        assert_eq!(path_of(&mut world), "assets/a.mat");
        world.get_mut::<MeshMaterials>(e).unwrap().parts = vec![slot("assets/b.mat")];
        assert_eq!(
            path_of(&mut world),
            "assets/b.mat",
            "an edited slot is resolved again"
        );
    }

    #[test]
    fn cached_handles_are_resolved_again_when_the_guid_index_changes() {
        use melon_core::{AssetGuid, MaterialSlot, MeshMaterials};
        let root = std::env::temp_dir().join(format!("melon_extract_epoch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets/materials")).unwrap();
        let guid = AssetGuid::new();
        let place = |name: &str| {
            std::fs::write(root.join(format!("assets/materials/{name}.mat")), "()").unwrap();
            std::fs::write(
                root.join(format!("assets/materials/{name}.mat.meta")),
                format!("(version: 1, guid: \"{guid}\")"),
            )
            .unwrap();
        };
        place("a");
        let mut world = World::new();
        let mut server = AssetServer::new(root.clone());
        server.scan_metas();
        let h = server.load::<Model>("builtin:cube");
        world.insert_resource(server);
        world.spawn((
            Transform::default(),
            h,
            MeshMaterials {
                parts: vec![MaterialSlot {
                    guid: guid.to_string(),
                    path: String::new(),
                }],
            },
        ));
        let path_of = |world: &mut World| {
            let frame = extract(world);
            let handle = frame.overrides[&0][0].unwrap();
            world.resource::<AssetServer>().path(handle).to_string()
        };
        assert_eq!(path_of(&mut world), "assets/materials/a.mat");
        // The file moves outside the editor; the index is rescanned.
        std::fs::remove_file(root.join("assets/materials/a.mat")).unwrap();
        std::fs::remove_file(root.join("assets/materials/a.mat.meta")).unwrap();
        place("b");
        world.resource_mut::<AssetServer>().scan_metas();
        assert_eq!(path_of(&mut world), "assets/materials/b.mat");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mesh_materials_apply_to_the_chosen_lod_level() {
        use melon_core::{Lod, LodLevel, MaterialSlot};
        let mut world = World::new();
        world.insert_resource(AssetServer::new(".".into()));
        world.insert_resource(Camera {
            eye: Vec3::ZERO,
            center: Vec3::new(0.0, 0.0, -1.0),
            aspect: 1.0,
            ..Default::default()
        });
        let level = |path: &str| LodLevel {
            guid: String::new(),
            path: path.into(),
            screen_height: 0.001,
        };
        world.spawn((
            Transform::from_translation(Vec3::new(0.0, 0.0, -5.0)),
            Lod {
                levels: vec![level("builtin:cube")],
                hysteresis: 0.1,
            },
            MeshMaterials {
                parts: vec![MaterialSlot {
                    guid: String::new(),
                    path: "assets/m.mat".into(),
                }],
            },
        ));
        let frame = extract(&mut world);
        assert_eq!(frame.models.len(), 1);
        let parts = frame.overrides.get(&0).expect("override on the LOD model");
        let server = world.resource::<AssetServer>();
        assert_eq!(server.path(parts[0].unwrap()), "assets/m.mat");
    }

    #[test]
    fn child_matrix_is_composed() {
        let mut world = World::new();
        let h = cube(&mut world);
        let parent = world
            .spawn(Transform::from_translation(Vec3::new(5.0, 0.0, 0.0)))
            .id();
        world.spawn((Transform::from_translation(Vec3::Y), h, ChildOf(parent)));
        let frame = extract(&mut world);
        assert_eq!(frame.models.len(), 1);
        assert_eq!(translation(frame.models[0].0), Vec3::new(5.0, 1.0, 0.0));
    }

    #[test]
    fn entity_without_model_is_left_out() {
        let mut world = World::new();
        world.spawn(Transform::default());
        assert!(extract(&mut world).models.is_empty());
    }

    #[test]
    fn view_proj_comes_from_camera() {
        let mut world = World::new();
        let camera = Camera::default();
        world.insert_resource(camera);
        assert_eq!(extract(&mut world).view_proj, camera.view_proj());
    }

    #[test]
    fn missing_camera_and_light_give_defaults() {
        let mut world = World::new();
        let frame = extract(&mut world);
        assert_eq!(frame.view_proj, glam::Mat4::default());
        assert_eq!(frame.light.ambient, Light::default().ambient);
        assert_eq!(frame.environment, Environment::default());
    }

    #[test]
    fn environment_comes_from_resource() {
        let mut world = World::new();
        let env = Environment {
            fog_density: 0.5,
            ..Default::default()
        };
        world.insert_resource(env);
        assert_eq!(extract(&mut world).environment, env);
    }

    #[test]
    fn quality_comes_from_resource_or_default() {
        let mut world = World::new();
        assert_eq!(extract(&mut world).quality, RenderQuality::default());
        let low = RenderQuality::preset(melon_core::QualityPreset::Low);
        world.insert_resource(low);
        assert_eq!(extract(&mut world).quality, low);
    }

    #[test]
    fn blends_while_playing_and_paused() {
        assert!(blends(SimulationState::Playing) && blends(SimulationState::Paused));
    }

    #[test]
    fn no_blend_while_editing() {
        assert!(!blends(SimulationState::Editing));
    }
}
