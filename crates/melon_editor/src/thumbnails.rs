//! Which Content Browser model tiles need a thumbnail, and the egui textures
//! already made. The host renders at most one per frame
//! (`melon_render::Renderer::render_thumbnail`).

use std::collections::HashMap;

use melon_core::{AssetServer, Handle, Material, Model};

/// What a thumbnail shows: a model, or a material on a sphere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThumbnailSubject {
    Model(Handle<Model>),
    Material(Handle<Material>),
}

/// One model or material to render, and the texture id to update if it had one.
#[derive(Clone, Debug, PartialEq)]
pub struct ThumbnailRequest {
    /// Project-relative path of the model or `.mat` file.
    pub path: String,
    pub subject: ThumbnailSubject,
    /// The model file's `AssetServer` version this render is for.
    pub version: u32,
    pub existing: Option<egui::TextureId>,
}

#[derive(Default)]
pub struct Thumbnails {
    /// Per model: the version last rendered (or failed), and its texture.
    /// `None` = no thumbnail (failed, and no older one to keep).
    done: HashMap<String, (u32, Option<egui::TextureId>)>,
    /// Model tiles on screen this frame, in drawing order.
    wanted: Vec<String>,
}

impl Thumbnails {
    /// Forget last frame's tiles (the view calls `want` for each tile drawn).
    pub fn begin_frame(&mut self) {
        self.wanted.clear();
    }

    pub fn want(&mut self, path: String) {
        if !self.wanted.contains(&path) {
            self.wanted.push(path);
        }
    }

    pub fn texture(&self, path: &str) -> Option<egui::TextureId> {
        self.done.get(path).and_then(|(_, id)| *id)
    }

    /// The first tile on screen without a thumbnail for its file's current
    /// version (a failed version is not retried).
    pub fn request(&self, server: &mut AssetServer) -> Option<ThumbnailRequest> {
        self.wanted.iter().find_map(|path| {
            let (subject, version) = if path.to_lowercase().ends_with(".mat") {
                let handle: Handle<Material> = server.load(path);
                (ThumbnailSubject::Material(handle), server.version(handle))
            } else {
                let handle: Handle<Model> = server.load(path);
                (ThumbnailSubject::Model(handle), server.version(handle))
            };
            let done = self.done.get(path);
            (done.map(|(v, _)| *v) != Some(version)).then(|| ThumbnailRequest {
                path: path.clone(),
                subject,
                version,
                existing: done.and_then(|(_, id)| *id),
            })
        })
    }

    /// Record a render: `Some(id)` on success; `None` keeps an older
    /// thumbnail if there is one, and stops retries until the file changes.
    pub fn finish(&mut self, request: &ThumbnailRequest, id: Option<egui::TextureId>) {
        self.done.insert(
            request.path.clone(),
            (request.version, id.or(request.existing)),
        );
    }

    #[cfg(test)]
    pub(crate) fn wanted(&self) -> &[String] {
        &self.wanted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::AssetServer;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    fn project(name: &str) -> (PathBuf, AssetServer) {
        let root = std::env::temp_dir().join(format!("melon_thumbs_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        for file in ["a.glb", "b.glb", "c.glb"] {
            std::fs::write(root.join("assets").join(file), b"x").unwrap();
        }
        let server = AssetServer::new(root.clone());
        (root, server)
    }

    /// Give the file a later modified time and let the server notice.
    fn bump(root: &Path, server: &mut AssetServer, rel: &str) {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(root.join(rel))
            .unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        server.poll_changes();
    }

    const A: &str = "assets/a.glb";
    const B: &str = "assets/b.glb";

    #[test]
    fn material_files_get_a_material_thumbnail_request() {
        let (root, mut server) = project("material");
        std::fs::write(root.join("assets/m.mat"), "()").unwrap();
        let mut thumbs = Thumbnails::default();
        thumbs.want("assets/m.mat".into());
        let request = thumbs.request(&mut server).unwrap();
        assert!(matches!(request.subject, ThumbnailSubject::Material(_)));
        thumbs.want(A.into());
        thumbs.finish(&request, Some(egui::TextureId::User(7)));
        let next = thumbs.request(&mut server).unwrap();
        assert!(matches!(next.subject, ThumbnailSubject::Model(_)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn requests_only_wanted_paths_one_at_a_time() {
        let (root, mut server) = project("wanted");
        let mut thumbs = Thumbnails::default();
        thumbs.want(A.into());
        thumbs.want(B.into());
        thumbs.want(A.into());
        let first = thumbs.request(&mut server).unwrap();
        assert_eq!(first.path, A);
        thumbs.finish(&first, Some(egui::TextureId::User(1)));
        let second = thumbs.request(&mut server).unwrap();
        assert_eq!(second.path, B);
        thumbs.finish(&second, Some(egui::TextureId::User(2)));
        assert!(thumbs.request(&mut server).is_none());
        assert_eq!(thumbs.texture(A), Some(egui::TextureId::User(1)));
        assert_eq!(thumbs.texture("assets/c.glb"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn version_change_requests_again_with_existing_id() {
        let (root, mut server) = project("version");
        let mut thumbs = Thumbnails::default();
        thumbs.want(A.into());
        let first = thumbs.request(&mut server).unwrap();
        thumbs.finish(&first, Some(egui::TextureId::User(1)));
        bump(&root, &mut server, A);
        let again = thumbs.request(&mut server).unwrap();
        assert_eq!(again.path, A);
        assert_eq!(again.existing, Some(egui::TextureId::User(1)));
        assert_eq!(again.version, first.version + 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_model_not_retried_until_version_changes() {
        let (root, mut server) = project("failed");
        let mut thumbs = Thumbnails::default();
        thumbs.want(A.into());
        let first = thumbs.request(&mut server).unwrap();
        thumbs.finish(&first, None);
        assert!(thumbs.request(&mut server).is_none());
        assert_eq!(thumbs.texture(A), None);
        bump(&root, &mut server, A);
        assert!(thumbs.request(&mut server).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_rerender_keeps_the_last_good_thumbnail() {
        let (root, mut server) = project("keep_good");
        let mut thumbs = Thumbnails::default();
        thumbs.want(A.into());
        let first = thumbs.request(&mut server).unwrap();
        thumbs.finish(&first, Some(egui::TextureId::User(1)));
        bump(&root, &mut server, A);
        let second = thumbs.request(&mut server).unwrap();
        thumbs.finish(&second, None);
        assert_eq!(thumbs.texture(A), Some(egui::TextureId::User(1)));
        assert!(thumbs.request(&mut server).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn begin_frame_forgets_tiles_no_longer_shown() {
        let (root, mut server) = project("begin");
        let mut thumbs = Thumbnails::default();
        thumbs.want(A.into());
        thumbs.begin_frame();
        assert!(thumbs.request(&mut server).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
