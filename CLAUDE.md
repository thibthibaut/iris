# iris album

A GUI-first wedding album generator. Read `PLAN.md` for the goal, `CONTEXT.md` for the vocabulary (use those words in code and UI), and `docs/adr/` for decisions.

## Build and run

- Always inside the dev environment: `devenv shell -- cargo …`. Native dependencies (vips, libclang, openssl, typst) come from `devenv.nix`; add missing ones there, never with Homebrew.
- `devenv shell -- cargo test -p iris-core` — the pure core, runs in seconds. `cargo test --workspace` for everything.
- `devenv shell -- cargo run --release -- serve` → http://127.0.0.1:3000/. The photos are under `wedding-photo/` (two NFS mounts; reads are slow).
- CLI for scripting: `iris stages`, `iris index`, `iris run <stage>…`, `iris inventory`.

## Layout

- `crates/iris-core` — pure domain: no I/O. Decisions (Override replay), People, Shots, Chapters, quality gate, selection, layout, Proposal, Spread editing.
- `src/stages` — every pipeline step is a `Stage` (cancellable job, progress through `ProgressSink`).
- `src/state.rs` — the album as the user sees it (stored analysis + Overrides).
- `src/album.rs` — glue: candidates, current album (Proposal + Locks), previews.
- `src/render.rs` — `Renderer` trait, Typst implementation (CLI for now).
- `src/web` — axum + askama (`templates/`) + htmx; assets embedded from `assets/`.

## Rules

1. **GUI first.** Every feature ships with its GUI: a stage is not done until it can be started, watched, cancelled and inspected from the browser. Every setting is editable from the browser. CLI commands call the same functions.
2. **Never load full-size photos into your context.** Check results with a low-res screenshot or Spread preview (≤ 1200 px wide), one at a time. Read text reports, not images.
3. Stages are idempotent and incremental (per-Photo stages skip Photos done at the current `version`).
4. User decisions are Overrides (`iris_core::model::Override`), referring to Photos and faces, never Shots, clusters or Slots. They survive every regeneration.
5. Originals are read once (ingest) into WebP proxies under `.iris-cache/webp/`; analysis uses the proxies. Originals are decoded again only for print, one at a time.
6. Disk: proxies ≤ 300 MB; print crops go to a temp dir deleted after each export; one Edition at a time.
7. Never send images to a network service except through the VLM stages, which enforce the privacy settings.
8. Tests live at the agreed seams only: the pure core and the indexing helpers. Keep them few and behavioural. Check the GUI with a headless browser screenshot.
9. Quality v2 and Rejected are computed on the fly from stored measurements (`iris_core::quality`), so threshold and Couple changes apply at once.
