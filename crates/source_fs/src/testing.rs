//! Builders that synthesise content layouts for tests.
//!
//! rustlife's tests must never depend on Valve content being present, so this
//! module writes small but format-accurate VPK archives and fake Steam library
//! layouts into a caller-provided directory (typically a `tempfile::TempDir`).
//!
//! Available to other crates through the `testing` cargo feature.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::vpk::{DIR_ARCHIVE_INDEX, SIGNATURE};

/// Files grouped the way the VPK tree nests them:
/// extension -> directory -> [(file stem, file)].
type GroupedFiles<'a> = BTreeMap<String, BTreeMap<String, Vec<(String, &'a PendingFile)>>>;

struct PendingFile {
    data: Vec<u8>,
    preload_len: usize,
}

/// Writes a VPK set (`<stem>_dir.vpk` plus `<stem>_000.vpk`).
///
/// ```
/// # let dir = tempfile::tempdir().unwrap();
/// use source_fs::{Mount, testing::VpkBuilder, vpk::VpkArchive};
///
/// VpkBuilder::new().file("scripts/a.txt", b"hello").write(dir.path(), "pak01").unwrap();
/// let vpk = VpkArchive::open(dir.path().join("pak01.vpk")).unwrap();
/// assert_eq!(vpk.read("scripts/a.txt").unwrap().unwrap(), b"hello");
/// ```
#[must_use]
pub struct VpkBuilder {
    version: u32,
    embed_in_dir: bool,
    files: BTreeMap<String, PendingFile>,
}

impl Default for VpkBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl VpkBuilder {
    /// Starts an empty version-2 archive.
    pub fn new() -> Self {
        Self {
            version: 2,
            embed_in_dir: false,
            files: BTreeMap::new(),
        }
    }

    /// Selects the directory format version (1 or 2).
    pub fn version(mut self, version: u32) -> Self {
        self.version = version;
        self
    }

    /// Stores file data inside the directory file (archive index `0x7fff`)
    /// instead of a numbered archive.
    pub fn embed_in_dir(mut self, embed: bool) -> Self {
        self.embed_in_dir = embed;
        self
    }

    /// Adds a file stored entirely in the data archive.
    pub fn file(self, path: &str, data: &[u8]) -> Self {
        self.file_with_preload(path, data, 0)
    }

    /// Adds a file whose first `preload_len` bytes are stored inline in the
    /// directory, exercising the preload code path.
    pub fn file_with_preload(mut self, path: &str, data: &[u8], preload_len: usize) -> Self {
        let preload_len = preload_len.min(data.len());
        self.files.insert(
            path.to_owned(),
            PendingFile {
                data: data.to_vec(),
                preload_len,
            },
        );
        self
    }

    /// Writes the archive into `dir` and returns the directory file's path.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from writing the files.
    pub fn write(self, dir: &Path, stem: &str) -> io::Result<PathBuf> {
        let mut grouped = GroupedFiles::new();
        for (path, file) in &self.files {
            let (directory, name) = path.rsplit_once('/').unwrap_or((" ", path.as_str()));
            let (filename, extension) = name.rsplit_once('.').unwrap_or((name, " "));
            grouped
                .entry(extension.to_owned())
                .or_default()
                .entry(directory.to_owned())
                .or_default()
                .push((filename.to_owned(), file));
        }

        let archive_index = if self.embed_in_dir {
            DIR_ARCHIVE_INDEX
        } else {
            0
        };
        let mut tree = Vec::new();
        let mut data = Vec::new();
        for (extension, directories) in &grouped {
            push_cstr(&mut tree, extension);
            for (directory, files) in directories {
                push_cstr(&mut tree, directory);
                for (filename, file) in files {
                    push_cstr(&mut tree, filename);
                    let (preload, rest) = file.data.split_at(file.preload_len);
                    tree.extend_from_slice(&crc32fast::hash(&file.data).to_le_bytes());
                    tree.extend_from_slice(&(preload.len() as u16).to_le_bytes());
                    tree.extend_from_slice(&archive_index.to_le_bytes());
                    tree.extend_from_slice(&(data.len() as u32).to_le_bytes());
                    tree.extend_from_slice(&(rest.len() as u32).to_le_bytes());
                    tree.extend_from_slice(&0xffff_u16.to_le_bytes());
                    tree.extend_from_slice(preload);
                    data.extend_from_slice(rest);
                }
                tree.push(0);
            }
            tree.push(0);
        }
        tree.push(0);

        let mut dir_file = Vec::new();
        dir_file.extend_from_slice(&SIGNATURE.to_le_bytes());
        dir_file.extend_from_slice(&self.version.to_le_bytes());
        dir_file.extend_from_slice(&(tree.len() as u32).to_le_bytes());
        if self.version == 2 {
            let embedded_len = if self.embed_in_dir {
                data.len() as u32
            } else {
                0
            };
            dir_file.extend_from_slice(&embedded_len.to_le_bytes());
            dir_file.extend_from_slice(&[0; 12]);
        }
        dir_file.extend_from_slice(&tree);

        let dir_path = dir.join(format!("{stem}_dir.vpk"));
        if self.embed_in_dir {
            dir_file.extend_from_slice(&data);
        } else {
            fs::write(dir.join(format!("{stem}_000.vpk")), &data)?;
        }
        fs::write(&dir_path, dir_file)?;
        Ok(dir_path)
    }
}

fn push_cstr(buffer: &mut Vec<u8>, text: &str) {
    buffer.extend_from_slice(text.as_bytes());
    buffer.push(0);
}

/// Builds a fake Steam installation: a root with `libraryfolders.vdf`, one or
/// more libraries, installed apps with `appmanifest_<id>.acf`, and workshop
/// items.
///
/// ```
/// # let tmp = tempfile::tempdir().unwrap();
/// use source_fs::testing::SteamFixture;
///
/// let steam = SteamFixture::new(tmp.path());
/// let game = steam.install_app(steam.root(), 320, "Half-Life 2 Deathmatch").unwrap();
/// assert!(game.ends_with("steamapps/common/Half-Life 2 Deathmatch"));
/// ```
#[derive(Debug, Clone)]
pub struct SteamFixture {
    root: PathBuf,
    libraries: Vec<PathBuf>,
}

impl SteamFixture {
    /// Creates a Steam root at `root` that is also its own first library.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            libraries: vec![root.to_path_buf()],
        }
    }

    /// The Steam root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Registers an additional library folder and rewrites `libraryfolders.vdf`.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors.
    pub fn add_library(&mut self, library: &Path) -> io::Result<()> {
        self.libraries.push(library.to_path_buf());
        self.write_library_folders()
    }

    /// Writes `steamapps/libraryfolders.vdf` listing every library.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors.
    pub fn write_library_folders(&self) -> io::Result<()> {
        let mut vdf = String::from("\"libraryfolders\"\n{\n");
        for (index, library) in self.libraries.iter().enumerate() {
            let _ = writeln!(
                vdf,
                "\t\"{index}\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}",
                library.display()
            );
        }
        vdf.push_str("}\n");
        let steamapps = self.root.join("steamapps");
        fs::create_dir_all(&steamapps)?;
        fs::write(steamapps.join("libraryfolders.vdf"), vdf)
    }

    /// Installs an app into `library`, writing its manifest, and returns the
    /// game directory (`<library>/steamapps/common/<install_dir>`).
    ///
    /// # Errors
    ///
    /// Propagates I/O errors.
    pub fn install_app(
        &self,
        library: &Path,
        app_id: u32,
        install_dir: &str,
    ) -> io::Result<PathBuf> {
        self.write_library_folders()?;
        let steamapps = library.join("steamapps");
        let game_dir = steamapps.join("common").join(install_dir);
        fs::create_dir_all(&game_dir)?;
        fs::write(
            steamapps.join(format!("appmanifest_{app_id}.acf")),
            format!(
                "\"AppState\"\n{{\n\t\"appid\"\t\t\"{app_id}\"\n\t\"installdir\"\t\t\"{install_dir}\"\n}}\n"
            ),
        )?;
        Ok(game_dir)
    }

    /// Creates an (empty) workshop item directory and returns it so the caller
    /// can populate it, e.g. with a [`VpkBuilder`] named `workshop`.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors.
    pub fn workshop_item(&self, library: &Path, app_id: u32, item_id: u64) -> io::Result<PathBuf> {
        let dir = library
            .join("steamapps/workshop/content")
            .join(app_id.to_string())
            .join(item_id.to_string());
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

/// A minimal `gameinfo.txt` mirroring the structure of HL2DM's: a custom
/// wildcard, a mod VPK, a shared-content VPK, and loose directories.
pub const SAMPLE_GAMEINFO: &str = r#"
"GameInfo"
{
    game    "Sample DM"
    type    multiplayer_only
    FileSystem
    {
        SteamAppId  320
        SearchPaths
        {
            game+mod            mod/custom/*
            game+mod            mod/mod_pak.vpk
            game                |all_source_engine_paths|shared/shared_pak.vpk
            platform            |all_source_engine_paths|platform/platform_misc.vpk
            mod+mod_write+default_write_path    |gameinfo_path|.
            game+game_write     mod
            gamebin             mod/bin
            game                |all_source_engine_paths|shared
            game+download       mod/download
        }
    }
}
"#;
