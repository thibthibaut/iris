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
- `src/preflight.rs` — checks the print PDF as written (page sizes, parity, fonts); golden PDF test.
- `src/web` — axum + askama (`templates/`) + htmx; assets embedded from `assets/` (`assets/fonts/` = static print fonts, `assets/web-fonts/` = variable fonts for the UI only).
- `scripts/` — `serve.sh` (run the app), `shot.sh` (GUI screenshot).

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

## Status and next steps (2026-10-26)

### Running the app during QA
- `scripts/serve.sh` (after `devenv shell -- cargo build --release`): copies the binary to `.iris-cache/iris-app` and serves on :3000. Run it in the background from the repo root; to restart, stop it, rebuild, run again.
- The user labels People while it runs: restart only when no Override was saved in the last 20 s (`sqlite3 album.db "select (strftime('%s','now')*1000 - max(ts_ms))/1000 from overrides"`).
- Screenshots: `scripts/shot.sh URL OUT [W,H]` (headless Brave, profile in `.iris-cache/brave-profile`; delete it if styles look old). Spread previews: `curl localhost:3000/preview/<uid>`, then `vips thumbnail` to ≤ 1200 px before reading. Tell the user to hard-refresh (⌘⇧R) after CSS/JS changes.
- DB: `album.db` at the repo root (gitignored). Proxies: `.iris-cache/webp/`. Exports: `exports/<edition>/`.

### State of the data
- Indexed: 980 photos (626 photographer, 354 guests). All 9 Chapters populated. Album: 48 pages, 23 Spreads, 140 Shots; `ours` v2 exported with 0 preflight warnings.
- The user's Overrides: ~65 names, Emna = Bride, Thibaut = Groom, 27 Group memberships, 7 Chapter moves, 4 Bans, 3 Locked Spreads. No Quotas yet.
- The 3 Locked Spreads were Locked before the margins changed (15 → 10 mm): they keep the old geometry until "Relayout" in the Spread editor.
- Default Chapter start times in `Settings::default()` are guesses; the photographer's mairie photos start at 14:07. The user should set real times in Settings.
- Default Arabic titles (Tunisian) are in `Settings::default()` / `Texts::from`; the opening page's names (آمنة و تيبو) await Emna's check.

### Lessons (don't re-learn them)
- `face_id` returns bboxes as fractions of the image (0–1), not pixels.
- libvips decodes sequentially: an image pipeline can be read once; derive the 400 px proxy from the saved 1200 px file.
- Settings Overrides use dotted paths; a number indexes a list (`chapters.3.starts_at`).
- `htmx.onLoad` gets the swapped child, not its container: re-init drag-and-drop on `document`.
- Ingest failures are retried only by an explicit Re-run (failed ≠ pending).
- Locked Spreads keep the geometry they were Locked with: after a margin change, "Relayout" them in the Spread editor.
- Typst warns on variable fonts and silently falls back on unknown ones: print fonts in `assets/fonts/` are static instances; the variable originals for the web UI live in `assets/web-fonts/` (Typst scans font dirs recursively). Export preflight surfaces Typst warnings.
- `render_print` gives both halves of a Spread the same (left, right) numbers.
- zsh reads `$f[wght]` as an array subscript: write `${f}[wght]`.
- A Proposal is stored at build time; the shown album is it + Locks + current Decisions. Check Conflicts against the shown album (`iris_core::album::conflicts`), not the stored one.

### Code review of `17650dc...f3f2533` — remaining findings, in this order
**Print correctness (Spec):** done (page numbers, bleed Crop, Ban/Pin and unresolved MoveChapter/People Override Conflicts, Group budget, real PDF preflight + golden test in `src/preflight.rs`).

**Missing (Spec):** Groups renamable/addable in the GUI (§6); Hero Slots skip the 250 ppi shrink and crop zoom only warns after saving; export manifest lacks an album-state hash and a "keep this version" option.

**Standards:**
- CLAUDE.md rule 1: no GUI for Chapter weights/prompts/titles, Groups, Editions, wedding date, `design.body_font`; `Export::all(&["ours", …])` and `HERO_CHAPTERS` hard-code setting keys.
- CONTEXT.md vocabulary: "Family"/"families" in UI text (use Group), `ChapterSection`/`sections` (use Chapter), `duplicates`/`.dup-group` (use Shot/copies), `SpreadLayout` (borderline).
- Smells: string keys for Chapter/Group/Edition/Template; bare `i64` Persons; `exports()` 6-tuple → struct; `Spread { … }` literal duplicated in `album.rs`; "face is Couple" check duplicated in `state.rs` and `album.rs`; `jobs::hash` → `fingerprint`.
- Possible bugs: `Source::parse` maps unknown values to Guest; Export staleness ignores texts/design/Bans; `couple_anchors()` recomputed per Photo in `AlbumState::load`.

**Then:** keep PLAN.md §13 ("As built") in step, and continue QA with the user (People → Chapter times → Chapters → rebuild → Spread editor → export a proof).
