//! Writes the `.meta` sidecars that give every file under `assets/` a stable
//! GUID. `melon_core` only reads them (`AssetServer::scan_metas`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use melon_core::assets::{asset_dir_files, project_relative};
use melon_core::{AssetGuid, AssetMeta, AssetServer, META_VERSION, meta_path};

/// Bring the `.meta` files under `<root>/assets` in line with the asset
/// files, then rescan the server's GUID index:
/// - a valid `.meta` whose asset is gone is deleted (anything else ending
///   in `.meta` is left alone);
/// - a broken `.meta` is left alone (its asset gets no GUID until fixed);
/// - of several `.meta` files sharing a GUID, one keeps it (the path the
///   server already knew, else the oldest-created asset, else path order)
///   and the others get new GUIDs;
/// - an asset with no `.meta` gets one;
/// - an asset that moved with its `.meta` keeps its handles (`rename_path`).
pub fn reconcile(server: &mut AssetServer) {
    let root = server.root().to_path_buf();
    let before: HashMap<AssetGuid, String> = server
        .guid_index()
        .map(|(guid, path)| (guid, path.to_string()))
        .collect();
    let files = asset_dir_files(&root);

    let mut has_meta = HashSet::new();
    let mut by_guid: HashMap<AssetGuid, Vec<PathBuf>> = HashMap::new();
    for file in &files {
        let Some(asset) = file
            .to_str()
            .and_then(|f| f.strip_suffix(".meta"))
            .map(PathBuf::from)
        else {
            continue;
        };
        if !asset.is_file() {
            // Only delete sidecars we wrote; a user file that happens to end
            // in `.meta` stays.
            if AssetMeta::read(file).is_err() {
                continue;
            }
            match std::fs::remove_file(file) {
                Ok(()) => tracing::info!("removed orphan {}", file.display()),
                Err(error) => tracing::warn!("could not remove {}: {error}", file.display()),
            }
            continue;
        }
        match AssetMeta::read(file) {
            Ok(meta) => by_guid.entry(meta.guid).or_default().push(asset.clone()),
            Err(error) => tracing::warn!("broken {}: {error}", file.display()),
        }
        has_meta.insert(asset);
    }

    for (guid, assets) in by_guid.iter().filter(|(_, a)| a.len() > 1) {
        let known = before.get(guid);
        let keeper = assets
            .iter()
            .find(|a| project_relative(&root, a).as_ref() == known)
            .or_else(|| assets.iter().min_by_key(|a| created(a)))
            .expect("group has members");
        for other in assets.iter().filter(|a| *a != keeper) {
            tracing::warn!(
                "{} shares a GUID with {}; giving it a new one",
                other.display(),
                keeper.display()
            );
            write_meta(other, AssetGuid::new());
        }
    }

    for file in &files {
        let name = file.to_string_lossy();
        if !name.ends_with(".meta") && !name.ends_with(".meta.tmp") && !has_meta.contains(file) {
            write_meta(file, AssetGuid::new());
        }
    }

    server.scan_metas();
    for (guid, old) in before {
        if let Some(new) = server.path_of_guid(guid).map(str::to_string)
            && new != old
            // A case-only rename: the old spelling still "exists" on a
            // case-insensitive file system.
            && (!root.join(&old).exists() || old.eq_ignore_ascii_case(&new))
        {
            server.rename_path(&old, &new);
        }
    }
}

/// Creation time for the duplicate tie-break; unknown sorts last.
fn created(path: &Path) -> (bool, Option<SystemTime>) {
    let time = std::fs::metadata(path).and_then(|m| m.created()).ok();
    (time.is_none(), time)
}

/// Write `<asset>.meta` atomically (temp file, then rename). A failure is
/// logged and leaves any existing `.meta` as it was.
fn write_meta(asset: &Path, guid: AssetGuid) {
    let meta = meta_path(asset);
    let mut tmp = meta.clone().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let text = AssetMeta {
        version: META_VERSION,
        guid,
    }
    .to_ron();
    let result = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &meta));
    if let Err(error) = result {
        tracing::warn!("could not write {}: {error}", meta.display());
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::{AssetGuid, AssetMeta, AssetServer, Handle, META_VERSION, Model, meta_path};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_assetdb_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets/models")).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str) -> PathBuf {
        let file = root.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();
        file
    }

    fn write_meta_for(file: &Path, guid: AssetGuid) {
        let meta = AssetMeta {
            version: META_VERSION,
            guid,
        };
        std::fs::write(meta_path(file), meta.to_ron()).unwrap();
    }

    #[test]
    fn reconcile_writes_missing_metas_but_not_for_meta_or_dot_files() {
        let root = temp_root("missing");
        let a = write(&root, "assets/models/a.glb");
        let readme = write(&root, "assets/README.md");
        let hidden = write(&root, "assets/.hidden");
        let git = write(&root, "assets/.git/x");
        let mut server = AssetServer::new(root.clone());
        reconcile(&mut server);
        assert!(AssetMeta::read(&meta_path(&a)).is_ok());
        assert!(AssetMeta::read(&meta_path(&readme)).is_ok());
        assert!(!meta_path(&hidden).exists());
        assert!(!meta_path(&git).exists());
        assert!(!meta_path(&meta_path(&a)).exists());
        assert!(server.guid_of("assets/models/a.glb").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_removes_orphan_meta() {
        let root = temp_root("orphan");
        let gone = root.join("assets/models/gone.glb");
        write_meta_for(&gone, AssetGuid::new());
        let mut server = AssetServer::new(root.clone());
        reconcile(&mut server);
        assert!(!meta_path(&gone).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_leaves_broken_meta_alone() {
        let root = temp_root("broken");
        let a = write(&root, "assets/models/a.glb");
        std::fs::write(meta_path(&a), "<<<<<<< HEAD").unwrap();
        let mut server = AssetServer::new(root.clone());
        reconcile(&mut server);
        assert_eq!(
            std::fs::read_to_string(meta_path(&a)).unwrap(),
            "<<<<<<< HEAD"
        );
        assert_eq!(server.guid_of("assets/models/a.glb"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_duplicate_cold_start_original_keeps_guid() {
        let root = temp_root("dup_cold");
        let original = write(&root, "assets/models/cube.glb");
        let g = AssetGuid::new();
        write_meta_for(&original, g);
        std::thread::sleep(Duration::from_millis(50));
        // An Explorer copy: later creation time, same modified time.
        let copy = root.join("assets/models/cube - Copy.glb");
        std::fs::copy(&original, &copy).unwrap();
        std::fs::copy(meta_path(&original), meta_path(&copy)).unwrap();
        let modified = std::fs::metadata(&original).unwrap().modified().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&copy)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let mut server = AssetServer::new(root.clone());
        reconcile(&mut server);
        assert_eq!(server.guid_of("assets/models/cube.glb"), Some(g));
        assert_ne!(server.guid_of("assets/models/cube - Copy.glb"), Some(g));
        assert!(server.guid_of("assets/models/cube - Copy.glb").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_duplicate_known_path_keeps_guid() {
        let root = temp_root("dup_known");
        let a = write(&root, "assets/models/a.glb");
        std::thread::sleep(Duration::from_millis(50));
        let b = write(&root, "assets/models/b.glb");
        let g = AssetGuid::new();
        write_meta_for(&b, g);
        let mut server = AssetServer::new(root.clone());
        reconcile(&mut server);
        assert_eq!(server.guid_of("assets/models/b.glb"), Some(g));
        // a.glb is older, so the cold-start rule would pick it; the path the
        // server already knows must win.
        write_meta_for(&a, g);
        reconcile(&mut server);
        assert_eq!(server.guid_of("assets/models/b.glb"), Some(g));
        assert_ne!(server.guid_of("assets/models/a.glb"), Some(g));
        assert!(server.guid_of("assets/models/a.glb").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_follows_moved_asset() {
        let root = temp_root("moved");
        let a = write(&root, "assets/models/a.glb");
        let g = AssetGuid::new();
        write_meta_for(&a, g);
        let mut server = AssetServer::new(root.clone());
        reconcile(&mut server);
        let h: Handle<Model> = server.load("assets/models/a.glb#mesh0");
        let b = root.join("assets/models/sub/b.glb");
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();
        std::fs::rename(&a, &b).unwrap();
        std::fs::rename(meta_path(&a), meta_path(&b)).unwrap();
        reconcile(&mut server);
        assert_eq!(server.path(h), "assets/models/sub/b.glb#mesh0");
        assert_eq!(server.path_of_guid(g), Some("assets/models/sub/b.glb"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_keeps_a_user_file_named_meta() {
        let root = temp_root("user_meta");
        let notes = write(&root, "assets/notes.meta");
        let mut server = AssetServer::new(root.clone());
        reconcile(&mut server);
        assert!(
            notes.exists(),
            "a .meta that is not ours must not be deleted"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_follows_case_only_rename() {
        let root = temp_root("case_only");
        let a = write(&root, "assets/models/a.glb");
        let g = AssetGuid::new();
        write_meta_for(&a, g);
        let mut server = AssetServer::new(root.clone());
        reconcile(&mut server);
        let h: Handle<Model> = server.load("assets/models/a.glb");
        let upper = root.join("assets/models/A.glb");
        std::fs::rename(&a, &upper).unwrap();
        std::fs::rename(meta_path(&a), meta_path(&upper)).unwrap();
        reconcile(&mut server);
        assert_eq!(server.path(h), "assets/models/A.glb");
        assert_eq!(server.path_of_guid(g), Some("assets/models/A.glb"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
