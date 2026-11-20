//! Assigning a `.mat` file to the parts of an entity's model.

use std::path::Path;

use melon_core::{AssetMeta, MaterialSlot, meta_path};

/// The slot naming `rel` (project-relative), with its GUID when the `.meta`
/// next to it can be read.
pub fn slot_for(root: &Path, rel: &str) -> MaterialSlot {
    let guid = AssetMeta::read(&meta_path(&root.join(rel)))
        .map(|m| m.guid.to_string())
        .unwrap_or_default();
    MaterialSlot {
        guid,
        path: rel.to_string(),
    }
}

/// Put `slot` on part `part`, or on every one of the model's `parts` for
/// `None` (at least one slot). Other parts keep what they had.
pub fn assign(
    slots: &mut Vec<MaterialSlot>,
    part: Option<usize>,
    parts: usize,
    slot: MaterialSlot,
) {
    match part {
        Some(i) => {
            if slots.len() <= i {
                slots.resize(i + 1, MaterialSlot::default());
            }
            slots[i] = slot;
        }
        None => {
            let count = parts.max(1).max(slots.len());
            *slots = vec![slot; count];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::MaterialSlot;

    fn slot(path: &str) -> MaterialSlot {
        MaterialSlot {
            guid: String::new(),
            path: path.into(),
        }
    }

    #[test]
    fn one_part_extends_the_list_and_keeps_the_others() {
        let mut slots = vec![slot("a.mat")];
        assign(&mut slots, Some(2), 3, slot("b.mat"));
        assert_eq!(slots, [slot("a.mat"), slot(""), slot("b.mat")]);
        assign(&mut slots, Some(0), 3, slot("c.mat"));
        assert_eq!(slots[0], slot("c.mat"));
    }

    #[test]
    fn no_part_fills_every_part_of_the_model() {
        let mut slots = vec![slot("a.mat")];
        assign(&mut slots, None, 3, slot("b.mat"));
        assert_eq!(slots, [slot("b.mat"), slot("b.mat"), slot("b.mat")]);
        // A model with no known parts still gets one slot.
        let mut none = Vec::new();
        assign(&mut none, None, 0, slot("b.mat"));
        assert_eq!(none.len(), 1);
    }

    #[test]
    fn the_slot_carries_the_guid_from_the_meta_file() {
        let root = std::env::temp_dir().join(format!("melon_assign_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/m.mat"), "()").unwrap();
        let guid = melon_core::AssetGuid::new();
        std::fs::write(
            root.join("assets/m.mat.meta"),
            melon_core::AssetMeta {
                version: melon_core::META_VERSION,
                guid,
            }
            .to_ron(),
        )
        .unwrap();
        let s = slot_for(&root, "assets/m.mat");
        assert_eq!((s.guid, s.path), (guid.to_string(), "assets/m.mat".into()));
        assert_eq!(slot_for(&root, "assets/none.mat").guid, "");
        let _ = std::fs::remove_dir_all(&root);
    }
}
