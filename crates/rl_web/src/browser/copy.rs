//! Copying the planned files into the browser's storage.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

use super::error::BrowserError;
use super::intake::PickedFolder;
use super::storage::{write_file, write_manifest};
use crate::import::Plan;
use crate::manifest::Manifest;

/// How far an import has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The file being copied.
    pub file: String,
    /// Files finished, including those an earlier run copied.
    pub files_done: usize,
    /// Files in the plan.
    pub files_total: usize,
    /// Bytes copied, including those an earlier run copied.
    pub bytes_done: u64,
    /// Bytes in the plan.
    pub bytes_total: u64,
}

/// Copies what `plan` lists and `done` does not already hold, in order,
/// recording each finished file in the stored manifest so an interrupted
/// import resumes. Returns the completed manifest.
///
/// `on_progress` is called as chunks arrive.
///
/// # Errors
///
/// Fails when the browser refuses a write (out of space, say); what was
/// copied stays recorded, and a later call resumes.
pub async fn run_import(
    plan: &Plan,
    picked: &PickedFolder,
    mut done: Manifest,
    on_progress: impl FnMut(&Progress) + 'static,
) -> Result<Manifest, BrowserError> {
    let index = picked.path_index();
    let pending = plan.pending(&done);
    let files_total = plan.files.len();
    let bytes_total = plan.total_bytes();
    let pending_bytes: u64 = pending.iter().map(|file| file.size).sum();
    // What an earlier run already copied counts as done.
    let mut bytes_before = bytes_total - pending_bytes;
    let mut files_before = files_total - pending.len();
    let on_progress = Rc::new(RefCell::new(on_progress));

    write_manifest(&done).await?;
    for file in pending {
        let source = index
            .get(file.source.as_str())
            .ok_or_else(|| BrowserError(format!("{} was not in the picked folder", file.source)))?;
        let chunk_progress = {
            let on_progress = Rc::clone(&on_progress);
            let name = file.dest.clone();
            let (files_done, bytes_done) = (files_before, bytes_before);
            Closure::<dyn FnMut(f64)>::new(move |copied: f64| {
                (on_progress.borrow_mut())(&Progress {
                    file: name.clone(),
                    files_done,
                    files_total,
                    bytes_done: bytes_done + copied as u64,
                    bytes_total,
                });
            })
        };
        write_file(
            &file.dest,
            &picked.file(*source),
            chunk_progress.as_ref().unchecked_ref(),
        )
        .await?;
        bytes_before += file.size;
        files_before += 1;
        done.record(&file.dest, file.size);
        write_manifest(&done).await?;
    }
    done.complete();
    write_manifest(&done).await?;
    Ok(done)
}
