//! The imported install, as the browser stores it.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use js_sys::{Array, Function, Reflect, Uint8Array};
use source_fs::byte_source::check_range;
use source_fs::remote::RemoteTree;
use source_fs::{BoxFuture, ByteSource};
use wasm_bindgen::prelude::*;

use super::error::{BrowserError, describe};
use crate::manifest::Manifest;

#[wasm_bindgen(module = "/js/storage.js")]
extern "C" {
    #[wasm_bindgen(js_name = readRange, catch)]
    async fn read_range(path: &str, offset: f64, length: f64) -> Result<Uint8Array, JsValue>;
    #[wasm_bindgen(js_name = readManifest, catch)]
    async fn read_manifest_text() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = writeManifest, catch)]
    async fn write_manifest_text(text: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = writeFile, catch)]
    pub(super) async fn write_file(
        path: &str,
        file: &JsValue,
        on_progress: &Function,
    ) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = removeInstall, catch)]
    async fn remove_install_js() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = estimate, catch)]
    async fn estimate_js() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = requestPersistence, catch)]
    async fn request_persistence_js() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = storageAvailable)]
    fn storage_available_js() -> bool;
}

/// Whether the browser has the private filesystem the import needs.
#[must_use]
pub fn storage_available() -> bool {
    storage_available_js()
}

/// How much the page may store, and how much it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageEstimate {
    /// Bytes in use by this page.
    pub usage: u64,
    /// Bytes the browser lets it use.
    pub quota: u64,
    /// Whether the browser promised not to evict it.
    pub persisted: bool,
}

/// Asks the browser for the page's storage usage and quota.
///
/// # Errors
///
/// Fails if the browser has no storage API.
pub async fn estimate() -> Result<StorageEstimate, BrowserError> {
    let value = estimate_js().await?;
    let number = |key: &str| {
        Reflect::get(&value, &key.into())
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as u64
    };
    Ok(StorageEstimate {
        usage: number("usage"),
        quota: number("quota"),
        persisted: Reflect::get(&value, &"persisted".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

/// Asks the browser to keep the stored files rather than evict them under
/// pressure. Returns whether it agreed (it may refuse, or decide silently).
///
/// # Errors
///
/// Fails if the browser has no storage API.
pub async fn request_persistence() -> Result<bool, BrowserError> {
    Ok(request_persistence_js().await?.as_bool().unwrap_or(false))
}

/// Deletes every imported file and the record of them.
///
/// # Errors
///
/// Fails if the browser refuses.
pub async fn remove_install() -> Result<(), BrowserError> {
    remove_install_js().await?;
    Ok(())
}

/// Writes `bytes` to `path` inside the install, replacing it. For small files
/// the program makes itself (and tests); the player's files go through the
/// import.
///
/// # Errors
///
/// Fails if the browser refuses the write.
pub async fn write_bytes(path: &str, bytes: &[u8]) -> Result<(), BrowserError> {
    let blob = new_blob(bytes)?;
    let noop = Closure::<dyn FnMut(f64)>::new(|_| {});
    write_file(path, &blob, noop.as_ref().unchecked_ref()).await?;
    Ok(())
}

/// `new Blob([bytes])`, without pulling in web-sys for one call.
pub(super) fn new_blob(bytes: &[u8]) -> Result<JsValue, BrowserError> {
    // `new Blob([bytes])`: the constructor's one argument is a list of parts.
    let parts = Array::of1(&Uint8Array::from(bytes));
    let arguments = Array::of1(&parts);
    let constructor = Reflect::get(&js_sys::global(), &"Blob".into())?;
    let constructor: Function = constructor
        .dyn_into()
        .map_err(|value| BrowserError(describe(&value)))?;
    Ok(Reflect::construct(&constructor, &arguments)?)
}

/// The record of what has been imported, or `None` if nothing has.
///
/// # Errors
///
/// Fails if the browser's storage cannot be read or the record is damaged.
pub async fn read_manifest() -> Result<Option<Manifest>, BrowserError> {
    let text = read_manifest_text().await?;
    let Some(text) = text.as_string() else {
        return Ok(None);
    };
    Manifest::parse(&text)
        .map(Some)
        .map_err(|error| BrowserError(error.to_string()))
}

/// Replaces the record of what has been imported.
///
/// # Errors
///
/// Fails if the browser refuses the write.
pub async fn write_manifest(manifest: &Manifest) -> Result<(), BrowserError> {
    write_manifest_text(&manifest.to_text()).await?;
    Ok(())
}

/// One imported file, read in ranges from the browser's storage.
///
/// Holds only its path and length: the browser handle behind it is looked up
/// (and cached) by the JavaScript side, so the source is plain data and can
/// be shared like any other `ByteSource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpfsSource {
    path: String,
    len: u64,
}

impl OpfsSource {
    /// The file at `path` inside the install, `len` bytes long.
    #[must_use]
    pub fn new(path: impl Into<String>, len: u64) -> Self {
        Self {
            path: path.into(),
            len,
        }
    }
}

impl ByteSource for OpfsSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, io::Result<Vec<u8>>> {
        Box::pin(async move {
            check_range(self.len, offset, len)?;
            if len == 0 {
                return Ok(Vec::new());
            }
            let bytes = read_range(&self.path, offset as f64, len as f64)
                .await
                .map_err(|error| io::Error::other(describe(&error)))?;
            Ok(bytes.to_vec())
        })
    }

    fn describe(&self) -> String {
        format!("browser storage: {}", self.path)
    }
}

/// The imported install as a [`RemoteTree`], built from its manifest.
#[derive(Debug)]
pub struct OpfsTree {
    files: BTreeMap<String, Arc<OpfsSource>>,
}

impl OpfsTree {
    /// The files `manifest` records.
    #[must_use]
    pub fn from_manifest(manifest: &Manifest) -> Self {
        Self {
            files: manifest
                .files()
                .map(|(path, size)| (path.to_owned(), Arc::new(OpfsSource::new(path, size))))
                .collect(),
        }
    }
}

impl RemoteTree for OpfsTree {
    fn files(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    fn open(&self, path: &str) -> Option<Arc<dyn ByteSource>> {
        self.files
            .get(path)
            .map(|source| Arc::clone(source) as Arc<dyn ByteSource>)
    }
}
