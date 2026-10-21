//! Listing the maps the mounted content provides.

use source_fs::Vfs;

/// A map that can be played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapEntry {
    /// Map name without path or extension, e.g. `dm_lockdown`.
    pub name: String,
    /// Human-readable origin of the copy that would load, e.g. `base game`.
    pub origin: String,
}

/// Lists the maps in `vfs` (`maps/*.bsp`), sorted by name, each attributed to
/// the highest-priority mount providing it.
#[must_use]
pub fn list_maps(vfs: &Vfs) -> Vec<MapEntry> {
    vfs.list("maps/*.bsp")
        .into_iter()
        .filter_map(|file| {
            let name = file.path.strip_prefix("maps/")?.strip_suffix(".bsp")?;
            Some(MapEntry {
                name: name.to_owned(),
                origin: file.origin.to_string(),
            })
        })
        .collect()
}

/// The names of the deathmatch maps in `vfs`: those following Source's `dm_`
/// naming convention, which excludes campaign and test maps shipped with the
/// same game.
#[must_use]
pub fn deathmatch_maps(vfs: &Vfs) -> Vec<String> {
    list_maps(vfs)
        .into_iter()
        .map(|entry| entry.name)
        .filter(|name| name.starts_with("dm_"))
        .collect()
}

#[cfg(test)]
mod tests {
    use source_fs::MountOrigin;

    use super::*;

    #[test]
    fn deathmatch_maps_are_the_dm_prefixed_ones() {
        let vfs = Vfs::new();
        let dir = tempfile::tempdir().unwrap();
        for name in ["maps/dm_a.bsp", "maps/d1_town.bsp", "maps/dm_b.bsp"] {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        }
        vfs.mount_back(
            source_fs::LooseDirectory::open(dir.path()).unwrap(),
            MountOrigin::BaseGame,
        );
        assert_eq!(deathmatch_maps(&vfs), ["dm_a", "dm_b"]);
        assert_eq!(list_maps(&vfs).len(), 3);
    }
}
