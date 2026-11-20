use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::assets::split_mesh_fragment;
use crate::bevy_reflect::serde::{TypedReflectDeserializer, TypedReflectSerializer};
use crate::bevy_reflect::{FromReflect, PartialReflect, ReflectFromReflect, TypeRegistry};
use crate::glam::Vec3;
use crate::hierarchy::{depth_first, world_matrix};
use crate::{
    Aabb, AssetGuid, AssetServer, Bob, BodyInfo, BodyKind, ChildOf, Collider, ColliderShape,
    Handle, Model, Name, NameCounter, RigidBody, RigidBodyKind, Spin, Transform,
};
use anyhow::{Context, Result, anyhow, bail};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::query::{With, Without};
use bevy_ecs::reflect::{AppTypeRegistry, ReflectComponent};
use bevy_ecs::world::World;
use ron::value::RawValue;
use serde::de::DeserializeSeed;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable id of a scene entity. Assigned when first needed (`ensure_ids`,
/// editor `capture`) and saved with the scene.
#[derive(Component, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EditorId(pub Uuid);

/// Give every scene entity an `EditorId`.
pub fn ensure_ids(world: &mut World) {
    let missing: Vec<Entity> = world
        .query_filtered::<Entity, (With<Transform>, Without<EditorId>)>()
        .iter(world)
        .collect();
    for e in missing {
        world.entity_mut(e).insert(EditorId(Uuid::new_v4()));
    }
}

/// 2: entities store a model path (`model`) instead of `mesh_ref`.
/// 3: entities store a `parent` index; `transform` is local to the parent.
/// 4: entities also store `model_guid`; it wins over `model` when it resolves.
/// 5: components are a map from type path to RON, through the registry.
/// 6: bodies are `RigidBody` + `Collider` components; version 1-5 `BodyInfo` is converted on load.
pub const SCENE_VERSION: u32 = 6;

/// Version 1 `mesh_ref` values, read so old scenes still load.
#[derive(Deserialize, Clone, Copy)]
pub enum LegacyMeshRef {
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

/// Saved components the registry could not read (type not registered, or
/// its data no longer deserializes), by type path. Written back unchanged.
#[derive(Component, Default, Clone)]
pub struct KeptComponents(pub BTreeMap<String, Box<RawValue>>);

/// A whole scene in memory: what save files hold, and what Play snapshots.
#[derive(Serialize, Deserialize, Clone)]
pub struct SceneData {
    pub version: u32,
    pub entities: Vec<SceneEntity>,
    pub name_counter: u32,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SceneEntity {
    pub name: String,
    /// Index of the parent in `entities` (version 3+).
    #[serde(default)]
    pub parent: Option<u32>,
    /// Model asset path (project-relative, or `builtin:cube`).
    #[serde(default)]
    pub model: Option<String>,
    /// GUID of the model file (version 4+); `model` is the fallback.
    #[serde(default)]
    pub model_guid: Option<AssetGuid>,
    /// Version 5: component RON by type path (`Transform` is local to the parent).
    #[serde(default)]
    pub components: BTreeMap<String, Box<RawValue>>,
    /// Stable editor id (`EditorId`), so undo steps and Play find the entity again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<uuid::Uuid>,
    // Versions 1-4, read only: converted into `components` on load.
    #[serde(default, skip_serializing, deserialize_with = "present")]
    pub transform: Option<Transform>,
    #[serde(default, skip_serializing)]
    pub mesh_ref: Option<LegacyMeshRef>,
    #[serde(default, skip_serializing)]
    pub aabb: Option<Aabb>,
    #[serde(default, skip_serializing)]
    pub body_info: Option<BodyInfo>,
    #[serde(default, skip_serializing)]
    pub spin: Option<Spin>,
    #[serde(default, skip_serializing)]
    pub bob: Option<Bob>,
}

/// A field written without `Some(..)` (versions 1-4 `transform`).
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

/// A component's RON text as the registry writes it.
pub fn component_ron(value: &dyn PartialReflect, registry: &TypeRegistry) -> Result<Box<RawValue>> {
    let config = ron::ser::PrettyConfig::new()
        .compact_structs(true)
        .compact_maps(true)
        .compact_arrays(true);
    let text = ron::ser::to_string_pretty(&TypedReflectSerializer::new(value, registry), config)
        .map_err(|e| anyhow!(e))?;
    RawValue::from_boxed_ron(text.into_boxed_str()).map_err(|e| anyhow!(e))
}

/// Read one saved component: `Ok(None)` if `path` is not a registered component.
fn decode(
    registry: &TypeRegistry,
    path: &str,
    raw: &RawValue,
) -> Result<Option<Box<dyn PartialReflect>>> {
    let Some(registration) = registry
        .get_with_type_path(path)
        .filter(|r| r.data::<ReflectComponent>().is_some())
    else {
        return Ok(None);
    };
    let mut de = ron::Deserializer::from_str(raw.get_ron())?;
    let value = TypedReflectDeserializer::new(registration, registry).deserialize(&mut de)?;
    de.end()?;
    // Build the concrete type now, so bad data fails here and not in `insert`.
    let value = match registration.data::<ReflectFromReflect>() {
        Some(from) => from
            .from_reflect(value.as_partial_reflect())
            .context("data does not match the type")?
            .into_partial_reflect(),
        None => value,
    };
    Ok(Some(value))
}

/// Type path version 5 saved `BodyInfo` under.
pub const BODY_INFO_PATH: &str = "melon_core::components::BodyInfo";

/// Read a version 5 `BodyInfo` (no longer a registered component).
fn read_body_info(raw: &RawValue) -> Result<BodyInfo> {
    let mut registry = TypeRegistry::default();
    registry.register::<BodyInfo>();
    let registration = registry
        .get(std::any::TypeId::of::<BodyInfo>())
        .context("BodyInfo registration")?;
    let mut de = ron::Deserializer::from_str(raw.get_ron())?;
    let value = TypedReflectDeserializer::new(registration, &registry).deserialize(&mut de)?;
    de.end()?;
    BodyInfo::from_reflect(value.as_ref()).context("`BodyInfo` data does not match the type")
}

/// A version 1-5 body as `RigidBody` + `Collider` (a box), saved like any
/// other component. `scale` is the entity scale. A body of kind `None` was no body at all.
fn convert_body_info(
    info: BodyInfo,
    entity: &SceneEntity,
    registry: &TypeRegistry,
    saved: &mut BTreeMap<String, Box<RawValue>>,
) -> Result<Option<Vec3>> {
    let he = info.half_extents;
    if he.x <= 0.0 || he.y <= 0.0 || he.z <= 0.0 {
        bail!(
            "entity '{}' has non-positive half-extents: {he:?}",
            entity.name
        );
    }
    let kind = match info.body_type {
        BodyKind::None => return Ok(None),
        BodyKind::Dynamic => RigidBodyKind::Dynamic,
        BodyKind::Fixed => RigidBodyKind::Fixed,
    };
    let body = RigidBody {
        kind,
        ..Default::default()
    };
    let collider = Collider {
        // World units for now: `apply_scene` divides by the world scale once
        // the parents are in place.
        shape: ColliderShape::Box { half_extents: he },
        ..Default::default()
    };
    for value in [&body as &dyn PartialReflect, &collider] {
        if let std::collections::btree_map::Entry::Vacant(slot) =
            saved.entry(value.reflect_type_path().to_string())
        {
            slot.insert(component_ron(value, registry)?);
        }
    }
    Ok(Some(he))
}

/// Insert a value `decode` returned.
fn insert(world: &mut World, registry: &TypeRegistry, id: Entity, value: &dyn PartialReflect) {
    let type_id = value
        .get_represented_type_info()
        .expect("decoded values have a type")
        .type_id();
    let component = registry
        .get_type_data::<ReflectComponent>(type_id)
        .expect("decoded from a component registration");
    component.insert(&mut world.entity_mut(id), value, registry);
}

/// One entity's components, read and ready to insert.
struct Decoded {
    /// World-unit half extents of a converted `BodyInfo` box.
    legacy_half: Option<Vec3>,
    components: Vec<Box<dyn PartialReflect>>,
    kept: BTreeMap<String, Box<RawValue>>,
}

impl Decoded {
    fn get<T: 'static>(&self) -> Option<&T> {
        self.components
            .iter()
            .find_map(|c| c.try_downcast_ref::<T>())
    }
}

/// Read an entity's components (versions 1-4 converted first). Unknown or
/// unreadable ones are kept, with a warning once per type path per load.
fn decode_entity(
    entity: &SceneEntity,
    registry: &TypeRegistry,
    warned: &mut HashSet<String>,
) -> Result<Decoded> {
    let mut saved = entity.components.clone();
    let mut legacy = |value: &dyn PartialReflect| -> Result<()> {
        let path = value.reflect_type_path().to_string();
        saved.insert(path, component_ron(value, registry)?);
        Ok(())
    };
    if let Some(t) = entity.transform {
        legacy(&t)?;
    }
    if let Some(a) = entity.aabb {
        legacy(&a)?;
    }
    if let Some(s) = entity.spin {
        legacy(&s)?;
    }
    if let Some(b) = entity.bob {
        legacy(&b)?;
    }
    // Versions 1-4 stored `body_info` as a field, version 5 as a component.
    let body_info = match (entity.body_info, saved.remove(BODY_INFO_PATH)) {
        (Some(info), _) => Some(info),
        (None, Some(raw)) => Some(read_body_info(&raw.trim_boxed())?),
        (None, None) => None,
    };
    let mut legacy_half = None;
    if let Some(info) = body_info {
        legacy_half = convert_body_info(info, entity, registry, &mut saved)?;
    }

    let mut decoded = Decoded {
        legacy_half,
        components: Vec::new(),
        kept: BTreeMap::new(),
    };
    for (path, raw) in saved {
        let raw = raw.trim_boxed();
        match decode(registry, &path, &raw) {
            Ok(Some(value)) => decoded.components.push(value),
            result => {
                if warned.insert(path.clone()) {
                    match result {
                        Err(error) => tracing::warn!(
                            "entity '{}': component `{path}` could not be read ({error:#}); kept as is",
                            entity.name
                        ),
                        _ => tracing::warn!(
                            "entity '{}': component `{path}` is not registered; kept as is",
                            entity.name
                        ),
                    }
                }
                decoded.kept.insert(path, raw);
            }
        }
    }
    Ok(decoded)
}

/// The world's type registry (`Core::new` adds it). Missing is an error,
/// not a panic: a save or load fails instead of closing the editor.
fn registry_of(world: &World) -> Result<AppTypeRegistry> {
    world
        .get_resource::<AppTypeRegistry>()
        .cloned()
        .context("the world has no AppTypeRegistry (Core::new adds it)")
}

/// Capture all entities with Name + Transform, except ones marked for deletion
/// (deleted this frame but not yet despawned). Depth-first, so parents come
/// before their children and sibling order is kept.
pub fn scene_to_data(world: &mut World) -> Result<SceneData> {
    ensure_ids(world);
    let rows: Vec<Entity> = depth_first(world).into_iter().map(|(e, _)| e).collect();
    let world: &World = world;
    let index: std::collections::HashMap<Entity, u32> = rows
        .iter()
        .enumerate()
        .map(|(i, e)| (*e, i as u32))
        .collect();
    let registry = registry_of(world)?;
    let registry = registry.read();

    let mut entities = Vec::with_capacity(rows.len());
    for &e in &rows {
        let parent = world
            .get::<ChildOf>(e)
            .and_then(|c| index.get(&c.parent()).copied());
        entities.push(capture_entity(world, &registry, e, parent)?);
    }

    let name_counter = world
        .get_resource::<NameCounter>()
        .map(|c| c.0)
        .unwrap_or(0);

    Ok(SceneData {
        version: SCENE_VERSION,
        entities,
        name_counter,
    })
}

/// One entity as the scene file holds it (`parent` is the caller's index).
pub fn capture_entity(
    world: &World,
    registry: &TypeRegistry,
    e: Entity,
    parent: Option<u32>,
) -> Result<SceneEntity> {
    let server = world.get_resource::<AssetServer>();
    let name = world
        .get::<Name>(e)
        .map(|n| n.0.clone())
        .unwrap_or_default();
    let mut components = BTreeMap::new();
    for registration in registry.iter() {
        let Some(component) = registration.data::<ReflectComponent>() else {
            continue;
        };
        let Some(value) = component.reflect(world.entity(e)) else {
            continue;
        };
        let path = registration.type_info().type_path();
        let raw = component_ron(value.as_partial_reflect(), registry)
            .with_context(|| format!("entity '{name}': cannot save `{path}`"))?;
        components.insert(path.to_string(), raw);
    }
    if let Some(kept) = world.get::<KeptComponents>(e) {
        for (path, raw) in &kept.0 {
            components
                .entry(path.clone())
                .or_insert_with(|| raw.clone());
        }
    }
    Ok(SceneEntity {
        name,
        parent,
        model: world
            .get::<Handle<Model>>(e)
            .zip(server)
            .map(|(h, s)| s.path(*h).to_string()),
        model_guid: world
            .get::<Handle<Model>>(e)
            .zip(server)
            .and_then(|(h, s)| s.guid_of(s.path(*h))),
        components,
        uid: world.get::<EditorId>(e).map(|i| i.0),
        transform: None,
        mesh_ref: None,
        aabb: None,
        body_info: None,
        spin: None,
        bob: None,
    })
}

/// Replace the scene in `world` with `data`.
/// Two-phase: validate all entities before mutating the world.
/// Despawns all existing scene entities, then spawns from `data`. Physics bodies
/// come from the physics sync systems afterwards (an owner of a `PhysicsWorld`
/// clears it before and syncs after).
pub fn apply_scene(world: &mut World, data: &SceneData) -> Result<()> {
    // Phase 1: Validate (no world mutation)
    if !(1..=SCENE_VERSION).contains(&data.version) {
        bail!(
            "unsupported scene version: expected {}, got {}",
            SCENE_VERSION,
            data.version
        );
    }

    let registry = registry_of(world)?;
    let registry = registry.read();
    let mut warned = HashSet::new();
    let mut decoded = Vec::with_capacity(data.entities.len());
    for (i, entity) in data.entities.iter().enumerate() {
        let d = decode_entity(entity, &registry, &mut warned)?;
        validate_entity(entity, &d, i)?;
        decoded.push(d);
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

    // Spawn everything, attach parents, then create bodies from world poses
    // (which need the parents in place).
    let legacy_half: Vec<Option<Vec3>> = decoded.iter().map(|d| d.legacy_half).collect();
    let ids: Vec<Entity> = data
        .entities
        .iter()
        .zip(decoded)
        .map(|(entity, d)| spawn_scene_entity(world, &registry, entity, d))
        .collect();
    for (entity, &id) in data.entities.iter().zip(&ids) {
        if let Some(p) = entity.parent {
            world.entity_mut(id).insert(ChildOf(ids[p as usize]));
        }
    }
    // Converted `BodyInfo` boxes: world-unit extents become local ones under
    // the entity's world scale (its parents' included).
    for (&id, half) in ids.iter().zip(&legacy_half) {
        if let Some(half) = half {
            let scale = world_matrix(world, id).to_scale_rotation_translation().0;
            if let Some(mut c) = world.get_mut::<Collider>(id) {
                c.shape = ColliderShape::Box {
                    half_extents: *half / scale.abs().max(Vec3::splat(1e-6)),
                };
            }
        }
    }
    // A hand-copied block can repeat a uid; steps would find the wrong entity.
    let mut seen = HashSet::new();
    let mut regenerated = 0;
    for &id in &ids {
        if let Some(uid) = world.get::<EditorId>(id).map(|i| i.0)
            && !seen.insert(uid)
        {
            let fresh = uuid::Uuid::new_v4();
            world.entity_mut(id).insert(EditorId(fresh));
            regenerated += 1;
        }
    }
    if regenerated > 0 {
        tracing::warn!("{regenerated} entities shared an id with another; gave them new ones");
    }
    let roots: Vec<Entity> = data
        .entities
        .iter()
        .zip(&ids)
        .filter(|(e, _)| e.parent.is_none())
        .map(|(_, &id)| id)
        .collect();
    world.insert_resource(crate::hierarchy::RootOrder(roots));
    Ok(())
}

/// Insert kept components whose type is now registered and whose data
/// deserializes (after a game plugin (re)registers its types). Returns how
/// many were inserted.
pub fn resolve_kept_components(world: &mut World) -> usize {
    let Some(registry) = world.get_resource::<AppTypeRegistry>().cloned() else {
        return 0;
    };
    let registry = registry.read();
    let holders: Vec<Entity> = world
        .query_filtered::<Entity, With<KeptComponents>>()
        .iter(world)
        .collect();
    let mut inserted = 0;
    for id in holders {
        let Some(mut kept) = world.entity_mut(id).take::<KeptComponents>() else {
            continue;
        };
        kept.0
            .retain(|path, raw| match decode(&registry, path, raw) {
                Ok(Some(value)) => {
                    insert(world, &registry, id, value.as_ref());
                    inserted += 1;
                    false
                }
                _ => true,
            });
        if !kept.0.is_empty() {
            world.entity_mut(id).insert(kept);
        }
    }
    inserted
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
    let data = scene_to_data(world)?;

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

/// Read and parse a scene file (no world change).
pub fn read_scene(path: &Path) -> Result<SceneData> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read scene file: {}", path.display()))?;
    ron::from_str(&contents).context("failed to deserialize scene from RON")
}

/// Load a scene from a RON file (see `apply_scene`).
pub fn load_scene(world: &mut World, path: &Path) -> Result<()> {
    let data = read_scene(path)?;
    apply_scene(world, &data)?;

    tracing::info!(
        "loaded scene from {}: {} entities",
        path.display(),
        data.entities.len()
    );
    Ok(())
}

fn validate_entity(entity: &SceneEntity, decoded: &Decoded, index: usize) -> Result<()> {
    let name = &entity.name;
    let transform = decoded.get::<Transform>().copied().unwrap_or_default();

    // Validate scale is non-zero
    let scale = transform.scale;
    if scale.x.abs() < 1e-6 || scale.y.abs() < 1e-6 || scale.z.abs() < 1e-6 {
        bail!("entity '{name}' (index {index}) has zero or near-zero scale: {scale:?}");
    }

    // Validate finite values
    if !scale.is_finite()
        || !transform.translation.is_finite()
        || !transform.rotation.to_array().iter().all(|v| v.is_finite())
    {
        bail!("entity '{name}' (index {index}) has non-finite transform values");
    }

    Ok(())
}

/// Spawn one entity without its physics objects (`sync_components` makes them). Every scene
/// entity gets a `Transform` (default if the file has none it can read).
fn spawn_scene_entity(
    world: &mut World,
    registry: &TypeRegistry,
    entity: &SceneEntity,
    decoded: Decoded,
) -> Entity {
    let id = world.spawn_empty().id();
    fill_scene_entity(world, registry, id, entity, decoded);
    id
}

/// Give `id` the name, model, editor id and components of `entity`.
fn fill_scene_entity(
    world: &mut World,
    registry: &TypeRegistry,
    id: Entity,
    entity: &SceneEntity,
    decoded: Decoded,
) {
    world
        .entity_mut(id)
        .insert((Name(entity.name.clone()), Transform::default()));
    if let Some(uid) = entity.uid {
        world.entity_mut(id).insert(EditorId(uid));
    }

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

    for value in &decoded.components {
        insert(world, registry, id, value.as_ref());
    }
    if !decoded.kept.is_empty() {
        world.entity_mut(id).insert(KeptComponents(decoded.kept));
    }
}

/// Write `entity` into the world (undo / redo): over `existing` when given
/// (its components, model and kept data are replaced; its parent, children
/// and body are left to the caller), else as a new entity. No body is made.
pub fn restore_entity(
    world: &mut World,
    existing: Option<Entity>,
    entity: &SceneEntity,
) -> Result<Entity> {
    let registry = registry_of(world)?;
    let registry = registry.read();
    let decoded = decode_entity(entity, &registry, &mut HashSet::new())?;
    let id = match existing {
        Some(id) => {
            for registration in registry.iter() {
                if let Some(component) = registration.data::<ReflectComponent>() {
                    component.remove(&mut world.entity_mut(id));
                }
            }
            world
                .entity_mut(id)
                .remove::<(KeptComponents, Handle<Model>)>();
            id
        }
        None => world.spawn_empty().id(),
    };
    fill_scene_entity(world, &registry, id, entity, decoded);
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bevy_reflect::{self, Reflect, std_traits::ReflectDefault};
    use crate::glam::{Quat, Vec3};
    use bevy_ecs::world::World;

    fn world_with_entities() -> World {
        let mut world = reg_world();
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
            .query::<(&Name, &crate::Handle<crate::Model>)>()
            .iter(world)
            .find(|(n, _)| n.0 == name)
            .map(|(_, h)| h)
            .unwrap();
        world
            .resource::<crate::AssetServer>()
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
        let mut world = reg_world();
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
                crate::ChildOf(car),
            ));
        }
        world
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
    fn v3_roundtrip_keeps_parents_local_transforms_and_order() {
        let mut world = car_world();
        let data = scene_to_data(&mut world).unwrap();
        apply_scene(&mut world, &data).unwrap();
        let car = named(&mut world, "Car");
        let children: Vec<String> = world
            .get::<crate::Children>(car)
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
        let mut world = reg_world();
        apply_scene(&mut world, &data).unwrap();
        assert_eq!(world.query::<&crate::ChildOf>().iter(&world).count(), 0);
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
            let err = apply_scene(&mut world, &data).unwrap_err();
            let first = &data.entities[0].name;
            assert!(err.to_string().contains(first.as_str()), "{err}");
            assert_eq!(world.query::<&Name>().iter(&world).count(), 3);
        }
    }
    #[test]
    fn reapplying_scene_with_children_does_not_panic() {
        let mut world = car_world();
        let data = scene_to_data(&mut world).unwrap();
        apply_scene(&mut world, &data).unwrap();
        apply_scene(&mut world, &data).unwrap();
        assert_eq!(world.query::<&Name>().iter(&world).count(), 3);
    }
    #[test]
    fn v2_roundtrip_keeps_model_paths() {
        let mut world = world_with_entities();
        let data = scene_to_data(&mut world).unwrap();
        apply_scene(&mut world, &data).unwrap();
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
        let mut world = reg_world();
        apply_scene(&mut world, &data).unwrap();
        assert_eq!(model_path_of(&mut world, "Old"), "builtin:cube");
        assert_eq!(
            model_path_of(&mut world, "OldModel"),
            "assets/models/cube.glb"
        );
    }
    #[test]
    fn missing_model_path_still_loads() {
        let mut world = world_with_entities();
        let h: crate::Handle<crate::Model> = world
            .resource_mut::<crate::AssetServer>()
            .load("assets/models/does_not_exist.glb");
        world.spawn((Name("Ghost".to_string()), Transform::default(), h));
        let data = scene_to_data(&mut world).unwrap();
        apply_scene(&mut world, &data).unwrap();
        assert_eq!(
            model_path_of(&mut world, "Ghost"),
            "assets/models/does_not_exist.glb"
        );
    }
    #[test]
    fn save_load_roundtrip() {
        let mut world = world_with_entities();

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
        load_scene(&mut world, &tmp).expect("load should succeed");

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

        let mut world = reg_world();
        world.insert_resource(NameCounter(0));

        let result = load_scene(&mut world, &tmp);
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

        let mut world = reg_world();
        world.insert_resource(NameCounter(0));

        let result = load_scene(&mut world, &tmp);
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

        let mut world = reg_world();
        world.insert_resource(NameCounter(0));

        let result = load_scene(&mut world, &tmp);
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

        // Save the 2-entity scene
        let tmp = std::env::temp_dir().join("melon_test_clear.ron");
        save_scene(&mut world, &tmp).unwrap();

        // Add a 3rd entity
        world.spawn((Name("Extra".to_string()), Transform::default()));
        assert_eq!(world.query::<&Name>().iter(&world).count(), 3);

        // Load — should clear all and have only 2
        load_scene(&mut world, &tmp).unwrap();
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
        let data = scene_to_data(&mut world).unwrap();
        let floor = world
            .query::<(Entity, &Name)>()
            .iter(&world)
            .find(|(_, n)| n.0 == "Floor")
            .map(|(e, _)| e)
            .unwrap();
        world.get_mut::<Transform>(floor).unwrap().translation.y = 5.0;
        apply_scene(&mut world, &data).unwrap();
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
        world.entity_mut(floor).insert(crate::PendingDelete);
        let data = scene_to_data(&mut world).unwrap();
        apply_scene(&mut world, &data).unwrap();
        let names: Vec<String> = world
            .query::<&Name>()
            .iter(&world)
            .map(|n| n.0.clone())
            .collect();
        assert_eq!(names, ["Cube"]);
    }
    #[test]
    fn apply_scene_invalid_data_leaves_world_untouched() {
        let mut bad = reg_world();
        bad.spawn((
            Name("Flat".to_string()),
            Transform {
                scale: Vec3::ZERO,
                ..Default::default()
            },
        ));
        let data = scene_to_data(&mut bad).unwrap();
        let mut world = world_with_entities();
        assert!(apply_scene(&mut world, &data).is_err());
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
    fn read_log_interval_system(_interval: bevy_ecs::system::Res<crate::LogInterval>) {}
    #[test]
    fn load_keeps_world_so_systems_and_resources_survive() {
        let mut world = world_with_entities();
        world.insert_resource(crate::LogInterval { frames_per_log: 7 });
        // Systems bind to the World they first run on; a load must keep that World.
        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(read_log_interval_system);
        schedule.run(&mut world);
        let world_id = world.id();

        let tmp = std::env::temp_dir().join("melon_test_keep_world.ron");
        save_scene(&mut world, &tmp).unwrap();
        load_scene(&mut world, &tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        assert_eq!(world.id(), world_id, "load must not swap the World");
        assert_eq!(
            world.resource::<crate::LogInterval>().frames_per_log,
            7,
            "resources must survive a load"
        );
        schedule.run(&mut world); // panicked with "mismatched World" before the fix
        assert_eq!(world.query::<&Name>().iter(&world).count(), 2);
    }
    /// A temp project with `assets/models/a.glb` and its `.meta`, and a world
    /// whose `AssetServer` has scanned it.
    fn guid_world(name: &str) -> (World, std::path::PathBuf, crate::AssetGuid) {
        let root = std::env::temp_dir().join(format!("melon_scene_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let file = root.join("assets/models/a.glb");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();
        let guid = crate::AssetGuid::new();
        let meta = crate::AssetMeta {
            version: crate::META_VERSION,
            guid,
        };
        std::fs::write(crate::meta_path(&file), meta.to_ron()).unwrap();
        let mut server = AssetServer::new(root.clone());
        server.scan_metas();
        let mut world = reg_world();
        world.insert_resource(server);
        (world, root, guid)
    }
    fn one_entity_scene(model: &str, guid: Option<crate::AssetGuid>) -> SceneData {
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
        let data = scene_to_data(&mut world).unwrap();
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
        apply_scene(&mut world, &data).unwrap();
        assert_eq!(model_path_of(&mut world, "A"), "assets/models/a.glb#mesh1");
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn v4_unknown_guid_falls_back_to_path() {
        let (mut world, root, _) = guid_world("v4_unknown");
        let data = one_entity_scene("assets/models/a.glb", Some(crate::AssetGuid::new()));
        apply_scene(&mut world, &data).unwrap();
        assert_eq!(model_path_of(&mut world, "A"), "assets/models/a.glb");
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn v4_path_only_entity_loads() {
        let (mut world, root, _) = guid_world("v4_path_only");
        let ron = r#"(version: 4, entities: [(name: "A", transform: (translation: (0.0, 0.0, 0.0), rotation: (0.0, 0.0, 0.0, 1.0), scale: (1.0, 1.0, 1.0)), model: Some("assets/models/a.glb"))], name_counter: 0)"#;
        let data: SceneData = ron::from_str(ron).unwrap();
        apply_scene(&mut world, &data).unwrap();
        assert_eq!(model_path_of(&mut world, "A"), "assets/models/a.glb");
        let _ = std::fs::remove_dir_all(&root);
    }
    /// Checks the two-entity scene the v3 and v4 fixtures hold.
    fn assert_fixture_scene(data: &SceneData, version: u32) {
        assert_eq!(data.version, version);
        assert_eq!(data.name_counter, 3);
        let [floor, cube] = &data.entities[..] else {
            panic!("expected 2 entities");
        };
        assert_eq!(floor.name, "Floor");
        assert_eq!(
            floor.transform.unwrap().translation,
            Vec3::new(0.0, -1.0, 0.0)
        );
        assert_eq!(floor.transform.unwrap().rotation, Quat::IDENTITY);
        assert_eq!(floor.transform.unwrap().scale, Vec3::ONE);
        assert_eq!(floor.parent, None);
        assert_eq!(floor.model.as_deref(), Some("builtin:cube"));
        assert_eq!(floor.model_guid, None);
        assert_eq!(floor.aabb.unwrap().half_extents, Vec3::new(10.0, 0.5, 10.0));
        let body = floor.body_info.unwrap();
        assert_eq!(body.body_type, BodyKind::Fixed);
        assert_eq!(body.half_extents, Vec3::new(10.0, 0.5, 10.0));
        assert!(floor.spin.is_none() && floor.bob.is_none());
        assert_eq!(cube.name, "Cube");
        assert_eq!(
            cube.transform.unwrap().translation,
            Vec3::new(0.0, 2.0, 0.0)
        );
        assert_eq!(cube.parent, Some(0));
        assert_eq!(cube.model.as_deref(), Some("assets/models/cube.glb"));
        assert!(cube.aabb.is_none() && cube.body_info.is_none());
        assert!(cube.spin.is_some() && cube.bob.is_some());
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
    #[test]
    fn v3_scene_fixture_loads() {
        let ron = V3_FIXTURE;
        let data: SceneData = ron::from_str(ron).unwrap();
        assert_fixture_scene(&data, 3);
        assert_eq!(data.entities[1].model_guid, None);
    }
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
    #[test]
    fn v4_scene_fixture_loads() {
        let ron = V4_FIXTURE;
        let data: SceneData = ron::from_str(ron).unwrap();
        assert_fixture_scene(&data, 4);
        assert_eq!(
            data.entities[1].model_guid,
            Some("334087e7-ba37-4ff4-b11f-bc731349dbfa".parse().unwrap())
        );
    }
    /// A world with the engine's component registry (as `Core` builds it).
    fn reg_world() -> World {
        crate::Core::new().world
    }
    /// Save to RON text, parse it back and apply it to the same world.
    fn round_trip(world: &mut World) -> String {
        let data = scene_to_data(world).unwrap();
        let text = ron::ser::to_string_pretty(&data, ron::ser::PrettyConfig::default()).unwrap();
        let back: SceneData = ron::from_str(&text).unwrap();
        apply_scene(world, &back).unwrap();
        text
    }
    fn load_text(world: &mut World, text: &str) {
        let data: SceneData = ron::from_str(text).unwrap();
        apply_scene(world, &data).unwrap();
    }
    #[derive(bevy_ecs::component::Component, Reflect, Default, Debug, PartialEq)]
    #[reflect(Component, Default)]
    struct TestHealth {
        hp: f32,
    }
    const TEST_HEALTH: &str = "melon_core::scene::tests::TestHealth";
    #[test]
    fn audio_source_and_listener_round_trip() {
        use crate::{AudioListener, AudioSource};
        let mut world = reg_world();
        let source = AudioSource {
            clip: "assets/audio/door.ogg".into(),
            volume: 0.5,
            pitch: 1.5,
            looping: true,
            play_on_start: true,
            spatial: true,
            min_distance: 2.0,
            max_distance: 40.0,
            bus: "Music".into(),
        };
        world.spawn((
            Name("Speaker".to_string()),
            Transform::default(),
            source.clone(),
            AudioListener,
        ));
        round_trip(&mut world);
        let e = named(&mut world, "Speaker");
        assert_eq!(world.get::<AudioSource>(e), Some(&source));
        assert!(world.get::<AudioListener>(e).is_some());
    }
    #[test]
    fn runtime_registered_component_round_trips() {
        let mut core = crate::Core::new();
        core.register_component::<TestHealth>();
        let mut world = core.world;
        world.spawn((
            Name("Hero".to_string()),
            Transform::default(),
            TestHealth { hp: 42.5 },
        ));
        let text = round_trip(&mut world);
        assert!(text.contains(TEST_HEALTH), "{text}");
        let hero = named(&mut world, "Hero");
        assert_eq!(
            world.get::<TestHealth>(hero),
            Some(&TestHealth { hp: 42.5 })
        );
    }
    fn v5_text(components: &str) -> String {
        format!(
            r#"(version: 5, entities: [(name: "A", parent: None, model: None, model_guid: None, components: {{ {components} }})], name_counter: 0)"#
        )
    }
    #[test]
    fn kept_component_round_trips_byte_for_byte() {
        let mut world = reg_world();
        let unknown = "(hp: 3, kind: Fire, tags: [\"a\",  \"b\"], nested: Some((x: 1)))";
        load_text(
            &mut world,
            &v5_text(&format!(
                r#""game::Unknown":   {unknown} , "melon_core::components::Spin": ()"#
            )),
        );
        let a = named(&mut world, "A");
        assert!(world.get::<Spin>(a).is_some());
        let kept = world.get::<KeptComponents>(a).unwrap();
        assert_eq!(kept.0["game::Unknown"].get_ron(), unknown);

        let text = round_trip(&mut world);
        assert!(
            text.contains(&format!("\"game::Unknown\": {unknown},")),
            "{text}"
        );
        let a = named(&mut world, "A");
        assert_eq!(
            world.get::<KeptComponents>(a).unwrap().0["game::Unknown"].get_ron(),
            unknown
        );
    }
    #[test]
    fn undeserializable_known_component_is_kept() {
        let mut world = reg_world();
        load_text(
            &mut world,
            &v5_text(
                r#""melon_core::components::Spin": "oops", "melon_core::components::Aabb": (half_extents: (x: 1.0, y: 2.0, z: 3.0))"#,
            ),
        );
        let a = named(&mut world, "A");
        assert!(world.get::<Spin>(a).is_none());
        assert_eq!(
            world.get::<Aabb>(a).unwrap().half_extents,
            Vec3::new(1.0, 2.0, 3.0)
        );
        assert!(world.get::<Transform>(a).is_some());
        let kept = world.get::<KeptComponents>(a).unwrap();
        assert_eq!(kept.0["melon_core::components::Spin"].get_ron(), "\"oops\"");
        let text = round_trip(&mut world);
        assert!(
            text.contains(r#""melon_core::components::Spin": "oops""#),
            "{text}"
        );
    }
    #[test]
    fn kept_component_inserted_when_type_registers() {
        let mut core = crate::Core::new();
        load_text(
            &mut core.world,
            &v5_text(&format!(r#""{TEST_HEALTH}": (hp: 7.0), "game::Other": ()"#)),
        );
        let a = named(&mut core.world, "A");
        assert!(
            core.world
                .get::<KeptComponents>(a)
                .unwrap()
                .0
                .contains_key(TEST_HEALTH)
        );
        assert_eq!(resolve_kept_components(&mut core.world), 0);

        core.register_component::<TestHealth>();
        assert_eq!(resolve_kept_components(&mut core.world), 1);
        assert_eq!(
            core.world.get::<TestHealth>(a),
            Some(&TestHealth { hp: 7.0 })
        );
        let kept = core.world.get::<KeptComponents>(a).unwrap();
        assert_eq!(kept.0.keys().collect::<Vec<_>>(), ["game::Other"]);
        assert_eq!(resolve_kept_components(&mut core.world), 0);
    }
    #[test]
    fn missing_type_registry_is_an_error_not_a_panic() {
        let mut world = World::new();
        assert!(scene_to_data(&mut world).is_err());
        let data = scene_to_data(&mut crate::Core::new().world).unwrap();
        let mut bare = World::new();
        assert!(apply_scene(&mut bare, &data).is_err());
    }
    #[test]
    fn duplicate_uids_in_a_file_are_regenerated() {
        let mut world = reg_world();
        world.spawn((Name("A".into()), Transform::default()));
        world.spawn((Name("B".into()), Transform::default()));
        let mut data = scene_to_data(&mut world).unwrap();
        data.entities[1].uid = data.entities[0].uid;
        apply_scene(&mut world, &data).unwrap();
        let ids: HashSet<_> = world
            .query::<&EditorId>()
            .iter(&world)
            .map(|i| i.0)
            .collect();
        assert_eq!(ids.len(), 2);
    }
}
