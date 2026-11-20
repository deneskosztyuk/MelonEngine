//! `WorldEnvironment`: a registered component (saved in scenes, edited in
//! the Inspector) that points at a `.env` asset. While an entity carries
//! one, the asset drives the global `Light` and `Environment` resources,
//! like Godot's WorldEnvironment. Without one (or with an empty reference)
//! both resources hold their defaults.

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::reflect::ReflectComponent;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use bevy_reflect::{Reflect, std_traits::ReflectDefault};

use crate::assets::{AssetServer, EnvironmentFile};
use crate::environment_asset::{AssetRef, EnvironmentAsset};
use crate::{Environment, Light, Name, Texture};

/// Points at the `.env` asset that sets the scene's look. The first entity
/// with one (Hierarchy order) wins.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component, Default)]
pub struct WorldEnvironment {
    pub environment: AssetRef,
}

/// An in-progress, unsaved edit of a `.env` file (project-relative path and
/// the edited asset). The system prefers it over the file while the winner
/// resolves to that path. Set by the editor.
#[derive(Resource, Clone, Debug, Default)]
pub struct EnvironmentPreview(pub Option<(String, EnvironmentAsset)>);

/// What the system last applied.
#[derive(Resource, Default)]
struct Cache {
    applied: bool,
    handle: Option<u32>,
    version: u32,
    asset: Option<EnvironmentAsset>,
    failed: Option<(u32, u32)>,
    carriers: (usize, Option<Entity>),
}

/// Every carrier: those in the Hierarchy (roots in `RootOrder`, then children
/// depth first) first, then any others (no `Name` / `Transform`) by entity
/// index.
fn carriers(world: &mut World) -> Vec<Entity> {
    let mut found: Vec<Entity> = crate::hierarchy::depth_first(world)
        .into_iter()
        .map(|(e, _)| e)
        .filter(|&e| world.get::<WorldEnvironment>(e).is_some())
        .collect();
    let mut rest: Vec<Entity> = world
        .query_filtered::<Entity, bevy_ecs::query::With<WorldEnvironment>>()
        .iter(world)
        .filter(|e| !found.contains(e))
        .collect();
    rest.sort_by_key(|e| e.index());
    found.extend(rest);
    found
}

/// The entity whose `WorldEnvironment` applies.
pub fn active_environment(world: &mut World) -> Option<Entity> {
    carriers(world).first().copied()
}

/// Copy the winner's `.env` asset into `Light` and `Environment`; with no
/// winner (after one was applied), restore the defaults. Resources are
/// written only when a value changes.
pub fn world_environment_system(world: &mut World) {
    let all = carriers(world);
    let winner = all.first().copied();
    let mut cache = world.remove_resource::<Cache>().unwrap_or_default();
    if (all.len(), winner) != cache.carriers {
        cache.carriers = (all.len(), winner);
        if let Some(w) = winner.filter(|_| all.len() > 1) {
            let name = world.get::<Name>(w).map_or("?", |n| n.0.as_str());
            tracing::warn!("{} World Environments; {name} wins", all.len());
        }
    }
    let applied = cache.applied;
    let result = match winner {
        Some(w) => {
            let reference = world
                .get::<WorldEnvironment>(w)
                .cloned()
                .unwrap_or_default();
            let preview = world
                .get_resource::<EnvironmentPreview>()
                .and_then(|p| p.0.clone());
            world.resource_scope(
                |_, mut server: bevy_ecs::change_detection::Mut<AssetServer>| {
                    let asset =
                        resolve_asset(&mut server, &reference.environment, preview, &mut cache);
                    let texture = asset.sky.texture.resolve::<Texture>(&mut server);
                    (asset.light(), asset.environment(texture))
                },
            )
        }
        None if applied => {
            cache = Cache::default();
            (Light::default(), Environment::default())
        }
        None => {
            world.insert_resource(cache);
            return;
        }
    };
    cache.applied = winner.is_some();
    world.insert_resource(cache);
    let (light, environment) = result;
    if world.get_resource::<Light>() != Some(&light) {
        world.insert_resource(light);
    }
    if world.get_resource::<Environment>() != Some(&environment) {
        world.insert_resource(environment);
    }
}

/// The asset to apply: the preview for its path, else the file (re-read when
/// its version changes; a broken file keeps the last good copy), else the
/// defaults.
fn resolve_asset(
    server: &mut AssetServer,
    reference: &AssetRef,
    preview: Option<(String, EnvironmentAsset)>,
    cache: &mut Cache,
) -> EnvironmentAsset {
    let Some(handle) = reference.resolve::<EnvironmentFile>(server) else {
        cache.handle = None;
        cache.asset = None;
        return EnvironmentAsset::default();
    };
    let (id, version) = (handle.id(), server.version(handle));
    if cache.handle != Some(id) {
        cache.asset = None;
    }
    if (cache.handle, cache.version) != (Some(id), version) || cache.asset.is_none() {
        let path = server.path(handle).to_string();
        let parsed = server
            .read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| EnvironmentAsset::parse(&String::from_utf8_lossy(&bytes)));
        match parsed {
            Ok(asset) => {
                cache.asset = Some(asset);
                cache.failed = None;
            }
            Err(error) => {
                if cache.failed != Some((id, version)) {
                    tracing::warn!("{path}: {error}; keeping the last good environment");
                    cache.failed = Some((id, version));
                }
            }
        }
        cache.handle = Some(id);
        cache.version = version;
    }
    match preview {
        Some((path, asset)) if path == server.path(handle) => asset,
        _ => cache.asset.clone().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::RootOrder;
    use crate::{AssetGuid, AssetMeta, Core, PostUpdate, Transform};
    use bevy_ecs::change_detection::DetectChanges;
    use bevy_ecs::schedule::Schedule;
    use glam::Vec3;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    fn run(world: &mut World) {
        let mut schedule = Schedule::new(PostUpdate);
        schedule.add_systems(world_environment_system);
        schedule.run(world);
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    /// A temp project with a `Core` whose server reads it.
    fn project(name: &str) -> (PathBuf, Core) {
        let root =
            std::env::temp_dir().join(format!("melon_worldenv_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        let mut core = Core::new();
        core.world.insert_resource(AssetServer::new(root.clone()));
        (root, core)
    }

    fn write(root: &Path, file: &str, text: &str) {
        std::fs::write(root.join(file), text).unwrap();
    }

    fn rescan(core: &mut Core) {
        core.world.resource_mut::<AssetServer>().scan_metas();
    }

    fn carrier(world: &mut World, name: &str, path: &str) -> Entity {
        world
            .spawn((
                Name(name.into()),
                Transform::default(),
                WorldEnvironment {
                    environment: AssetRef {
                        guid: String::new(),
                        path: path.into(),
                    },
                },
            ))
            .id()
    }

    fn fog(world: &World) -> f32 {
        world.resource::<Environment>().fog_density
    }

    #[test]
    fn defaults_reproduce_todays_light_and_environment() {
        let (root, mut core) = project("defaults");
        write(&root, "assets/a.env", "()");
        carrier(&mut core.world, "Env", "assets/a.env");
        run(&mut core.world);
        let light = *core.world.resource::<Light>();
        let want = Light::default();
        assert!(close(light.direction, want.direction), "{light:?}");
        assert_eq!((light.ambient, light.color), (want.ambient, want.color));
        assert_eq!(
            *core.world.resource::<Environment>(),
            Environment::default()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn asset_values_drive_the_resources_including_ao_and_gi() {
        let (root, mut core) = project("values");
        write(
            &root,
            "assets/a.env",
            "(sun: (elevation: 90.0, color: (1.0, 0.5, 0.25)), ambient: (share: 0.4), \
             fog: (density: 0.003), ao: (strength: 0.3), gi: (intensity: 0.5))",
        );
        carrier(&mut core.world, "Env", "assets/a.env");
        run(&mut core.world);
        let light = *core.world.resource::<Light>();
        assert!(close(light.direction, Vec3::NEG_Y), "{light:?}");
        assert_eq!(light.color, Vec3::new(1.0, 0.5, 0.25));
        assert_eq!(light.ambient, 0.4);
        let env = *core.world.resource::<Environment>();
        assert_eq!(env.fog_density, 0.003);
        assert_eq!((env.ao_strength, env.gi_intensity), (0.3, 0.5));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn azimuth_turns_the_sun_around_y() {
        let mut a = EnvironmentAsset::default();
        a.sun.elevation = 0.0;
        a.sun.azimuth = 90.0;
        // The sun sits toward +X, so its light travels toward -X.
        assert!(close(a.light().direction, Vec3::NEG_X));
    }

    #[test]
    fn removing_the_component_restores_the_defaults() {
        let (root, mut core) = project("remove");
        write(
            &root,
            "assets/a.env",
            "(fog: (density: 0.05), sun: (elevation: 5.0))",
        );
        let e = carrier(&mut core.world, "Env", "assets/a.env");
        run(&mut core.world);
        assert_eq!(fog(&core.world), 0.05);
        core.world.despawn(e);
        run(&mut core.world);
        assert_eq!(fog(&core.world), Environment::default().fog_density);
        let want = Light::default();
        assert!(close(
            core.world.resource::<Light>().direction,
            want.direction
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unchanged_asset_does_not_rewrite_the_resources() {
        let (root, mut core) = project("unchanged");
        write(&root, "assets/a.env", "(fog: (density: 0.02))");
        carrier(&mut core.world, "Env", "assets/a.env");
        run(&mut core.world);
        let tick = core.world.change_tick();
        core.world.increment_change_tick();
        run(&mut core.world);
        let env = core
            .world
            .get_resource_ref::<Environment>()
            .expect("environment");
        assert!(
            !env.last_changed()
                .is_newer_than(tick, core.world.change_tick()),
            "rewritten although nothing changed"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn world_environment_is_a_registered_component() {
        let core = Core::new();
        let registry = core.world.resource::<bevy_ecs::reflect::AppTypeRegistry>();
        assert!(
            registry
                .read()
                .get(std::any::TypeId::of::<WorldEnvironment>())
                .is_some()
        );
    }

    #[test]
    fn empty_ref_keeps_todays_defaults() {
        let mut core = Core::new();
        core.world.spawn((
            Name("Env".into()),
            Transform::default(),
            WorldEnvironment::default(),
        ));
        run(&mut core.world);
        assert_eq!(
            *core.world.resource::<Environment>(),
            Environment::default()
        );
        let (l, want) = (*core.world.resource::<Light>(), Light::default());
        assert!(close(l.direction, want.direction));
        assert_eq!((l.ambient, l.color), (want.ambient, want.color));
    }

    #[test]
    fn file_edit_reapplies() {
        let (root, mut core) = project("edit");
        let file = root.join("assets/a.env");
        write(&root, "assets/a.env", "(fog: (density: 0.02))");
        carrier(&mut core.world, "Env", "assets/a.env");
        run(&mut core.world);
        assert_eq!(fog(&core.world), 0.02);
        write(&root, "assets/a.env", "(fog: (density: 0.04))");
        let f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(2))
            .unwrap();
        core.world.resource_mut::<AssetServer>().poll_changes();
        run(&mut core.world);
        assert_eq!(fog(&core.world), 0.04);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn broken_file_keeps_the_last_good_asset() {
        let (root, mut core) = project("broken");
        let file = root.join("assets/a.env");
        write(&root, "assets/a.env", "(fog: (density: 0.02))");
        carrier(&mut core.world, "Env", "assets/a.env");
        run(&mut core.world);
        write(&root, "assets/a.env", "(fog: ");
        let f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(2))
            .unwrap();
        core.world.resource_mut::<AssetServer>().poll_changes();
        run(&mut core.world);
        assert_eq!(fog(&core.world), 0.02);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_file_uses_defaults() {
        let (root, mut core) = project("missing");
        carrier(&mut core.world, "Env", "assets/absent.env");
        run(&mut core.world);
        assert_eq!(
            *core.world.resource::<Environment>(),
            Environment::default()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn first_in_hierarchy_order_wins() {
        let (root, mut core) = project("order");
        write(&root, "assets/a.env", "(fog: (density: 0.1))");
        write(&root, "assets/b.env", "(fog: (density: 0.2))");
        let a = carrier(&mut core.world, "A", "assets/a.env");
        let b = carrier(&mut core.world, "B", "assets/b.env");
        core.world.insert_resource(RootOrder(vec![b, a]));
        run(&mut core.world);
        assert_eq!(fog(&core.world), 0.2);
        assert_eq!(active_environment(&mut core.world), Some(b));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ref_resolves_by_guid_after_a_rename() {
        let (root, mut core) = project("guid");
        write(&root, "assets/moved.env", "(fog: (density: 0.07))");
        let guid = AssetGuid::new();
        write(
            &root,
            "assets/moved.env.meta",
            &AssetMeta {
                version: crate::META_VERSION,
                guid,
            }
            .to_ron(),
        );
        rescan(&mut core);
        core.world.spawn((
            Name("Env".into()),
            Transform::default(),
            WorldEnvironment {
                environment: AssetRef {
                    guid: guid.to_string(),
                    path: "assets/old_name.env".into(),
                },
            },
        ));
        run(&mut core.world);
        assert_eq!(fog(&core.world), 0.07);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_overrides_the_file_for_its_path() {
        let (root, mut core) = project("preview");
        write(&root, "assets/a.env", "(fog: (density: 0.02))");
        carrier(&mut core.world, "Env", "assets/a.env");
        let mut edited = EnvironmentAsset::default();
        edited.fog.density = 0.3;
        core.world.insert_resource(EnvironmentPreview(Some((
            "assets/a.env".into(),
            edited.clone(),
        ))));
        run(&mut core.world);
        assert_eq!(fog(&core.world), 0.3);
        core.world.insert_resource(EnvironmentPreview(Some((
            "assets/other.env".into(),
            edited,
        ))));
        run(&mut core.world);
        assert_eq!(fog(&core.world), 0.02, "another path's preview is ignored");
        core.world.insert_resource(EnvironmentPreview(None));
        run(&mut core.world);
        assert_eq!(fog(&core.world), 0.02);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn carrier_without_transform_still_applies() {
        let (root, mut core) = project("bare");
        write(&root, "assets/a.env", "(fog: (density: 0.02))");
        write(&root, "assets/b.env", "(fog: (density: 0.2))");
        let bare = core
            .world
            .spawn(WorldEnvironment {
                environment: AssetRef {
                    guid: String::new(),
                    path: "assets/a.env".into(),
                },
            })
            .id();
        run(&mut core.world);
        assert_eq!(fog(&core.world), 0.02);
        assert_eq!(active_environment(&mut core.world), Some(bare));
        let listed = carrier(&mut core.world, "B", "assets/b.env");
        run(&mut core.world);
        assert_eq!(active_environment(&mut core.world), Some(listed));
        assert_eq!(fog(&core.world), 0.2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sky_texture_ref_reaches_the_environment() {
        let (root, mut core) = project("sky");
        write(
            &root,
            "assets/a.env",
            "(sky: (texture: (path: \"assets/sky.hdr\")))",
        );
        carrier(&mut core.world, "Env", "assets/a.env");
        run(&mut core.world);
        let want = core
            .world
            .resource_mut::<AssetServer>()
            .load::<Texture>("assets/sky.hdr");
        assert_eq!(core.world.resource::<Environment>().sky_texture, Some(want));
        let _ = std::fs::remove_dir_all(&root);
    }
}
