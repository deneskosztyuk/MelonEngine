//! Component reflection: remote wrappers for glam 0.33 (bevy_reflect only
//! supports glam 0.32) and the registry of saved, editable components.

use std::any::TypeId;

use bevy_ecs::{
    component::Component,
    reflect::{AppTypeRegistry, ReflectComponent},
    resource::Resource,
    world::World,
};
use bevy_reflect::{
    FromType, GetTypeRegistration, Reflect, TypePath, reflect_remote, std_traits::ReflectDefault,
};

#[reflect_remote(glam::Vec2)]
pub struct Vec2Reflect {
    pub x: f32,
    pub y: f32,
}

#[reflect_remote(glam::Vec3)]
pub struct Vec3Reflect {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

// SIMD storage: fields are reached through glam's `Deref`, and FromReflect
// starts from `Default` and cloning uses `Clone`: no struct literal can
// build the type.
#[reflect_remote(glam::Vec4)]
#[derive(Clone, Default)]
#[reflect(Clone, Default)]
pub struct Vec4Reflect {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

#[reflect_remote(glam::Quat)]
#[derive(Clone, Default)]
#[reflect(Clone, Default)]
pub struct QuatReflect {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

/// An enum that knows usable payloads for its variants. The Inspector asks
/// for one when the user switches variant (a plain default would give a
/// sphere of radius 0). Register with `#[reflect(VariantDefaults)]`.
pub trait VariantDefaults: Sized {
    /// The enum set to variant `name` with useful field values.
    fn variant_default(name: &str) -> Option<Self>;
}

/// Type data of `VariantDefaults`.
#[derive(Clone)]
pub struct ReflectVariantDefaults {
    variant_default: fn(&str) -> Option<Box<dyn Reflect>>,
}

impl ReflectVariantDefaults {
    pub fn variant_default(&self, name: &str) -> Option<Box<dyn Reflect>> {
        (self.variant_default)(name)
    }
}

impl<T: Reflect + VariantDefaults> FromType<T> for ReflectVariantDefaults {
    fn from_type() -> Self {
        Self {
            variant_default: |name| {
                T::variant_default(name).map(|v| Box::new(v) as Box<dyn Reflect>)
            },
        }
    }
}

/// Bumped on every component registration, so views built from the
/// registry (Add Component list) know to rebuild.
#[derive(Resource, Default)]
pub struct RegistryGeneration(pub u64);

/// A registered component as the editor lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentInfo {
    pub type_path: &'static str,
    pub display_name: String,
    /// Has a default value, so Add Component can create it.
    pub addable: bool,
}

/// Register `T` (with its `ReflectComponent`) in the world's
/// `AppTypeRegistry`, replacing an earlier registration of the same type.
/// No layout check: `App::register_component` does that.
pub(crate) fn register<T>(world: &mut World)
where
    T: Component + Reflect + TypePath + GetTypeRegistration,
{
    world.register_component::<T>();
    let mut registration = T::get_type_registration();
    registration.insert(<ReflectComponent as FromType<T>>::from_type());
    let registry = world.resource::<AppTypeRegistry>().clone();
    {
        let mut registry = registry.write();
        T::register_type_dependencies(&mut registry);
        // Not `overwrite_registration`: it would mark the short name ambiguous.
        match registry.get_mut(TypeId::of::<T>()) {
            Some(existing) => *existing = registration,
            None => {
                registry.add_registration(registration);
            }
        }
    }
    world.resource_mut::<RegistryGeneration>().0 += 1;
}

/// Registered components (those with `ReflectComponent`), by display name.
pub fn registered_components(world: &World) -> Vec<ComponentInfo> {
    let registry = world.resource::<AppTypeRegistry>().read();
    let mut infos: Vec<ComponentInfo> = registry
        .iter()
        .filter(|r| r.data::<ReflectComponent>().is_some())
        .map(|r| {
            let table = r.type_info().type_path_table();
            ComponentInfo {
                type_path: table.path(),
                display_name: display_name(table.short_path()),
                addable: r.data::<ReflectDefault>().is_some(),
            }
        })
        .collect();
    infos.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    infos
}

/// `BodyInfo` -> "Body Info", `AABBCollider` -> "AABB Collider".
pub fn display_name(short_name: &str) -> String {
    let chars: Vec<char> = short_name.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_uppercase() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
                out.push(' ');
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{App, Core, Transform};
    use bevy_ecs::component::Component;
    use bevy_ecs::reflect::AppTypeRegistry;
    use bevy_reflect::serde::{
        ReflectSerializer, TypedReflectDeserializer, TypedReflectSerializer,
    };
    use bevy_reflect::{FromReflect, PartialReflect, Reflect, ReflectRef};
    use glam::{Quat, Vec3};
    use serde::de::DeserializeSeed;
    use std::alloc::Layout;

    fn generation(core: &Core) -> u64 {
        core.world.resource::<RegistryGeneration>().0
    }

    #[test]
    fn engine_components_are_registered() {
        let core = Core::new();
        let names: Vec<String> = registered_components(&core.world)
            .into_iter()
            .map(|c| c.display_name)
            .collect();
        assert_eq!(
            names,
            [
                "Aabb",
                "Audio Listener",
                "Audio Source",
                "Bob",
                "Character Controller",
                "Collider",
                "Joint",
                "Lod",
                "Mesh Materials",
                "Point Light",
                "Rigid Body",
                "Scene Lighting",
                "Spin",
                "Spot Light",
                "Transform"
            ]
        );
    }

    #[test]
    fn glam_fields_reflect_by_name() {
        let core = Core::new();
        let t = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let ReflectRef::Struct(s) = t.reflect_ref() else {
            panic!("Transform is not a struct");
        };
        let ReflectRef::Struct(translation) = s.field("translation").unwrap().reflect_ref() else {
            panic!("translation is not a struct");
        };
        let names: Vec<&str> = (0..translation.field_len())
            .map(|i| translation.name_at(i).unwrap())
            .collect();
        assert_eq!(names, ["x", "y", "z"]);

        let registry = core.world.resource::<AppTypeRegistry>().read();
        let ron = ron::to_string(&ReflectSerializer::new(&t, &registry)).unwrap();
        assert!(ron.contains("x:"), "{ron}");
        let typed = ron::to_string(&TypedReflectSerializer::new(&t, &registry)).unwrap();
        assert_eq!(
            typed,
            concat!(
                "(translation:(x:1.0,y:2.0,z:3.0),",
                "rotation:(x:0.0,y:0.0,z:0.0,w:1.0),",
                "scale:(x:1.0,y:1.0,z:1.0))"
            )
        );
    }

    #[test]
    fn quat_and_vec4_round_trip_through_reflect_serde() {
        let core = Core::new();
        let t = Transform {
            translation: Vec3::new(1.0, -2.0, 3.5),
            rotation: Quat::from_rotation_y(0.7) * Quat::from_rotation_x(0.2),
            scale: Vec3::new(2.0, 1.0, 0.5),
        };
        let registry = core.world.resource::<AppTypeRegistry>().read();
        let ron = ron::to_string(&TypedReflectSerializer::new(&t, &registry)).unwrap();
        let registration = registry.get(std::any::TypeId::of::<Transform>()).unwrap();
        let mut de = ron::Deserializer::from_str(&ron).unwrap();
        let value = TypedReflectDeserializer::new(registration, &registry)
            .deserialize(&mut de)
            .unwrap();
        let back = Transform::from_reflect(value.as_partial_reflect()).unwrap();
        assert_eq!(back.translation, t.translation);
        assert_eq!(back.rotation, t.rotation);
        assert_eq!(back.scale, t.scale);

        let v = Vec4Reflect(glam::Vec4::new(1.0, 2.0, 3.0, 4.0));
        let ron = ron::to_string(&TypedReflectSerializer::new(
            &v,
            &registry_with::<Vec4Reflect>(),
        ))
        .unwrap();
        assert_eq!(ron, "(x:1.0,y:2.0,z:3.0,w:4.0)");
    }

    fn registry_with<T: bevy_reflect::GetTypeRegistration>() -> bevy_reflect::TypeRegistry {
        let mut registry = bevy_reflect::TypeRegistry::new();
        registry.register::<T>();
        registry
    }

    #[derive(Component, Reflect, Default)]
    #[reflect(Component, Default)]
    struct Speed(f32);

    #[test]
    fn reregister_same_layout_refreshes() {
        let mut core = Core::new();
        let before = generation(&core);
        let build = |app: &mut App| {
            app.register_component::<Speed>();
        };
        core.rebuild_plugins(build).unwrap();
        core.rebuild_plugins(build).unwrap();
        assert_eq!(generation(&core), before + 2);
        let speeds = registered_components(&core.world)
            .into_iter()
            .filter(|c| c.display_name == "Speed")
            .count();
        assert_eq!(speeds, 1);
        let registry = core.world.resource::<AppTypeRegistry>().read();
        assert!(registry.get_with_short_type_path("Speed").is_some());
    }

    #[test]
    fn reregister_changed_layout_refused() {
        let mut core = Core::new();
        core.rebuild_plugins(|app| {
            app.register_component::<Speed>();
        })
        .unwrap();
        let before = generation(&core);
        let result = core.rebuild_plugins(|app| {
            app.register_component_with_layout::<Speed>(Layout::new::<[u64; 4]>());
        });
        assert!(result.unwrap_err().contains("restart"));
        assert_eq!(generation(&core), before);
        assert!(
            registered_components(&core.world)
                .iter()
                .any(|c| c.display_name == "Speed")
        );
    }

    #[test]
    fn display_names_split_case() {
        assert_eq!(display_name("BodyInfo"), "Body Info");
        assert_eq!(display_name("Aabb"), "Aabb");
        assert_eq!(display_name("AABBCollider"), "AABB Collider");
    }
}
