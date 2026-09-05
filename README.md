# XMLSpy-rs — Browser XML IDE

An XMLSpy-style XML IDE that runs entirely in the browser: single-pass streaming
well-formedness scanner, sparse structural index, and virtualized Text / Grid /
Schema / Browser views.

Stack: **React 19 + TypeScript + Vite 7 + Tailwind CSS 4**, with the XML engine written in
**Rust** and shipped as an inlined WebAssembly module (TypeScript fallback included).

📋 See [`TODO.md`](./TODO.md) for the feature-by-feature status audit (what's done, what's
partial, what's left, and the suggested next tasks).

> ⚠️ The app lives in the nested folder [`XMLSpy/`](./XMLSpy) — all commands below
> must be run from `XMLSpy/XMLSpy` (the folder that contains `package.json`).

---

## 1. Run it on a brand-new machine (nothing installed)

### Step 1 — Install Node.js (this is the only prerequisite)

Vite 7 requires **Node.js 20.19+ or 22.12+** (Node 22 LTS recommended).
npm ships with Node, so you don't install it separately.

**Windows**
1. Download the LTS installer from <https://nodejs.org/en/download> and run it
   (keep "Add to PATH" checked), **or** in PowerShell:
   ```powershell
   winget install OpenJS.NodeJS.LTS
   ```
2. Close and reopen the terminal.

**macOS**
```bash
# with Homebrew (install Homebrew first from https://brew.sh)
brew install node
```

**Linux (Ubuntu/Debian)** — the distro's `apt install nodejs` is usually too old:
```bash
curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -
sudo apt-get install -y nodejs
```

**Any OS, version-manager route (recommended if you juggle Node versions)**
```bash
# https://github.com/nvm-sh/nvm  (macOS/Linux)
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash
nvm install 22
nvm use 22
```

Verify:
```bash
node -v   # v22.x (must be >= 20.19)
npm -v    # 10.x or newer
```

### Step 2 — Get the code

With Git ([git-scm.com/downloads](https://git-scm.com/downloads)):
```bash
git clone https://github.com/debanaik1407/XMLSpy.git
cd XMLSpy/XMLSpy
```

No Git? Download the ZIP from the GitHub page ("Code" → "Download ZIP"),
unzip it, then `cd` into the inner `XMLSpy` folder.

### Step 3 — Install dependencies

```bash
npm ci        # exact versions from package-lock.json (preferred)
# or: npm install
```
Takes a few seconds and creates `node_modules/` (~107 MB). No other global tools,
no Rust toolchain, no database, no API keys are needed.

### Step 4 — Start the dev server

```bash
npm run dev
```
Open the printed URL — <http://localhost:5173>. Hot reload is on; edit anything in
`src/` and the browser updates instantly. Stop with `Ctrl+C`.

---

## 2. Production build

```bash
npm run build     # outputs dist/
npm run preview   # serves the built app locally
```

Thanks to `vite-plugin-singlefile`, the build is **one self-contained
`dist/index.html`** (~490 KB — JS, CSS *and* the 68 KB WebAssembly engine inlined). You can
double-click that file, email it, or drop it on any static host — no server required.

---

## 2b. The Rust engine (optional — a prebuilt module is committed)

Scanning, the structural index and the streaming Find run in Rust compiled to WebAssembly.
**You do not need Rust installed to run or build the app**: the compiled module is
committed as base64 in `src/engine/wasmBinary.ts`. Install Rust only if you want to change
the engine.

```bash
# one-time: rustup + the wasm target  (https://rustup.rs)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add wasm32-unknown-unknown

# from XMLSpy/XMLSpy
npm run build:wasm     # rebuilds the module and regenerates src/engine/wasmBinary.ts
npm run test:parity    # 84 checks: Rust vs the TypeScript engine, incl. the real worker source

# from the repo root
cd rust
cargo test                                  # 41 tests
cargo run -p xmlspy-cli --release -- bench big.xml
```

The status bar shows **⚙ Rust/WASM** or **⚙ TS fallback** so you always know which engine
answered. Details: [`rust/README.md`](./rust/README.md) and
[`rust/bench/reports/`](./rust/bench/reports).

---

## 3. Useful extras

| Command | What it does |
| --- | --- |
| `npm run dev` | Dev server with HMR on port 5173 |
| `npm run dev -- --port 3000` | Dev server on a different port |
| `npm run dev -- --host` | Expose on your LAN (open from a phone/another PC) |
| `npm run build` | Single-file production bundle in `dist/` |
| `npm run preview` | Serve the production build |
| `npx tsc --noEmit` | Type-check the whole project |
| `npm run test:parity` | Diff the Rust/WASM engine against the TypeScript engine (84 checks) |
| `npm run build:wasm` | Rebuild the Rust engine → `src/engine/wasmBinary.ts` (needs Rust) |

### Running behind a proxy / remote preview URL

Vite blocks unknown `Host` headers. If you run the dev server on a remote box,
container, or Codespace and reach it through a proxied domain, use the included
sandbox config, which sets `host: 0.0.0.0` and `allowedHosts: true`:

```bash
npx vite --config vite.config.sandbox.ts
```
(For local development just use `npm run dev`; that file is optional and can be
deleted if you never run remotely.)

---

## 4. Troubleshooting

| Symptom | Fix |
| --- | --- |
| `npm: command not found` | Node isn't installed or the terminal wasn't restarted after installing. |
| `crypto.hash is not a function` / weird Vite errors | Node too old. Upgrade to 20.19+/22.12+ (`nvm install 22`). |
| `Cannot find module ... package.json` | You're in the outer folder. `cd XMLSpy` once more. |
| `EADDRINUSE: port 5173` | Something else uses the port: `npm run dev -- --port 5174`. |
| `Blocked request. This host is not allowed` | Use `npx vite --config vite.config.sandbox.ts` (see above). |
| Install fails behind a corporate proxy | `npm config set proxy http://user:pass@host:port` (and `https-proxy`). |
| Broken install | `rm -rf node_modules package-lock.json && npm install` (Windows: `rmdir /s /q node_modules`). |
| Status bar says `⚙ TS fallback` | WebAssembly was blocked (very old browser, or a CSP without `wasm-unsafe-eval`). The app still works, ~4× slower. |
| `npm run build:wasm` fails | Rust missing: install from <https://rustup.rs>, then `rustup target add wasm32-unknown-unknown`. |

---

## 5. Project layout

```
XMLSpy/                  ← repo root
├── rust/                ← the Rust engine (cargo workspace, no external crates)
│   ├── crates/xmlspy-core    shared no_std types
│   ├── crates/xmlspy-index   StructuralIndex + .xsi codec
│   ├── crates/xmlspy-parse   resumable scanner, SWAR classifier, streaming Finder
│   ├── crates/xmlspy-wasm    C ABI cdylib for the browser (no wasm-bindgen)
│   ├── crates/xmlspy-cli     `xmlspy` CLI: wf / index / info / search / gen / bench
│   ├── build-wasm.sh         builds + base64-embeds the module into the app
│   └── bench/reports/        measured numbers, honestly reported
└── XMLSpy/              ← the actual app (run npm here)
    ├── index.html
    ├── package.json
    ├── vite.config.ts
    ├── vite.config.sandbox.ts   ← optional, remote-preview only
    ├── tsconfig.json
    └── src/
        ├── main.tsx, App.tsx
        ├── components/   Chrome, TextView, GridView, SchemaView, BrowserView, DocsView, Panels
        ├── engine/       engine (selection), wasmEngine + wasmBinary (Rust/WASM),
        │                 scanner (TS fallback), worker, document, pieceTable,
        │                 xpath, schemaInfer, highlight, corpus
        ├── scripts/      parity.mjs — Rust vs TypeScript engine diff
        ├── docs/         architecture, roadmap, rustCode
        └── utils/
```

`@/` is aliased to `src/`, so `import { cn } from "@/utils/cn"` works anywhere.
