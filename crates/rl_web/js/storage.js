// The browser's side of importing the player's game files.
//
// Everything here is plumbing: the decisions (which files, in which order,
// how to resume) are made in Rust (`rl_web::import`). The files live in the
// origin private filesystem (OPFS), under `install/`, mirroring the install
// folder, with a text manifest next to them (`install.manifest`).
//
// OPFS is written through a worker using synchronous access handles, the one
// write API every browser that has OPFS supports (verified on iPhone Safari).
// Reads happen on the main thread, as `File` slices, which are asynchronous
// and need no lock.

const INSTALL_DIR = "install";
const MANIFEST_FILE = "install.manifest";
/** Bytes copied per message to the worker. */
const CHUNK_BYTES = 8 * 1024 * 1024;

// ---- the worker ------------------------------------------------------------

const WORKER_SOURCE = `
"use strict";
const handles = new Map();
async function directory(path, create) {
  let dir = await navigator.storage.getDirectory();
  const parts = path.split("/").filter(Boolean);
  const file = parts.pop();
  for (const part of parts) dir = await dir.getDirectoryHandle(part, { create });
  return { dir, file };
}
async function open(path) {
  let handle = handles.get(path);
  if (!handle) {
    const { dir, file } = await directory(path, true);
    const fileHandle = await dir.getFileHandle(file, { create: true });
    handle = await fileHandle.createSyncAccessHandle();
    handles.set(path, handle);
  }
  return handle;
}
const commands = {
  // Writes a chunk of a file; the first (offset 0) empties it.
  async put({ path, offset, data }) {
    const handle = await open(path);
    if (offset === 0) handle.truncate(0);
    handle.write(new Uint8Array(data), { at: offset });
  },
  async close({ path }) {
    const handle = handles.get(path);
    if (handle) { handle.flush(); handle.close(); handles.delete(path); }
  },
  async putText({ path, text }) {
    const handle = await open(path);
    const bytes = new TextEncoder().encode(text);
    handle.truncate(0);
    handle.write(bytes, { at: 0 });
    handle.flush(); handle.close(); handles.delete(path);
  },
  async remove({ path }) {
    for (const [open, handle] of handles) { handle.close(); handles.delete(open); }
    const root = await navigator.storage.getDirectory();
    try { await root.removeEntry(path, { recursive: true }); }
    catch (error) { if (error.name !== "NotFoundError") throw error; }
  },
};
self.onmessage = async ({ data }) => {
  try {
    await commands[data.command](data);
    self.postMessage({ id: data.id });
  } catch (error) {
    self.postMessage({ id: data.id, error: error.name + ": " + error.message });
  }
};
`;

let worker = null;
let nextId = 1;
const pending = new Map();

function callWorker(message, transfer = []) {
  if (!worker) {
    const url = URL.createObjectURL(new Blob([WORKER_SOURCE], { type: "text/javascript" }));
    worker = new Worker(url);
    worker.onmessage = ({ data }) => {
      const call = pending.get(data.id);
      pending.delete(data.id);
      if (!call) return;
      if (data.error) call.reject(new Error(data.error)); else call.resolve();
    };
  }
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    worker.postMessage({ ...message, id }, transfer);
  });
}

// ---- reading ---------------------------------------------------------------

const files = new Map();
const directories = new Map();

async function installDirectory(parts) {
  const key = parts.join("/");
  let dir = directories.get(key);
  if (!dir) {
    if (parts.length === 0) {
      dir = await (await navigator.storage.getDirectory()).getDirectoryHandle(INSTALL_DIR);
    } else {
      const parent = await installDirectory(parts.slice(0, -1));
      dir = await parent.getDirectoryHandle(parts[parts.length - 1]);
    }
    directories.set(key, dir);
  }
  return dir;
}

async function installFile(path) {
  let file = files.get(path);
  if (!file) {
    const parts = path.split("/").filter(Boolean);
    const name = parts.pop();
    const dir = await installDirectory(parts);
    file = await (await dir.getFileHandle(name)).getFile();
    files.set(path, file);
  }
  return file;
}

function forgetReads() {
  files.clear();
  directories.clear();
}

/** Reads `length` bytes at `offset` of an imported file. */
export async function readRange(path, offset, length) {
  const file = await installFile(path);
  return new Uint8Array(await file.slice(offset, offset + length).arrayBuffer());
}

/** The text of the import record, or `undefined` if nothing was imported. */
export async function readManifest() {
  try {
    const root = await navigator.storage.getDirectory();
    const file = await (await root.getFileHandle(MANIFEST_FILE)).getFile();
    return await file.text();
  } catch (error) {
    if (error.name === "NotFoundError") return undefined;
    throw error;
  }
}

// ---- writing ---------------------------------------------------------------

/** Replaces the import record. */
export async function writeManifest(text) {
  await callWorker({ command: "putText", path: MANIFEST_FILE, text });
}

/**
 * Copies `file` (a `File`, or a `Blob`) to `install/<path>` in chunks, calling
 * `onProgress(bytesCopied)` after each. Resolves once it is flushed.
 */
export async function writeFile(path, file, onProgress) {
  forgetReads();
  const target = INSTALL_DIR + "/" + path;
  let offset = 0;
  if (file.size === 0) {
    await callWorker({ command: "put", path: target, offset: 0, data: new ArrayBuffer(0) });
  }
  while (offset < file.size) {
    const data = await file.slice(offset, offset + CHUNK_BYTES).arrayBuffer();
    // Read the length first: sending the buffer detaches it, and a detached
    // buffer is 0 bytes long.
    const length = data.byteLength;
    await callWorker({ command: "put", path: target, offset, data }, [data]);
    offset += length;
    if (onProgress) onProgress(offset);
  }
  await callWorker({ command: "close", path: target });
}

/** Removes everything imported, files and record. */
export async function removeInstall() {
  forgetReads();
  await callWorker({ command: "remove", path: INSTALL_DIR });
  await callWorker({ command: "remove", path: MANIFEST_FILE });
}

// ---- storage state ---------------------------------------------------------

/** Usage and quota in bytes, and whether the browser promised to keep the data. */
export async function estimate() {
  const estimate = navigator.storage.estimate ? await navigator.storage.estimate() : {};
  return {
    usage: estimate.usage || 0,
    quota: estimate.quota || 0,
    persisted: navigator.storage.persisted ? await navigator.storage.persisted() : false,
  };
}

/** Asks the browser not to evict the stored files. */
export async function requestPersistence() {
  return navigator.storage.persist ? await navigator.storage.persist() : false;
}

/** Whether the browser has the private filesystem at all. */
export function storageAvailable() {
  return !!(navigator.storage && navigator.storage.getDirectory);
}

// ---- picking the folder ----------------------------------------------------

async function walk(handle, prefix, out) {
  for await (const [name, entry] of handle.entries()) {
    const path = prefix ? prefix + "/" + name : name;
    if (entry.kind === "directory") await walk(entry, path, out);
    else out.push({ path, file: await entry.getFile() });
  }
}

function inputPicker() {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.webkitdirectory = true;
    input.multiple = true;
    input.style.display = "none";
    input.addEventListener("change", () => {
      const picked = [...input.files].map((file) => ({ path: file.webkitRelativePath || file.name, file }));
      input.remove();
      resolve(picked);
    });
    input.addEventListener("cancel", () => { input.remove(); resolve(null); });
    document.body.appendChild(input);
    input.click();
  });
}

/**
 * Asks the player for the install folder. Must be called straight from a tap
 * or click (browsers refuse a picker otherwise). Resolves to `null` if the
 * player cancels, else `{ paths, sizes, files }`: parallel arrays.
 */
export async function pickFolder() {
  let picked;
  if (window.showDirectoryPicker) {
    try {
      const handle = await window.showDirectoryPicker();
      picked = [];
      await walk(handle, handle.name, picked);
    } catch (error) {
      if (error.name === "AbortError") return null;
      throw error;
    }
  } else {
    picked = await inputPicker();
    if (picked === null) return null;
  }
  return {
    paths: picked.map((entry) => entry.path),
    sizes: picked.map((entry) => entry.file.size),
    files: picked.map((entry) => entry.file),
  };
}
