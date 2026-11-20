//! GPU materials per `Handle<Material>` (`.mat` files) and the textures they
//! use: loaded on first use, rebuilt when the material file or one of its
//! textures changes on disk (the `AssetServer` bumps the file's version). A
//! missing or broken texture binds the 1x1 default, so the factors apply
//! alone; a broken `.mat` keeps its last good version, or leaves the part
//! with its model's own material.

use std::collections::{HashMap, HashSet};

use glam::Vec3;
use melon_core::material::AlphaMode;
use melon_core::{AssetServer, Handle, Material, MaterialAsset, Texture};

use crate::{MaterialUniform, SceneRenderer, Surface};

/// base colour and emissive are colour data (sRGB), the others linear.
const SRGB: [bool; 4] = [true, false, false, true];

struct CachedTexture {
    version: u32,
    /// `None`: the file is missing or broken (reported once per version).
    texture: Option<wgpu::Texture>,
}

struct CachedMaterial {
    version: u32,
    /// Texture versions this build used.
    deps: Vec<(Handle<Texture>, u32)>,
    group: wgpu::BindGroup,
    surface: Surface,
}

#[derive(Default)]
pub struct MaterialCache {
    textures: HashMap<(u32, bool), CachedTexture>,
    materials: HashMap<u32, CachedMaterial>,
    failed: HashSet<(u32, u32)>,
    /// Bind groups built so far (tests watch hot reload through it).
    pub builds: u32,
}

impl MaterialCache {
    /// The loaded texture of `handle` used as `srgb`, if it has loaded.
    pub fn texture(&self, handle: Handle<Texture>, srgb: bool) -> Option<&wgpu::Texture> {
        self.textures.get(&(handle.id(), srgb))?.texture.as_ref()
    }

    /// Bind group and surface of `handle`, if it has loaded.
    pub fn get(&self, handle: Handle<Material>) -> Option<(&wgpu::BindGroup, Surface)> {
        self.materials
            .get(&handle.id())
            .map(|m| (&m.group, m.surface))
    }

    /// Whether `handle` is built.
    pub fn contains(&self, handle: Handle<Material>) -> bool {
        self.materials.contains_key(&handle.id())
    }

    /// Free `handle` and the textures no other material uses.
    pub fn evict(&mut self, handle: Handle<Material>) {
        self.materials.remove(&handle.id());
        self.failed.retain(|(id, _)| *id != handle.id());
        self.free_unused_textures();
    }

    /// Free every material not in `used` (material ids) and the textures
    /// that leaves unused.
    pub fn retain_used(&mut self, used: HashSet<u32>) {
        let before = self.materials.len();
        self.materials.retain(|id, _| used.contains(id));
        self.failed.retain(|(id, _)| used.contains(id));
        if self.materials.len() != before {
            self.free_unused_textures();
        }
    }

    fn free_unused_textures(&mut self) {
        let live: HashSet<u32> = self
            .materials
            .values()
            .flat_map(|m| m.deps.iter().map(|(h, _)| h.id()))
            .collect();
        self.textures.retain(|(id, _), _| live.contains(id));
    }

    /// Built (materials, textures).
    #[cfg(test)]
    pub fn counts(&self) -> (usize, usize) {
        (self.materials.len(), self.textures.len())
    }

    fn load_texture(
        &mut self,
        scene: &SceneRenderer,
        server: &AssetServer,
        handle: Handle<Texture>,
        srgb: bool,
    ) {
        let version = server.version(handle);
        let key = (handle.id(), srgb);
        if self
            .textures
            .get(&key)
            .is_some_and(|t| t.version == version)
        {
            return;
        }
        // `read` serves the pack in a shipped game, the project folder otherwise.
        let path = server.path(handle).to_string();
        let texture = match server
            .read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(image::load_from_memory(&bytes)?))
        {
            Ok(img) => Some(scene.create_texture_from_image_as(&img, srgb)),
            Err(error) => {
                tracing::warn!("could not load texture {path}: {error}");
                None
            }
        };
        self.textures
            .insert(key, CachedTexture { version, texture });
    }

    /// Load `handle` on first use or after its file or one of its textures
    /// changed. On a load error the previous version stays in use; the error
    /// is logged once per file version.
    pub fn prepare(
        &mut self,
        scene: &SceneRenderer,
        server: &mut AssetServer,
        handle: Handle<Material>,
    ) {
        let id = handle.id();
        let version = server.version(handle);
        let current = self.materials.get(&id).is_some_and(|m| {
            m.version == version && m.deps.iter().all(|(h, v)| server.version(*h) == *v)
        });
        if current || self.failed.contains(&(id, version)) {
            return;
        }
        let path = server.path(handle).to_string();
        let asset = match server
            .read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| MaterialAsset::parse(&String::from_utf8(bytes)?))
        {
            Ok(asset) => asset,
            Err(error) => {
                tracing::warn!("could not load material {path}: {error:#}");
                self.failed.insert((id, version));
                return;
            }
        };
        let handles = asset.texture_handles(server);
        let mut deps = Vec::new();
        for (i, texture) in handles.iter().enumerate() {
            if let Some(h) = texture {
                self.load_texture(scene, server, *h, SRGB[i]);
                deps.push((*h, server.version(*h)));
            }
        }
        let textures: Vec<Option<&wgpu::Texture>> = handles
            .iter()
            .enumerate()
            .map(|(i, h)| h.and_then(|h| self.texture(h, SRGB[i])))
            .collect();
        let factors = MaterialUniform {
            base_color: asset.base_color,
            emissive: asset.emissive.map(|c| c * asset.emissive_strength),
            metallic: asset.metallic,
            roughness: asset.roughness,
            normal_scale: asset.normal_scale,
            alpha_cutoff: match asset.alpha_mode {
                // 0 means "no mask", so a cutoff of 0 is the smallest one.
                AlphaMode::Mask { cutoff } => cutoff.max(1e-4),
                _ => 0.0,
            },
            _pad: 0.0,
        };
        let group = scene.create_material_bind_group(&factors, [0, 1, 2, 3].map(|i| textures[i]));
        let surface = Surface {
            blend: asset.alpha_mode == AlphaMode::Blend,
            double_sided: asset.double_sided,
            masked: matches!(asset.alpha_mode, AlphaMode::Mask { .. }),
        };
        self.builds += 1;
        self.materials.insert(
            id,
            CachedMaterial {
                version,
                deps,
                group,
                surface,
            },
        );
    }
}

/// Blend draws ordered far to near from `eye`: each item is its world
/// position and a payload; the farthest comes first (ties keep input order).
pub fn sort_back_to_front<T>(eye: Vec3, items: &mut [(Vec3, T)]) {
    items.sort_by(|a, b| {
        b.0.distance_squared(eye)
            .total_cmp(&a.0.distance_squared(eye))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_draws_sort_far_to_near() {
        let mut items = [
            (Vec3::new(0.0, 0.0, -2.0), "near"),
            (Vec3::new(0.0, 0.0, -9.0), "far"),
            (Vec3::new(0.0, 0.0, -5.0), "mid"),
            (Vec3::new(0.0, 0.0, -5.0), "mid2"),
        ];
        sort_back_to_front(Vec3::ZERO, &mut items);
        let order: Vec<_> = items.iter().map(|i| i.1).collect();
        assert_eq!(order, ["far", "mid", "mid2", "near"]);
    }
}
