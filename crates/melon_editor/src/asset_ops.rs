//! File operations for the Content Browser. Each moves or copies an asset's
//! `.meta` with it and keeps the `AssetServer` in step. No UI.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use melon_core::assets::project_relative;
use melon_core::{AssetServer, meta_path};

use crate::asset_db::{reconcile, same_ignoring_case};

/// Reject names the file system or the asset walk cannot handle.
pub fn validate_name(name: &str) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        bail!("the name is empty");
    }
    if let Some(c) = name.chars().find(|c| {
        matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
    }) {
        bail!("the name cannot contain '{c}'");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        bail!("the name cannot end with a dot or a space");
    }
    if name.starts_with('.') {
        bail!("the name cannot start with a dot (hidden files are not assets)");
    }
    if name.to_lowercase().ends_with(".meta") {
        bail!("names ending in .meta are reserved for asset ids");
    }
    Ok(())
}

/// Something named `name` is already in `folder` (case-insensitive, as on
/// Windows).
fn name_taken(folder: &Path, name: &str) -> bool {
    std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| same_ignoring_case(&e.file_name().to_string_lossy(), name))
}

/// `folder/name`, or `stem 2.ext`, `stem 3.ext`, ... if that is taken.
pub fn unique_path(folder: &Path, name: &str) -> PathBuf {
    if !name_taken(folder, name) {
        return folder.join(name);
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map_or(name.into(), |s| s.to_string_lossy());
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| format!("{stem} {n}{ext}"))
        .find(|candidate| !name_taken(folder, candidate))
        .map(|candidate| folder.join(candidate))
        .expect("an unused number exists")
}

pub fn create_folder(server: &mut AssetServer, parent: &Path) -> anyhow::Result<PathBuf> {
    let dir = unique_path(parent, "New Folder");
    std::fs::create_dir(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    reconcile(server);
    Ok(dir)
}

/// Write a default `New Material.mat` into `parent` (a free name).
pub fn create_material(server: &mut AssetServer, parent: &Path) -> anyhow::Result<PathBuf> {
    let file = unique_path(parent, "New Material.mat");
    std::fs::write(&file, melon_core::MaterialAsset::new_default().to_ron())
        .with_context(|| format!("could not create {}", file.display()))?;
    reconcile(server);
    Ok(file)
}

/// Write a default `New Environment.env` into `parent` (a free name).
pub fn create_environment(server: &mut AssetServer, parent: &Path) -> anyhow::Result<PathBuf> {
    let file = unique_path(parent, "New Environment.env");
    std::fs::write(&file, melon_core::EnvironmentAsset::default().to_ron())
        .with_context(|| format!("could not create {}", file.display()))?;
    reconcile(server);
    Ok(file)
}

/// The environment a new scene's World Environment points at.
pub const DEFAULT_ENVIRONMENT: &str = "assets/environments/default.env";

/// Write `DEFAULT_ENVIRONMENT` (and its `.meta`) under `root` when it is missing.
pub fn ensure_default_environment(server: &mut AssetServer, root: &Path) -> anyhow::Result<()> {
    let file = root.join(DEFAULT_ENVIRONMENT);
    if file.is_file() {
        return Ok(());
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    std::fs::write(&file, melon_core::EnvironmentAsset::default().to_ron())
        .with_context(|| format!("could not create {}", file.display()))?;
    reconcile(server);
    Ok(())
}

#[cfg(test)]
pub(crate) const ONE_MATERIAL_GLTF: &str = r#"{
    "asset": {"version": "2.0"}, "scene": 0, "scenes": [{"nodes": [0]}],
    "nodes": [{"mesh": 0}],
    "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "material": 0}]}],
    "materials": [{"name": "Paint", "pbrMetallicRoughness": {"baseColorFactor": [0, 1, 0, 1]}}],
    "buffers": [{"byteLength": 36,
        "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAA"}],
    "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}],
    "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3,
        "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]}]
}"#;

/// Unpack the materials of the glTF `model` into `.mat` files (and png
/// textures) next to it, give them `.meta`s and fill their texture GUIDs.
/// Returns the project-relative `.mat` path of each mesh part, in part order.
pub fn extract_materials(server: &mut AssetServer, model: &Path) -> anyhow::Result<Vec<String>> {
    let root = server.root().to_path_buf();
    let rel_dir = model
        .parent()
        .and_then(|dir| project_relative(&root, dir))
        .context("the model is outside the project")?;
    let parts = melon_render::extract_materials(model, &rel_dir)?;
    reconcile(server);
    for rel in parts.iter().collect::<std::collections::BTreeSet<_>>() {
        let file = root.join(rel);
        let mut material = melon_core::MaterialAsset::read(&file)?;
        for texture in [
            &mut material.base_color_texture,
            &mut material.metallic_roughness_texture,
            &mut material.normal_texture,
            &mut material.emissive_texture,
        ]
        .into_iter()
        .flatten()
        {
            *texture = melon_core::TextureRef::for_path(server, &texture.path);
        }
        material.save(&file)?;
    }
    Ok(parts)
}

pub fn rename(server: &mut AssetServer, path: &Path, new_name: &str) -> anyhow::Result<PathBuf> {
    validate_name(new_name)?;
    let (Some(parent), Some(old_name)) = (path.parent(), path.file_name()) else {
        bail!("{} cannot be renamed", path.display());
    };
    let old_name = old_name.to_string_lossy();
    if old_name == new_name {
        return Ok(path.to_path_buf());
    }
    // A case-only rename of the same item is fine; anything else that exists is a clash.
    if !same_ignoring_case(&old_name, new_name) && name_taken(parent, new_name) {
        bail!("'{new_name}' already exists here");
    }
    let target = parent.join(new_name);
    move_item(server, path, &target)?;
    Ok(target)
}

/// Move one item into `folder`; a no-op if it is already there.
pub fn move_to(server: &mut AssetServer, path: &Path, folder: &Path) -> anyhow::Result<PathBuf> {
    if path.parent() == Some(folder) {
        return Ok(path.to_path_buf());
    }
    if folder.starts_with(path) {
        bail!("a folder cannot be moved into itself");
    }
    let Some(name) = path.file_name() else {
        bail!("{} cannot be moved", path.display());
    };
    if name_taken(folder, &name.to_string_lossy()) {
        bail!(
            "'{}' already exists in {}",
            name.to_string_lossy(),
            folder.display()
        );
    }
    let target = folder.join(name);
    move_item(server, path, &target)?;
    Ok(target)
}

/// Rename `from` to `to` on disk with its `.meta`; if the `.meta` cannot
/// follow, the file goes back so the two never separate.
fn move_item(server: &mut AssetServer, from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::rename(from, to)
        .with_context(|| format!("could not move {} to {}", from.display(), to.display()))?;
    let meta = meta_path(from);
    // `from` is gone now; `to` tells whether this was a file.
    if to.is_file()
        && meta.exists()
        && let Err(error) = std::fs::rename(&meta, meta_path(to))
    {
        let _ = std::fs::rename(to, from);
        return Err(anyhow::Error::new(error).context(format!(
            "could not move {}; nothing changed",
            meta.display()
        )));
    }
    rename_prefix(server, from, to);
    reconcile(server);
    Ok(())
}

/// Point the server's paths at the new location: the file itself, or every
/// indexed asset under a renamed folder. `reconcile` would miss a case-only
/// rename on Windows (the old path still "exists").
fn rename_prefix(server: &mut AssetServer, old: &Path, new: &Path) {
    let root = server.root().to_path_buf();
    let (Some(old), Some(new)) = (project_relative(&root, old), project_relative(&root, new))
    else {
        return;
    };
    let inside = format!("{old}/");
    let moved: Vec<String> = server
        .guid_index()
        .map(|(_, p)| p.to_string())
        .filter(|p| p.starts_with(&inside))
        .collect();
    server.rename_path(&old, &new);
    for path in moved {
        server.rename_path(&path, &format!("{new}/{}", &path[inside.len()..]));
    }
}

/// Copy a file (with its `.meta`) or a folder next to itself under a new
/// name; `reconcile` then gives the copies new GUIDs (the server already
/// maps the originals).
pub fn duplicate(server: &mut AssetServer, path: &Path) -> anyhow::Result<PathBuf> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        bail!("{} cannot be duplicated", path.display());
    };
    let target = unique_path(parent, &name.to_string_lossy());
    if path.is_dir() {
        copy_dir(path, &target, false)?;
    } else {
        std::fs::copy(path, &target)
            .with_context(|| format!("could not copy {}", path.display()))?;
        if meta_path(path).exists() {
            let _ = std::fs::copy(meta_path(path), meta_path(&target));
        }
    }
    reconcile(server);
    Ok(target)
}

/// Send an item (and its `.meta`) to the Recycle Bin.
pub fn delete(server: &mut AssetServer, path: &Path) -> anyhow::Result<()> {
    delete_with(server, path, |paths| {
        trash::delete_all(paths).context("could not move to the Recycle Bin")
    })
}

fn delete_with(
    server: &mut AssetServer,
    path: &Path,
    remove: impl FnOnce(&[PathBuf]) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut paths = vec![path.to_path_buf()];
    if path.is_file() && meta_path(path).exists() {
        paths.push(meta_path(path));
    }
    remove(&paths)?;
    reconcile(server);
    Ok(())
}

/// Copy files or folders from outside into `folder` (new names on clash).
/// `.meta` files are not copied: imports get fresh GUIDs.
pub fn import(
    server: &mut AssetServer,
    files: &[PathBuf],
    folder: &Path,
) -> Vec<anyhow::Result<PathBuf>> {
    let results = files
        .iter()
        .filter(|f| !is_meta(f))
        .map(|source| {
            let Some(name) = source.file_name() else {
                bail!("{} has no file name", source.display());
            };
            if folder.starts_with(source) {
                bail!("a folder cannot be copied into itself");
            }
            let target = unique_path(folder, &name.to_string_lossy());
            if source.is_dir() {
                copy_dir(source, &target, true)?;
            } else {
                std::fs::copy(source, &target)
                    .with_context(|| format!("could not copy {}", source.display()))?;
            }
            Ok(target)
        })
        .collect();
    reconcile(server);
    results
}

fn is_meta(path: &Path) -> bool {
    path.to_string_lossy().to_lowercase().ends_with(".meta")
}

fn copy_dir(from: &Path, to: &Path, skip_meta: bool) -> anyhow::Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("could not create {}", to.display()))?;
    for entry in std::fs::read_dir(from)?.flatten() {
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            copy_dir(&source, &target, skip_meta)?;
        } else if !(skip_meta && is_meta(&source)) {
            std::fs::copy(&source, &target)
                .with_context(|| format!("could not copy {}", source.display()))?;
        }
    }
    Ok(())
}

/// Asset files under `dir` (recursive), without `.meta` or dot-names.
fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') || is_meta(&path) {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// What a scene file would contain if it used `target` (a file, or any file
/// under a folder): each file's GUID and its project-relative path.
pub fn reference_needles(server: &AssetServer, target: &Path) -> Vec<String> {
    let mut files = Vec::new();
    if target.is_dir() {
        files_under(target, &mut files);
    } else {
        files.push(target.to_path_buf());
    }
    let mut needles = Vec::new();
    for file in files {
        let Some(rel) = project_relative(server.root(), &file) else {
            continue;
        };
        if let Some(guid) = server.guid_of(&rel) {
            needles.push(guid.to_string());
        }
        needles.push(rel);
    }
    needles
}

/// `.ron` files under the project (not `target`, `.git`, `.melon` or other
/// dot-folders) whose text contains any needle, sorted.
pub fn scene_references(project_root: &Path, needles: &[String]) -> Vec<PathBuf> {
    fn walk(dir: &Path, needles: &[String], out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if name.starts_with('.') {
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if name != "target" {
                    walk(&path, needles, out);
                }
            } else if name.to_lowercase().ends_with(".ron")
                && std::fs::read_to_string(&path)
                    .is_ok_and(|text| needles.iter().any(|n| text.contains(n.as_str())))
            {
                out.push(path);
            }
        }
    }
    let mut found = Vec::new();
    if !needles.is_empty() {
        walk(project_root, needles, &mut found);
    }
    found.sort();
    found
}

/// Some entity in the open world draws `target` (a model file, or any model
/// under a folder).
pub fn open_scene_uses(
    world: &mut bevy_ecs::world::World,
    server: &AssetServer,
    target: &Path,
) -> bool {
    let Some(rel) = project_relative(server.root(), target) else {
        return false;
    };
    let inside = format!("{rel}/");
    world
        .query::<&melon_core::Handle<melon_core::Model>>()
        .iter(world)
        .any(|h| {
            let file = melon_core::assets::split_mesh_fragment(server.path(*h)).0;
            file == rel || file.starts_with(&inside)
        })
}

/// A path the OS shell accepts: absolute, no `..`, no `\\?\` prefix.
fn shell_path(path: &Path) -> PathBuf {
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = full.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => full,
    }
}

fn spawn(command: &mut std::process::Command) {
    if let Err(error) = command.spawn() {
        tracing::warn!("could not start {:?}: {error}", command.get_program());
    }
}

/// Show the item selected in the OS file manager.
pub fn reveal(path: &Path) {
    let path = shell_path(path);
    if cfg!(windows) {
        let mut select = std::ffi::OsString::from("/select,");
        select.push(&path);
        spawn(std::process::Command::new("explorer").arg(select));
    } else if cfg!(target_os = "macos") {
        spawn(std::process::Command::new("open").arg("-R").arg(&path));
    } else {
        spawn(std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(&path)));
    }
}

/// Open a file in the program the OS assigns to it.
pub fn open_external(path: &Path) {
    let path = shell_path(path);
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    spawn(std::process::Command::new(program).arg(&path));
}

#[cfg(test)]
mod tests {
    use super::ONE_MATERIAL_GLTF;
    use super::*;
    use crate::asset_db::reconcile;
    use melon_core::{AssetGuid, AssetServer, Handle, Model, meta_path};
    use std::path::{Path, PathBuf};

    fn project(name: &str) -> (PathBuf, AssetServer) {
        let root = std::env::temp_dir().join(format!("melon_ops_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets/models")).unwrap();
        let server = AssetServer::new(root.clone());
        (root, server)
    }

    /// Write an asset file and give it a `.meta` through `reconcile`.
    fn asset(root: &Path, server: &mut AssetServer, rel: &str) -> (PathBuf, AssetGuid) {
        let file = root.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();
        reconcile(server);
        let guid = server.guid_of(rel).expect("reconcile wrote a .meta");
        (file, guid)
    }

    fn remove_plain(paths: &[PathBuf]) -> anyhow::Result<()> {
        for p in paths {
            if p.is_dir() {
                std::fs::remove_dir_all(p)?;
            } else {
                std::fs::remove_file(p)?;
            }
        }
        Ok(())
    }

    #[test]
    fn validate_name_rejects_bad_names() {
        for bad in [
            "",
            "  ",
            "a/b",
            "a\\b",
            "a:b",
            "a*b",
            "a?b",
            "a\"b",
            "a<b",
            "a>b",
            "a|b",
            "a.",
            "a ",
            ".hidden",
            "x.glb.meta",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_name("Crate 2.glb").is_ok());
    }

    #[test]
    fn extract_materials_writes_mats_with_metas_next_to_the_model() {
        let (root, mut server) = project("extract");
        let model = root.join("assets/models/thing.gltf");
        std::fs::write(&model, ONE_MATERIAL_GLTF).unwrap();
        reconcile(&mut server);
        let parts = extract_materials(&mut server, &model).unwrap();
        assert_eq!(parts, ["assets/models/thing_Paint.mat"]);
        let mat = root.join("assets/models/thing_Paint.mat");
        assert_eq!(
            melon_core::MaterialAsset::read(&mat).unwrap().base_color,
            [0.0, 1.0, 0.0, 1.0]
        );
        assert!(meta_path(&mat).is_file(), "the .mat has a .meta");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unique_path_counts_up() {
        let (root, _) = project("unique");
        let dir = root.join("assets");
        std::fs::write(dir.join("a.glb"), b"x").unwrap();
        std::fs::write(dir.join("a 2.glb"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.glb"), dir.join("a 3.glb"));
        assert_eq!(unique_path(&dir, "A.GLB"), dir.join("A 3.GLB"));
        assert_eq!(unique_path(&dir, "models"), dir.join("models 2"));
        assert_eq!(unique_path(&dir, "b.glb"), dir.join("b.glb"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_material_writes_a_loadable_default_with_a_meta() {
        let (root, mut server) = project("create_material");
        let assets = root.join("assets");
        let first = create_material(&mut server, &assets).unwrap();
        let second = create_material(&mut server, &assets).unwrap();
        assert_eq!(first, assets.join("New Material.mat"));
        assert_eq!(second, assets.join("New Material 2.mat"));
        assert_eq!(
            melon_core::MaterialAsset::read(&first).unwrap(),
            melon_core::MaterialAsset::new_default()
        );
        assert!(melon_core::meta_path(&first).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_environment_writes_a_loadable_default_with_a_meta() {
        let (root, mut server) = project("create_environment");
        let assets = root.join("assets");
        let first = create_environment(&mut server, &assets).unwrap();
        let second = create_environment(&mut server, &assets).unwrap();
        assert_eq!(first, assets.join("New Environment.env"));
        assert_eq!(second, assets.join("New Environment 2.env"));
        for f in [&first, &second] {
            assert_eq!(
                melon_core::EnvironmentAsset::read(f).unwrap(),
                melon_core::EnvironmentAsset::default()
            );
            assert!(melon_core::meta_path(f).is_file());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_folder_names_new_folder_uniquely() {
        let (root, mut server) = project("new_folder");
        let assets = root.join("assets");
        assert_eq!(
            create_folder(&mut server, &assets).unwrap(),
            assets.join("New Folder")
        );
        assert_eq!(
            create_folder(&mut server, &assets).unwrap(),
            assets.join("New Folder 2")
        );
        assert!(assets.join("New Folder 2").is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_keeps_handle_path_and_guid() {
        let (root, mut server) = project("rename");
        let (a, g) = asset(&root, &mut server, "assets/models/a.glb");
        let h: Handle<Model> = server.load("assets/models/a.glb#mesh1");
        let new = rename(&mut server, &a, "b.glb").unwrap();
        assert_eq!(new, root.join("assets/models/b.glb"));
        assert_eq!(server.path(h), "assets/models/b.glb#mesh1");
        assert_eq!(server.guid_of("assets/models/b.glb"), Some(g));
        assert!(meta_path(&new).exists() && !meta_path(&a).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_case_only_keeps_handle() {
        let (root, mut server) = project("rename_case");
        let (a, g) = asset(&root, &mut server, "assets/models/a.glb");
        let h: Handle<Model> = server.load("assets/models/a.glb");
        rename(&mut server, &a, "A.glb").unwrap();
        assert_eq!(server.path(h), "assets/models/A.glb");
        assert_eq!(server.guid_of("assets/models/A.glb"), Some(g));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_case_only_non_ascii_keeps_handle() {
        let (root, mut server) = project("rename_case_unicode");
        let (a, g) = asset(&root, &mut server, "assets/models/ä.glb");
        let h: Handle<Model> = server.load("assets/models/ä.glb");
        rename(&mut server, &a, "Ä.glb").unwrap();
        assert_eq!(server.path(h), "assets/models/Ä.glb");
        assert_eq!(server.guid_of("assets/models/Ä.glb"), Some(g));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_rejects_case_insensitive_clash() {
        let (root, mut server) = project("rename_clash");
        let (a, _) = asset(&root, &mut server, "assets/models/a.glb");
        let (b, _) = asset(&root, &mut server, "assets/models/b.glb");
        assert!(rename(&mut server, &b, "A.GLB").is_err());
        assert!(a.exists() && b.exists() && meta_path(&b).exists());
        assert!(rename(&mut server, &b, "bad/name").is_err());
        assert!(b.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_folder_moves_inner_handles() {
        let (root, mut server) = project("rename_folder");
        let (_, g) = asset(&root, &mut server, "assets/models/sub/x.glb");
        let h: Handle<Model> = server.load("assets/models/sub/x.glb");
        rename(&mut server, &root.join("assets/models/sub"), "parts").unwrap();
        assert_eq!(server.path(h), "assets/models/parts/x.glb");
        assert_eq!(server.guid_of("assets/models/parts/x.glb"), Some(g));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn move_rejects_into_itself_and_clash() {
        let (root, mut server) = project("move_reject");
        let a = root.join("assets/a");
        std::fs::create_dir_all(a.join("b")).unwrap();
        assert!(move_to(&mut server, &a, &a).is_err());
        assert!(move_to(&mut server, &a, &a.join("b")).is_err());
        let (x, _) = asset(&root, &mut server, "assets/x.glb");
        asset(&root, &mut server, "assets/c/x.glb");
        assert!(move_to(&mut server, &x, &root.join("assets/c")).is_err());
        assert!(x.exists() && a.join("b").is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn move_carries_meta_and_keeps_guid() {
        let (root, mut server) = project("move");
        let (x, g) = asset(&root, &mut server, "assets/x.glb");
        std::fs::create_dir_all(root.join("assets/c")).unwrap();
        let moved = move_to(&mut server, &x, &root.join("assets/c")).unwrap();
        assert_eq!(moved, root.join("assets/c/x.glb"));
        assert!(meta_path(&moved).exists() && !meta_path(&x).exists());
        assert_eq!(server.guid_of("assets/c/x.glb"), Some(g));
        assert_eq!(
            move_to(&mut server, &moved, &root.join("assets/c")).unwrap(),
            moved
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_gets_new_guid_original_keeps_its_own() {
        let (root, mut server) = project("dup");
        let (x, g) = asset(&root, &mut server, "assets/x.glb");
        let copy = duplicate(&mut server, &x).unwrap();
        assert_eq!(copy, root.join("assets/x 2.glb"));
        assert_eq!(server.guid_of("assets/x.glb"), Some(g));
        let copy_guid = server.guid_of("assets/x 2.glb").unwrap();
        assert_ne!(copy_guid, g);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_folder_gives_inner_copies_new_guids() {
        let (root, mut server) = project("dup_folder");
        let (_, g) = asset(&root, &mut server, "assets/d/x.glb");
        let copy = duplicate(&mut server, &root.join("assets/d")).unwrap();
        assert_eq!(copy, root.join("assets/d 2"));
        assert_eq!(server.guid_of("assets/d/x.glb"), Some(g));
        let inner = server.guid_of("assets/d 2/x.glb").unwrap();
        assert_ne!(inner, g);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_removes_file_and_meta() {
        let (root, mut server) = project("delete");
        let (x, _) = asset(&root, &mut server, "assets/x.glb");
        delete_with(&mut server, &x, remove_plain).unwrap();
        assert!(!x.exists() && !meta_path(&x).exists());
        assert_eq!(server.guid_of("assets/x.glb"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_copies_with_unique_names_and_metas() {
        let (root, mut server) = project("import");
        asset(&root, &mut server, "assets/a.glb");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("a.glb"), b"y").unwrap();
        std::fs::write(outside.join("a.glb.meta"), b"(version: 1)").unwrap();
        let results = import(
            &mut server,
            &[outside.join("a.glb"), outside.join("a.glb.meta")],
            &root.join("assets"),
        );
        let imported: Vec<PathBuf> = results.into_iter().filter_map(Result::ok).collect();
        assert_eq!(imported, [root.join("assets/a 2.glb")]);
        assert!(server.guid_of("assets/a 2.glb").is_some());
        assert!(!root.join("assets/a.glb 2.meta").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn rename_rolls_back_when_meta_is_locked() {
        use std::os::windows::fs::OpenOptionsExt;
        let (root, mut server) = project("rollback");
        let (a, _) = asset(&root, &mut server, "assets/a.glb");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(meta_path(&a))
            .unwrap();
        assert!(rename(&mut server, &a, "b.glb").is_err());
        assert!(a.exists());
        assert!(!root.join("assets/b.glb").exists());
        drop(lock);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scene_references_find_guid_and_path() {
        let (root, mut server) = project("refs");
        let (a, g) = asset(&root, &mut server, "assets/models/a.glb");
        std::fs::create_dir_all(root.join("assets/scenes")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(
            root.join("assets/scenes/one.ron"),
            format!("model_guid: Some(\"{g}\")"),
        )
        .unwrap();
        std::fs::write(
            root.join("two.ron"),
            "model: Some(\"assets/models/a.glb#mesh0\")",
        )
        .unwrap();
        std::fs::write(root.join("target/x.ron"), g.to_string()).unwrap();
        std::fs::write(root.join("three.ron"), "nothing here").unwrap();
        let needles = reference_needles(&server, &a);
        assert_eq!(
            scene_references(&root, &needles),
            [root.join("assets/scenes/one.ron"), root.join("two.ron")]
        );
        let folder_needles = reference_needles(&server, &root.join("assets/models"));
        assert!(folder_needles.contains(&g.to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_scene_uses_sees_fragments_and_folders() {
        let (root, mut server) = project("uses");
        let mut world = bevy_ecs::world::World::new();
        let h: Handle<Model> = server.load("assets/models/sub/a.glb#mesh1");
        world.spawn(h);
        assert!(open_scene_uses(
            &mut world,
            &server,
            &root.join("assets/models/sub/a.glb")
        ));
        assert!(open_scene_uses(
            &mut world,
            &server,
            &root.join("assets/models/sub")
        ));
        assert!(!open_scene_uses(
            &mut world,
            &server,
            &root.join("assets/models/other.glb")
        ));
        assert!(!open_scene_uses(
            &mut world,
            &server,
            &root.join("assets/models/su")
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}
