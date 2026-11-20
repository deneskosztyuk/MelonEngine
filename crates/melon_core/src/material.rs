//! Materials: `.mat` RON files (`MaterialAsset`) and the component that
//! assigns them to the mesh parts of an entity's model. Textures are
//! referenced by GUID with the path as fallback, like scene model
//! references. No GPU code: `melon_render` builds the bind groups.

use std::path::Path;

use bevy_ecs::component::Component;
use bevy_ecs::reflect::ReflectComponent;
use bevy_reflect::{Reflect, std_traits::ReflectDefault};
use serde::{Deserialize, Serialize};

use crate::assets::{AssetGuid, AssetServer, Handle, Material, Texture};

/// File extension of material assets.
pub const MATERIAL_EXTENSION: &str = "mat";

/// How a material's alpha is used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum AlphaMode {
    #[default]
    Opaque,
    /// Fragments with alpha below `cutoff` are discarded (opaque pass,
    /// prepass and shadows).
    Mask { cutoff: f32 },
    /// Sorted back to front after the opaque pass; casts no shadows.
    Blend,
}

/// A texture by GUID (wins when it resolves) or project-relative path.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TextureRef {
    pub guid: Option<AssetGuid>,
    pub path: String,
}

impl TextureRef {
    /// Names `path`, with its GUID when the server knows it.
    pub fn for_path(server: &AssetServer, path: &str) -> Self {
        Self {
            guid: server.guid_of(path),
            path: path.to_string(),
        }
    }

    pub fn resolve(&self, server: &mut AssetServer) -> Handle<Texture> {
        match self.guid.and_then(|g| server.load_guid(g, None)) {
            Some(handle) => handle,
            None => server.load(&self.path),
        }
    }
}

/// glTF metallic-roughness material as a file. Missing fields take the glTF
/// defaults, so a hand-written file can set only what it needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MaterialAsset {
    /// Linear RGBA factor.
    pub base_color: [f32; 4],
    pub base_color_texture: Option<TextureRef>,
    pub metallic: f32,
    pub roughness: f32,
    /// G = roughness, B = metallic.
    pub metallic_roughness_texture: Option<TextureRef>,
    pub normal_texture: Option<TextureRef>,
    pub normal_scale: f32,
    /// Linear RGB factor.
    pub emissive: [f32; 3],
    pub emissive_texture: Option<TextureRef>,
    /// Multiplies `emissive` (HDR glow above 1).
    pub emissive_strength: f32,
    pub alpha_mode: AlphaMode,
    pub double_sided: bool,
}

impl Default for MaterialAsset {
    fn default() -> Self {
        Self {
            base_color: [1.0; 4],
            base_color_texture: None,
            metallic: 1.0,
            roughness: 1.0,
            metallic_roughness_texture: None,
            normal_texture: None,
            normal_scale: 1.0,
            emissive: [0.0; 3],
            emissive_texture: None,
            emissive_strength: 1.0,
            alpha_mode: AlphaMode::Opaque,
            double_sided: false,
        }
    }
}

impl MaterialAsset {
    /// A plain rough dielectric: what Create > Material writes.
    pub fn new_default() -> Self {
        Self {
            metallic: 0.0,
            roughness: 0.5,
            ..Self::default()
        }
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(ron::from_str(text)?)
    }

    pub fn to_ron(&self) -> String {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .expect("material serialises")
    }

    pub fn read(path: &Path) -> anyhow::Result<Self> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    /// Atomic write.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        crate::ron_file::save(self, path, false)
    }

    /// Texture handles: base colour, metallic-roughness, normal, emissive.
    pub fn texture_handles(&self, server: &mut AssetServer) -> [Option<Handle<Texture>>; 4] {
        [
            &self.base_color_texture,
            &self.metallic_roughness_texture,
            &self.normal_texture,
            &self.emissive_texture,
        ]
        .map(|t| t.as_ref().map(|t| t.resolve(server)))
    }
}

/// One mesh part's material assignment: GUID (wins) or path; both empty =
/// unset (the part keeps its glTF material).
#[derive(Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Default)]
pub struct MaterialSlot {
    pub guid: String,
    pub path: String,
}

impl MaterialSlot {
    pub fn for_path(server: &AssetServer, path: &str) -> Self {
        Self {
            guid: server
                .guid_of(path)
                .map(|g| g.to_string())
                .unwrap_or_default(),
            path: path.to_string(),
        }
    }

    pub fn resolve(&self, server: &mut AssetServer) -> Option<Handle<Material>> {
        if let Some(handle) = self
            .guid
            .parse::<AssetGuid>()
            .ok()
            .and_then(|g| server.load_guid(g, None))
        {
            return Some(handle);
        }
        (!self.path.is_empty()).then(|| server.load(&self.path))
    }
}

/// Materials for the parts of the entity's model, in the model's part
/// order; parts past the list, and unset slots, keep the glTF material.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component, Default)]
pub struct MeshMaterials {
    pub parts: Vec<MaterialSlot>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AssetMeta;

    #[test]
    fn material_file_round_trips() {
        let material = MaterialAsset {
            base_color: [0.5, 0.25, 1.0, 0.5],
            base_color_texture: Some(TextureRef {
                guid: Some("11111111-2222-3333-4444-555555555555".parse().unwrap()),
                path: "assets/textures/a.png".into(),
            }),
            metallic: 0.1,
            roughness: 0.9,
            normal_texture: Some(TextureRef {
                guid: None,
                path: "assets/textures/n.png".into(),
            }),
            normal_scale: 0.5,
            emissive: [1.0, 0.5, 0.0],
            emissive_strength: 4.0,
            alpha_mode: AlphaMode::Mask { cutoff: 0.3 },
            double_sided: true,
            ..MaterialAsset::default()
        };
        let dir = std::env::temp_dir().join(format!("melon_mat_{}", std::process::id()));
        let file = dir.join("m.mat");
        material.save(&file).unwrap();
        assert_eq!(MaterialAsset::read(&file).unwrap(), material);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_fields_take_gltf_defaults() {
        let m = MaterialAsset::parse("(roughness: 0.2)").unwrap();
        assert_eq!(m.roughness, 0.2);
        assert_eq!(m.base_color, [1.0; 4]);
        assert_eq!(m.alpha_mode, AlphaMode::Opaque);
        assert!(MaterialAsset::parse("(roughness: ").is_err());
    }

    #[test]
    fn texture_ref_prefers_guid_and_falls_back_to_path() {
        let root = std::env::temp_dir().join(format!("melon_matref_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/t.png"), b"x").unwrap();
        let guid = AssetGuid::new();
        std::fs::write(
            root.join("assets/t.png.meta"),
            AssetMeta {
                version: crate::META_VERSION,
                guid,
            }
            .to_ron(),
        )
        .unwrap();
        let mut server = AssetServer::new(root.clone());
        server.scan_metas();
        let by_guid = TextureRef {
            guid: Some(guid),
            path: "assets/old_name.png".into(),
        }
        .resolve(&mut server);
        assert_eq!(server.path(by_guid), "assets/t.png");
        let by_path = TextureRef {
            guid: Some(AssetGuid::new()),
            path: "assets/other.png".into(),
        }
        .resolve(&mut server);
        assert_eq!(server.path(by_path), "assets/other.png");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn slot_resolves_path_and_empty_is_unset() {
        let mut server = AssetServer::new(".".into());
        assert!(MaterialSlot::default().resolve(&mut server).is_none());
        let slot = MaterialSlot {
            guid: String::new(),
            path: "assets/m.mat".into(),
        };
        let h = slot.resolve(&mut server).unwrap();
        assert_eq!(server.path(h), "assets/m.mat");
    }
}
