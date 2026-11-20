# Web probe

Throwaway feasibility pages for the web build (wasm + WebGPU) of rustlife.
They answer, on a real phone, the questions that decide whether the web
route is viable at all:

| Spike | Question |
|---|---|
| S1 | Does the browser have WebGPU, with which features and limits, and does it accept the shaders and formats the game needs? |
| S2 | How much can a page store in the browser's private filesystem (OPFS), and does it survive? |
| S3 | How much memory (wasm heap, GPU textures) can a tab use before it is killed? |
| S4 | Can the player pick a multi-gigabyte game folder from the Files app, and how fast is it copied into OPFS? |
| S5 | Which other APIs exist (audio worklet, WebTransport, threads, fullscreen, ...)? |

The page prints a JSON report; "Copy report" puts it on the clipboard.

The page contains **no game content** and never reads anything the player
has not picked. It is deployed by `.github/workflows/pages.yml` and is
replaced by the real web build later.

## Running locally

```sh
python3 -m http.server 8000 --directory web/probe
```

then open <http://localhost:8000/> (localhost counts as a secure context, so
WebGPU and OPFS work). On a phone it must be served over HTTPS, which is what
the GitHub Pages deployment is for.
