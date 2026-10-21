//! The layered virtual filesystem.

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard};

use crate::error::Result;
use crate::mount::Mount;
use crate::path::{Glob, normalize};

/// Where a mount's content comes from. Used for diagnostics and to label maps
/// in the map browser.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MountOrigin {
    /// Content shipped with the game (search paths from `gameinfo.txt`).
    BaseGame,
    /// User customisations in the mod's `custom/` folder.
    Custom,
    /// A Steam Workshop item.
    Workshop {
        /// App the item was published for.
        app_id: u32,
        /// Published file id.
        item_id: u64,
    },
    /// Files downloaded from game servers (the mod's `download/` folder).
    Download,
    /// The pakfile embedded in a map, mounted while that map is loaded.
    MapPakfile {
        /// Map name without extension, e.g. `dm_lockdown`.
        map: String,
    },
}

impl fmt::Display for MountOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BaseGame => f.write_str("base game"),
            Self::Custom => f.write_str("custom"),
            Self::Workshop { app_id, item_id } => write!(f, "workshop {item_id} (app {app_id})"),
            Self::Download => f.write_str("download"),
            Self::MapPakfile { map } => write!(f, "pakfile of {map}"),
        }
    }
}

/// Opaque handle identifying a mount within a [`Vfs`], used to unmount it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MountId(u64);

/// Public description of a mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    /// Handle for [`Vfs::unmount`].
    pub id: MountId,
    /// Where the content comes from.
    pub origin: MountOrigin,
    /// Human-readable location, from [`Mount::describe`].
    pub description: String,
}

/// A file found by [`Vfs::list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedFile {
    /// Canonical virtual path.
    pub path: String,
    /// Origin of the highest-priority mount providing it.
    pub origin: MountOrigin,
}

#[derive(Debug, Clone)]
struct Mounted {
    id: MountId,
    origin: MountOrigin,
    mount: Arc<dyn Mount>,
}

/// An ordered stack of [`Mount`]s searched from highest to lowest priority.
///
/// The first mount containing a path wins, exactly like the engine's search
/// path list. A `Vfs` is cheap to share (`Arc<Vfs>`) and safe to use from many
/// threads; the mount list can change at runtime (a map's pakfile is mounted
/// on load and removed on unload), guarded by a read-write lock that is held
/// only for the duration of each lookup.
///
/// ```
/// # let dir = tempfile::tempdir().unwrap();
/// # std::fs::write(dir.path().join("a.txt"), b"low").unwrap();
/// use source_fs::{LooseDirectory, MountOrigin, Vfs};
///
/// let vfs = Vfs::new();
/// vfs.mount_back(LooseDirectory::open(dir.path()).unwrap(), MountOrigin::BaseGame);
/// assert_eq!(vfs.read("A.TXT").unwrap().unwrap(), b"low");
/// ```
#[derive(Debug, Default)]
pub struct Vfs {
    mounts: RwLock<Vec<Mounted>>,
    next_id: AtomicU64,
}

impl Vfs {
    /// Creates an empty filesystem.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn mounts(&self) -> RwLockReadGuard<'_, Vec<Mounted>> {
        // Lookups never leave the list half-modified, so a poisoned lock
        // (a panic elsewhere while holding it) still guards valid data.
        self.mounts.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn insert(&self, mount: Arc<dyn Mount>, origin: MountOrigin, front: bool) -> MountId {
        let id = MountId(self.next_id.fetch_add(1, Ordering::Relaxed));
        tracing::debug!(%origin, location = %mount.describe(), front, "mounting");
        let mounted = Mounted { id, origin, mount };
        let mut mounts = self.mounts.write().unwrap_or_else(PoisonError::into_inner);
        if front {
            mounts.insert(0, mounted);
        } else {
            mounts.push(mounted);
        }
        id
    }

    /// Adds a mount with the lowest priority.
    pub fn mount_back(&self, mount: impl Mount + 'static, origin: MountOrigin) -> MountId {
        self.insert(Arc::new(mount), origin, false)
    }

    /// Adds a mount with the highest priority (used for map pakfiles).
    pub fn mount_front(&self, mount: impl Mount + 'static, origin: MountOrigin) -> MountId {
        self.insert(Arc::new(mount), origin, true)
    }

    /// Removes a mount. Returns whether it was present.
    pub fn unmount(&self, id: MountId) -> bool {
        let mut mounts = self.mounts.write().unwrap_or_else(PoisonError::into_inner);
        let before = mounts.len();
        mounts.retain(|mounted| mounted.id != id);
        mounts.len() != before
    }

    /// Removes every mount with the given origin; returns how many were removed.
    pub fn unmount_origin(&self, origin: &MountOrigin) -> usize {
        let mut mounts = self.mounts.write().unwrap_or_else(PoisonError::into_inner);
        let before = mounts.len();
        mounts.retain(|mounted| &mounted.origin != origin);
        before - mounts.len()
    }

    /// Removes every mount, e.g. before mounting a different set of games.
    pub fn clear(&self) {
        self.mounts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Describes every mount in priority order.
    #[must_use]
    pub fn mount_list(&self) -> Vec<MountInfo> {
        self.mounts()
            .iter()
            .map(|mounted| MountInfo {
                id: mounted.id,
                origin: mounted.origin.clone(),
                description: mounted.mount.describe(),
            })
            .collect()
    }

    /// Finds the highest-priority mount containing `path`. The mount is
    /// cloned out so the lock is not held while reading file data.
    fn find(&self, path: &str) -> Option<Mounted> {
        self.mounts()
            .iter()
            .find(|mounted| mounted.mount.contains(path))
            .cloned()
    }

    /// Reads the file at `path` (any case, either separator), or `Ok(None)` if
    /// no mount has it.
    ///
    /// # Errors
    ///
    /// Returns the error from the highest-priority mount containing the file
    /// if that mount fails to read it. Lower-priority copies are deliberately
    /// not tried: silently falling back would hide corrupt content.
    pub fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let path = normalize(path);
        match self.find(&path) {
            Some(mounted) => mounted.mount.read(&path),
            None => Ok(None),
        }
    }

    /// Reads a file as UTF-8 text, replacing invalid sequences. Content text
    /// files are nominally ASCII but community files contain stray Latin-1.
    ///
    /// # Errors
    ///
    /// See [`Vfs::read`].
    pub fn read_string(&self, path: &str) -> Result<Option<String>> {
        Ok(self
            .read(path)?
            .map(|bytes| match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
            }))
    }

    /// Whether any mount contains `path`.
    #[must_use]
    pub fn exists(&self, path: &str) -> bool {
        self.find(&normalize(path)).is_some()
    }

    /// Returns the origin of the mount that would serve `path`.
    #[must_use]
    pub fn locate(&self, path: &str) -> Option<MountOrigin> {
        self.find(&normalize(path)).map(|mounted| mounted.origin)
    }

    /// Lists every file matching `pattern` (see [`Glob`]), each attributed to
    /// the highest-priority mount providing it, sorted by path.
    #[must_use]
    pub fn list(&self, pattern: &str) -> Vec<ListedFile> {
        let glob = Glob::new(pattern);
        let prefix = glob.literal_prefix();
        let mut seen = HashSet::new();
        let mut files = Vec::new();
        for mounted in self.mounts().iter() {
            mounted.mount.for_each_file(&mut |path| {
                if path.starts_with(&prefix) && glob.matches(path) && seen.insert(path.to_owned()) {
                    files.push(ListedFile {
                        path: path.to_owned(),
                        origin: mounted.origin.clone(),
                    });
                }
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// In-memory mount for tests.
    #[derive(Debug)]
    struct MemoryMount(HashMap<String, Vec<u8>>);

    impl MemoryMount {
        fn new(files: &[(&str, &str)]) -> Self {
            Self(
                files
                    .iter()
                    .map(|(p, d)| (normalize(p), d.as_bytes().to_vec()))
                    .collect(),
            )
        }
    }

    impl Mount for MemoryMount {
        fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
            Ok(self.0.get(path).cloned())
        }
        fn contains(&self, path: &str) -> bool {
            self.0.contains_key(path)
        }
        fn for_each_file(&self, visit: &mut dyn FnMut(&str)) {
            self.0.keys().for_each(|k| visit(k));
        }
        fn describe(&self) -> String {
            "memory".into()
        }
    }

    #[test]
    fn earlier_mounts_shadow_later_ones() {
        let vfs = Vfs::new();
        vfs.mount_back(MemoryMount::new(&[("a.txt", "high")]), MountOrigin::Custom);
        vfs.mount_back(
            MemoryMount::new(&[("a.txt", "low"), ("b.txt", "only")]),
            MountOrigin::BaseGame,
        );
        assert_eq!(vfs.read_string("a.txt").unwrap().unwrap(), "high");
        assert_eq!(vfs.read_string("B.TXT").unwrap().unwrap(), "only");
        assert_eq!(vfs.locate("a.txt"), Some(MountOrigin::Custom));
        assert_eq!(vfs.locate("b.txt"), Some(MountOrigin::BaseGame));
        assert!(!vfs.exists("c.txt"));
        assert_eq!(vfs.read("c.txt").unwrap(), None);
    }

    #[test]
    fn front_mounts_take_priority_and_can_be_removed() {
        let vfs = Vfs::new();
        vfs.mount_back(
            MemoryMount::new(&[("m.vmt", "base")]),
            MountOrigin::BaseGame,
        );
        let pak = MountOrigin::MapPakfile { map: "dm_x".into() };
        let id = vfs.mount_front(MemoryMount::new(&[("m.vmt", "map")]), pak.clone());
        assert_eq!(vfs.read_string("m.vmt").unwrap().unwrap(), "map");
        assert!(vfs.unmount(id));
        assert!(!vfs.unmount(id));
        assert_eq!(vfs.read_string("m.vmt").unwrap().unwrap(), "base");

        vfs.mount_front(MemoryMount::new(&[]), pak.clone());
        assert_eq!(vfs.unmount_origin(&pak), 1);
        assert_eq!(vfs.mount_list().len(), 1);
    }

    #[test]
    fn list_deduplicates_by_priority_and_sorts() {
        let vfs = Vfs::new();
        let workshop = MountOrigin::Workshop {
            app_id: 320,
            item_id: 7,
        };
        vfs.mount_back(
            MemoryMount::new(&[("maps/b.bsp", ""), ("maps/a.bsp", "")]),
            workshop.clone(),
        );
        vfs.mount_back(
            MemoryMount::new(&[("maps/a.bsp", ""), ("maps/graphs/a.ain", ""), ("x.txt", "")]),
            MountOrigin::BaseGame,
        );
        let maps = vfs.list("maps/*.bsp");
        assert_eq!(
            maps,
            [
                ListedFile {
                    path: "maps/a.bsp".into(),
                    origin: workshop.clone()
                },
                ListedFile {
                    path: "maps/b.bsp".into(),
                    origin: workshop
                },
            ]
        );
        assert_eq!(vfs.list("**").len(), 4);
    }

    #[test]
    fn invalid_utf8_is_replaced_not_rejected() {
        #[derive(Debug)]
        struct Latin1;
        impl Mount for Latin1 {
            fn read(&self, _: &str) -> Result<Option<Vec<u8>>> {
                Ok(Some(vec![b'c', 0xe9]))
            }
            fn contains(&self, _: &str) -> bool {
                true
            }
            fn for_each_file(&self, _: &mut dyn FnMut(&str)) {}
            fn describe(&self) -> String {
                String::new()
            }
        }
        let vfs = Vfs::new();
        vfs.mount_back(Latin1, MountOrigin::BaseGame);
        assert_eq!(vfs.read_string("x").unwrap().unwrap(), "c\u{fffd}");
    }

    #[test]
    fn origin_display() {
        assert_eq!(
            MountOrigin::Workshop {
                app_id: 220,
                item_id: 5
            }
            .to_string(),
            "workshop 5 (app 220)"
        );
        assert_eq!(
            MountOrigin::MapPakfile { map: "dm_a".into() }.to_string(),
            "pakfile of dm_a"
        );
    }
}
