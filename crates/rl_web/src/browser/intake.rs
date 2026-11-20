//! Asking the player for their install folder.

use js_sys::{Array, Reflect};
use wasm_bindgen::prelude::*;

use super::error::BrowserError;
use super::storage::new_blob;
use crate::import::Candidate;

#[wasm_bindgen(module = "/js/storage.js")]
extern "C" {
    #[wasm_bindgen(js_name = pickFolder, catch)]
    async fn pick_folder_js() -> Result<JsValue, JsValue>;
}

/// The files of a folder the player picked: names and sizes in Rust, the
/// `File` objects themselves kept on the JavaScript side until copied.
pub struct PickedFolder {
    paths: Vec<String>,
    sizes: Vec<u64>,
    files: Array,
}

impl PickedFolder {
    /// The picked files, for planning an import.
    #[must_use]
    pub fn candidates(&self) -> Vec<Candidate> {
        self.paths
            .iter()
            .zip(&self.sizes)
            .map(|(path, &size)| Candidate {
                path: path.clone(),
                size,
            })
            .collect()
    }

    /// A folder of files held in memory, as if the player had picked it. For
    /// tests and for building a demo install.
    ///
    /// # Errors
    ///
    /// Fails if the browser cannot make the files.
    pub fn from_memory(files: &[(&str, &[u8])]) -> Result<Self, BrowserError> {
        let list = Array::new();
        for (_, bytes) in files {
            list.push(&new_blob(bytes)?);
        }
        Ok(Self {
            paths: files.iter().map(|(path, _)| (*path).to_owned()).collect(),
            sizes: files.iter().map(|(_, bytes)| bytes.len() as u64).collect(),
            files: list,
        })
    }

    /// Number of files picked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the folder was empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The `File` of the file at `index` in [`PickedFolder::candidates`].
    pub(super) fn file(&self, index: usize) -> JsValue {
        self.files.get(index as u32)
    }

    pub(super) fn path_index(&self) -> std::collections::HashMap<&str, usize> {
        self.paths
            .iter()
            .enumerate()
            .map(|(index, path)| (path.as_str(), index))
            .collect()
    }
}

/// Opens the browser's folder picker.
///
/// Browsers only allow this straight from a tap or click, so call it as the
/// first thing a click handler does, before any `await`. Returns `None` if the
/// player cancels.
///
/// # Errors
///
/// Fails if the browser refuses the picker.
pub async fn pick_folder() -> Result<Option<PickedFolder>, BrowserError> {
    let value = pick_folder_js().await?;
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    let list = |key: &str| -> Result<Array, BrowserError> {
        Ok(Reflect::get(&value, &key.into())?.into())
    };
    Ok(Some(PickedFolder {
        paths: list("paths")?
            .iter()
            .map(|path| path.as_string().unwrap_or_default())
            .collect(),
        sizes: list("sizes")?
            .iter()
            .map(|size| size.as_f64().unwrap_or(0.0) as u64)
            .collect(),
        files: list("files")?,
    }))
}
