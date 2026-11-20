// The page around the game: checks the browser, imports the player's game
// files, starts the game. The work happens in the Rust module (`rl_web`);
// this file only draws the menu and passes what the player chooses.

import init, { status, chooseFolder, importGame, listMaps, removeGame, play, probeFile } from "./pkg/rl_web.js";

// For developers: `rustlife.probeFile("scripts/x.txt")` in the console.
window.rustlife = { probeFile };

const $ = (id) => document.getElementById(id);
const NAME_KEY = "rustlife.name";

// ---- debug panel -----------------------------------------------------------

// `?debug=1` shows what the game logs, its frame rate and memory, with a Copy
// button: a phone has no console to read.
const debug = new URLSearchParams(location.search).get("debug") === "1";
const logLines = [];
let wasmMemory = null;
let frames = 0;
let fps = 0;

function plain(args) {
  // The game's logger passes CSS style strings after a `%c` format; drop them.
  const text = String(args[0] ?? "").replace(/%c/g, "");
  const rest = args.slice(1).filter((a) => !/^(color:|background:|font-style:|inherit)/.test(String(a)));
  return [text, ...rest.map(String)].join(" ");
}

if (debug) {
  for (const level of ["log", "info", "warn", "error", "debug"]) {
    const original = console[level];
    console[level] = (...args) => {
      logLines.push(`${level === "log" ? "" : level.toUpperCase() + " "}${plain(args)}`);
      if (logLines.length > 400) logLines.shift();
      original.apply(console, args);
    };
  }
  const panel = document.createElement("div");
  panel.id = "debug";
  panel.innerHTML = `<div id="debug-bar"><span id="debug-stats"></span>
    <button id="debug-copy">Copy</button><button id="debug-toggle">Log</button></div>
    <pre id="debug-log" hidden></pre>`;
  document.body.appendChild(panel);
  const log = panel.querySelector("#debug-log");
  panel.querySelector("#debug-toggle").onclick = () => {
    log.hidden = !log.hidden;
    if (!log.hidden) { log.textContent = logLines.join("\n"); log.scrollTop = log.scrollHeight; }
  };
  panel.querySelector("#debug-copy").onclick = async () => {
    const text = `${panel.querySelector("#debug-stats").textContent}\n${navigator.userAgent}\n${logLines.join("\n")}`;
    try { await navigator.clipboard.writeText(text); } catch { log.hidden = false; log.textContent = text; }
  };
  const tick = () => { frames += 1; requestAnimationFrame(tick); };
  requestAnimationFrame(tick);
  setInterval(() => {
    fps = frames; frames = 0;
    const heap = wasmMemory ? Math.round(wasmMemory.buffer.byteLength / 1048576) + " MB wasm" : "";
    panel.querySelector("#debug-stats").textContent = `${fps} fps · ${heap}`;
    if (!log.hidden) { log.textContent = logLines.join("\n"); log.scrollTop = log.scrollHeight; }
  }, 1000);
}

// ---- failures --------------------------------------------------------------

let lastPanic = "";
const originalError = console.error;
console.error = (...args) => {
  const text = args.map(String).join(" ");
  if (/panicked at/.test(text)) lastPanic = text;
  originalError.apply(console, args);
};

function fatal(message) {
  $("fatal-text").textContent = (lastPanic || message || "unknown error").slice(0, 4000);
  $("fatal").hidden = false;
}
window.addEventListener("error", (event) => {
  // Bevy ends its startup by throwing to leave the event loop; that is normal.
  if (/Using exceptions for control flow/.test(event.message || "")) { event.preventDefault(); return; }
  fatal(event.message);
});
window.addEventListener("unhandledrejection", (event) => {
  // Refusals the game recovers from (a browser may decline pointer lock).
  if (/pointer ?lock/i.test(String(event.reason))) { event.preventDefault(); return; }
  fatal(String(event.reason));
});
$("reload").onclick = () => location.reload();

// ---- audio -----------------------------------------------------------------

// Browsers start audio suspended until the player touches the page, and the
// game creates its audio context before that. Resume every context on touch.
const audioContexts = [];
for (const name of ["AudioContext", "webkitAudioContext"]) {
  const Original = window[name];
  if (!Original) continue;
  window[name] = class extends Original {
    constructor(...args) { super(...args); audioContexts.push(this); }
  };
}
const resumeAudio = () => audioContexts.forEach((context) => context.state !== "running" && context.resume());
for (const type of ["pointerdown", "touchend", "keydown"]) {
  window.addEventListener(type, resumeAudio, { capture: true });
}

// ---- helpers ---------------------------------------------------------------

const megabytes = (bytes) => (bytes / 1048576 >= 1024
  ? (bytes / 1073741824).toFixed(1) + " GB"
  : Math.round(bytes / 1048576) + " MB");

function show(...ids) {
  for (const id of ["unsupported", "import", "play"]) $(id).hidden = !ids.includes(id);
}

function say(text, isError = false) {
  const message = $("message");
  message.textContent = text;
  message.className = isError ? "error" : "muted";
  message.hidden = !text;
}

// ---- the menu --------------------------------------------------------------

async function refresh() {
  const state = await status();
  if (!state.storage) {
    show("unsupported");
    $("unsupported-text").textContent = "This browser cannot store the game files (no private file system).";
    say("");
    return;
  }
  const storage = `Using ${megabytes(state.usage)} of ${megabytes(state.quota)} available` +
    (state.persisted ? "; the browser promised to keep it." : ".");
  $("storage").textContent = storage;
  if (state.state === "complete") {
    say("");
    const maps = await listMaps();
    const select = $("map");
    select.replaceChildren(...maps.map((map) => new Option(map, map)));
    select.value = maps.includes("dm_lockdown") ? "dm_lockdown" : maps[0];
    $("player-name").value = localStorage.getItem(NAME_KEY) || "player";
    show("play");
  } else {
    show("import");
    $("plan").hidden = true;
    $("progress").hidden = true;
    $("choose").hidden = false;
    say(state.state === "partial"
      ? "An earlier copy did not finish. Choose the folder again to continue where it stopped."
      : "");
  }
}

$("choose").onclick = async () => {
  // The picker must be opened straight from the tap, so nothing comes before it.
  let plan;
  try {
    plan = await chooseFolder();
  } catch (error) {
    say(String(error), true);
    return;
  }
  if (plan === null) return;
  if (plan.error) { say(plan.error, true); return; }
  say("");
  const groups = $("groups");
  groups.replaceChildren(...plan.groups.map((group) => {
    const label = document.createElement("label");
    label.className = "group";
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = group.enabled;
    box.disabled = group.required;
    box.dataset.id = group.id;
    box.dataset.required = String(group.required);
    box.dataset.bytes = String(group.bytes);
    const text = document.createElement("span");
    text.textContent = `${group.title} (${group.files} files, ${megabytes(group.bytes)})`;
    label.append(box, text);
    return label;
  }));
  const total = () => [...groups.querySelectorAll("input")]
    .filter((box) => box.checked).reduce((sum, box) => sum + Number(box.dataset.bytes), 0);
  const update = () => { $("plan-total").textContent = `${megabytes(total())} will be copied into this browser.`; };
  groups.onchange = update;
  update();
  $("plan").hidden = false;
};

$("start-import").onclick = async () => {
  const optional = [...$("groups").querySelectorAll("input")]
    .filter((box) => box.checked && box.dataset.required === "false").map((box) => box.dataset.id);
  $("choose").hidden = true;
  $("plan").hidden = true;
  $("progress").hidden = false;
  say("Copying. Keep this page open.");
  try {
    await importGame(optional, (progress) => {
      $("bar").value = progress.bytesDone / Math.max(1, progress.bytesTotal);
      $("progress-text").textContent =
        `${megabytes(progress.bytesDone)} of ${megabytes(progress.bytesTotal)} · ${progress.file}`;
    });
  } catch (error) {
    say(`The copy stopped: ${error}. Choose the folder again to continue.`, true);
    $("choose").hidden = false;
    return;
  }
  await refresh();
};

$("remove").onclick = async () => {
  if (!confirm("Remove the copied game files from this browser?")) return;
  await removeGame();
  await refresh();
};

$("start").onclick = async () => {
  const name = $("player-name").value.trim() || "player";
  localStorage.setItem(NAME_KEY, name);
  const map = $("map").value;
  $("start").disabled = true;
  // The canvas has to be on the page before the game starts: the browser
  // refuses pointer lock and sizing for an element that is not displayed.
  document.body.classList.add("playing");
  $("loading-text").textContent = `Loading ${map}…`;
  $("loading").hidden = false;
  try {
    await play("#game", map, name);
  } catch (error) {
    document.body.classList.remove("playing");
    $("loading").hidden = true;
    $("start").disabled = false;
    say(String(error), true);
    return;
  }
  $("loading").hidden = true;
  $("game").focus();
};

// ---- start -----------------------------------------------------------------

async function main() {
  if (!navigator.gpu) {
    show("unsupported");
    $("unsupported-text").textContent = "This browser has no WebGPU. Use a current Safari, Chrome or Edge.";
    say("");
    return;
  }
  const wasm = await init();
  wasmMemory = wasm.memory;
  await refresh();
}
main().catch((error) => fatal(String(error)));
