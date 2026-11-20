//! What the page calls: the game's life in a browser, from importing the
//! player's files to running the client in a canvas.
//!
//! Each function is exported to JavaScript by wasm-bindgen. They share one
//! [`Session`], since a page runs on one thread and has one game.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;

use js_sys::{Array, Function, Object, Reflect};
use rl_client::ClientOptions;
use rl_client::content::Content;
use rl_client::net::{NetSettings, Network};
use rl_config::{Antialiasing, Quality, UserConfig};
use rl_data::GameData;
use rl_net::conditioner::NetConditions;
use rl_server::DummyScript;
use source_fs::Vfs;
use source_fs::remote::DEFAULT_RESIDENT_BUDGET;
use wasm_bindgen::prelude::*;

use super::{
    InstallStatus, PickedFolder, Progress, estimate, install_status, mount_installed, pick_folder,
    remove_install, request_persistence, run_import, storage_available,
};
use crate::import::{ImportRules, Plan, plan};
use crate::manifest::Manifest;

/// What one page remembers between calls.
#[derive(Default)]
struct Session {
    /// The folder the player picked, until it is imported.
    picked: Option<PickedFolder>,
    /// What importing it would copy.
    plan: Option<Plan>,
    /// The mounted game, once there is one.
    vfs: Option<Arc<Vfs>>,
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session::default());
}

/// A JavaScript object with the given properties.
fn object(properties: &[(&str, JsValue)]) -> JsValue {
    let object = Object::new();
    for (key, value) in properties {
        // Setting a property on a fresh plain object cannot fail.
        let _ = Reflect::set(&object, &JsValue::from_str(key), value);
    }
    object.into()
}

fn fail(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}

/// Called once when the module loads.
#[wasm_bindgen(start)]
pub fn start() {
    // Panics reach the console as messages rather than "unreachable".
    console_error_panic_hook::set_once();
}

/// What the browser can do and what has been imported.
///
/// `{ storage, state: "empty" | "partial" | "complete", files, bytes,
/// usage, quota, persisted }`.
///
/// # Errors
///
/// Fails if the browser's storage cannot be read.
#[wasm_bindgen]
pub async fn status() -> Result<JsValue, JsValue> {
    if !storage_available() {
        return Ok(object(&[("storage", false.into())]));
    }
    let (state, files, bytes) = match install_status().await.map_err(fail)? {
        InstallStatus::Empty => ("empty", 0, 0),
        InstallStatus::Partial { files, bytes } => ("partial", files, bytes),
        InstallStatus::Complete { files, bytes } => ("complete", files, bytes),
    };
    let storage = estimate().await.map_err(fail)?;
    Ok(object(&[
        ("storage", true.into()),
        ("state", state.into()),
        ("files", (files as f64).into()),
        ("bytes", (bytes as f64).into()),
        ("usage", (storage.usage as f64).into()),
        ("quota", (storage.quota as f64).into()),
        ("persisted", storage.persisted.into()),
    ]))
}

/// Asks the player for their install folder, and works out what to copy.
///
/// Must be called straight from a tap or click. Resolves to `null` if the
/// player cancels, else `{ error }` when the folder is not usable, or
/// `{ root, total, groups: [{ id, title, required, enabled, files, bytes }] }`.
///
/// # Errors
///
/// Fails if the browser refuses the picker.
#[wasm_bindgen(js_name = chooseFolder)]
pub async fn choose_folder() -> Result<JsValue, JsValue> {
    let Some(picked) = pick_folder().await.map_err(fail)? else {
        return Ok(JsValue::NULL);
    };
    let rules = ImportRules::builtin();
    let planned = match plan(&rules, &picked.candidates(), &rules.default_enabled()) {
        Ok(planned) => planned,
        Err(error) => return Ok(object(&[("error", error.to_string().into())])),
    };
    let result = describe(&planned);
    SESSION.with_borrow_mut(|session| {
        session.picked = Some(picked);
        session.plan = Some(planned);
    });
    Ok(result)
}

fn describe(plan: &Plan) -> JsValue {
    let groups = Array::new();
    for group in &plan.groups {
        groups.push(&object(&[
            ("id", group.id.as_str().into()),
            ("title", group.title.as_str().into()),
            ("required", group.required.into()),
            ("enabled", group.enabled.into()),
            ("files", (group.files as f64).into()),
            ("bytes", (group.bytes as f64).into()),
        ]));
    }
    object(&[
        ("root", plan.root.as_str().into()),
        ("total", (plan.total_bytes() as f64).into()),
        ("groups", groups.into()),
    ])
}

/// Copies the chosen folder's game files into the browser's storage.
/// `optional` lists the optional groups to include (by id).
/// `on_progress({ file, filesDone, filesTotal, bytesDone, bytesTotal })` is
/// called as the copy advances.
///
/// # Errors
///
/// Fails if nothing was chosen, or the browser refuses a write.
#[wasm_bindgen(js_name = importGame)]
pub async fn import_game(optional: Vec<String>, on_progress: Function) -> Result<(), JsValue> {
    let picked = SESSION.with_borrow_mut(|session| {
        session.plan = None;
        session.picked.take()
    });
    let Some(picked) = picked else {
        return Err(fail("choose the install folder first"));
    };
    let rules = ImportRules::builtin();
    let enabled: BTreeSet<String> = optional.into_iter().collect();
    let planned = plan(&rules, &picked.candidates(), &enabled).map_err(fail)?;
    // Ask the browser to keep the files; it may decline, and the import goes
    // ahead either way.
    let _ = request_persistence().await;
    let resume = super::read_manifest()
        .await
        .map_err(fail)?
        .filter(|manifest| manifest.status() != crate::manifest::ImportStatus::Complete)
        .unwrap_or_else(Manifest::new);
    run_import(&planned, &picked, resume, move |progress: &Progress| {
        let _ = on_progress.call1(
            &JsValue::NULL,
            &object(&[
                ("file", progress.file.as_str().into()),
                ("filesDone", (progress.files_done as f64).into()),
                ("filesTotal", (progress.files_total as f64).into()),
                ("bytesDone", (progress.bytes_done as f64).into()),
                ("bytesTotal", (progress.bytes_total as f64).into()),
            ]),
        );
    })
    .await
    .map_err(fail)?;
    Ok(())
}

/// Deletes the imported game files.
///
/// # Errors
///
/// Fails if the browser refuses.
#[wasm_bindgen(js_name = removeGame)]
pub async fn remove_game() -> Result<(), JsValue> {
    SESSION.with_borrow_mut(|session| session.vfs = None);
    remove_install().await.map_err(fail)
}

/// Mounts the imported game, if it is not mounted yet.
async fn mounted() -> Result<Arc<Vfs>, JsValue> {
    if let Some(vfs) = SESSION.with_borrow(|session| session.vfs.clone()) {
        return Ok(vfs);
    }
    let vfs = Arc::new(Vfs::new());
    mount_installed(
        &vfs,
        &ImportRules::builtin().mod_dir,
        DEFAULT_RESIDENT_BUDGET,
    )
    .await
    .map_err(fail)?;
    SESSION.with_borrow_mut(|session| session.vfs = Some(Arc::clone(&vfs)));
    Ok(vfs)
}

/// The deathmatch maps of the imported game, by name.
///
/// # Errors
///
/// Fails if the game is not imported or cannot be mounted.
#[wasm_bindgen(js_name = listMaps)]
pub async fn list_maps() -> Result<Vec<String>, JsValue> {
    let vfs = mounted().await?;
    Ok(rl_level::maps::deathmatch_maps(&vfs))
}

/// Starts the game on `map`, drawing into the canvas `canvas` (a CSS
/// selector). Resolves once the game is running; it then keeps running by
/// itself.
///
/// # Errors
///
/// Fails if the game is not imported, or the map's files cannot be fetched.
#[wasm_bindgen]
pub async fn play(canvas: String, map: String, player_name: String) -> Result<(), JsValue> {
    let vfs = mounted().await?;
    let data = GameData::builtin().map_err(fail)?;
    // What the client reads at startup must be in memory before it starts;
    // the map's own files are fetched by the client itself, behind its
    // loading screen.
    rl_client::prepare::startup(&vfs).await.map_err(fail)?;

    let content = Content::premounted(web_defaults(), &["hl2dm"], "browser storage");
    let net = NetSettings {
        network: Network::Host { port: None },
        name: player_name,
        conditions: NetConditions::NONE,
        dummies: 0,
        dummy_script: DummyScript::default(),
    };
    let mut options = ClientOptions::interactive(content, data, net);
    options.map = Some(map);
    options.canvas = Some(canvas);
    options.async_content = true;
    options.scale_factor = Some(render_scale());
    // `run` returns at once in a browser: the app is driven by the page.
    let _ = rl_client::run(vfs, options);
    Ok(())
}

/// The settings a page starts with: lighter than the desktop's, since the
/// device is likely a phone with a fraction of a desktop GPU's memory, and
/// the browser cannot be asked for more.
fn web_defaults() -> UserConfig {
    let mut config = UserConfig::default();
    let video = &mut config.settings.video;
    video.textures = Quality::Medium;
    video.shadows = Quality::Medium;
    video.antialiasing = Antialiasing::Off;
    config
}

/// Pixels to draw per logical pixel: the display's ratio, at most 2. A phone
/// screen's 3 would cost the GPU more than twice the pixels for detail small
/// enough that nobody can see it.
fn render_scale() -> f32 {
    let ratio = device_pixel_ratio();
    ratio.clamp(1.0, 2.0)
}

fn device_pixel_ratio() -> f32 {
    Reflect::get(&js_sys::global(), &"devicePixelRatio".into())
        .ok()
        .and_then(|value| value.as_f64())
        .map_or(1.0, |ratio| ratio as f32)
}

/// Says how the game's files are mounted and whether one is readable: where
/// `path` comes from, whether a synchronous read finds it in memory, and
/// whether an asynchronous one can fetch it. A diagnostic for developers
/// (`rustlife.probeFile("scripts/x.txt")` in the console).
///
/// # Errors
///
/// Fails if the game is not imported or cannot be mounted.
#[wasm_bindgen(js_name = probeFile)]
pub async fn probe_file(path: String) -> Result<String, JsValue> {
    let vfs = mounted().await?;
    let origin = vfs.locate(&path).map(|origin| origin.to_string());
    let synchronous = match vfs.read(&path) {
        Ok(Some(bytes)) => format!("resident, {} bytes", bytes.len()),
        Ok(None) => "not in any mount".to_owned(),
        Err(error) => format!("{error}"),
    };
    vfs.take_misses();
    let asynchronous = match vfs.read_async(&path).await {
        Ok(Some(bytes)) => format!("{} bytes", bytes.len()),
        Ok(None) => "not in any mount".to_owned(),
        Err(error) => format!("failed: {error}"),
    };
    Ok(format!(
        "{path}: from {origin:?}; synchronous read: {synchronous}; asynchronous read: {asynchronous}"
    ))
}
