# PLAN — `iris album`: GUI-first wedding album generator

Branch: `album` on `thibthibaut/iris`. Everything stays in Rust, in a single binary.

**This branch is a separate project.** It doesn't need to stay in sync with `main`, and nothing will be merged back. The agent is free to restructure iris as much as the album needs: rename or split modules, change the DB schema (no compatibility with old iris databases; a fresh DB is fine), replace the CLI layout, rewrite the web layer, drop features that get in the way (OCR, geo). Prefer a clean design over minimal diffs.

**Vocabulary.** `CONTEXT.md` is the glossary: Photo, Shot, Representative, Chapter, Spread, Page, Template, Slot, Crop, Hero, Edition, Group, Core Spread, Group Spread, Person, Couple, Couple Share, Quota, Override, Pin, Ban, Lock, Rejected, Proposal. Code, UI labels and this plan use those words and avoid the ones listed under _Avoid_. Architectural decisions live in `docs/adr/`.

**Goal.** From ~1,000 wedding photos (photographer + guests), produce print-ready, versioned PDFs for 4 Editions of an A4 landscape hardcover album.

**The product is the GUI.** The user (the groom) does everything in the browser: indexing with visible progress, labelling Persons and Groups, setting Quotas, fixing Chapters, choosing and replacing Shots, editing Spreads, writing text, exporting. Settings too. The CLI is a thin wrapper over the same functions, for scripting and for the coding agent. Nobody should have to edit a config file to use the app.

---

## 0. Context

### The day: 19 July 2025, Paris

| # | Chapter id | Moment | Where | Source |
|---|---|---|---|---|
| 1 | `prep_bride` | Préparatifs de la mariée | location A | photographer `preparatifs/` (+ guests) |
| 2 | `prep_groom` | Préparatifs du marié | location B | **guests only** (the photographer wasn't there) |
| 3 | `mairie_before` | Devant la mairie & salle d'attente | Mairie du 13e, place d'Italie (13h45) | photographer `mairie/` (+ guests) |
| 4 | `mairie_ceremony` | Salle des mariages, échange des alliances | Mairie | photographer `mairie/` (+ guests) |
| 5 | `mairie_after` | Sortie de la mairie, Group Portraits | in front of the Mairie | photographer `groupe/` and `mairie/` (+ guests) |
| 6 | `bnf_couple` | Séance couple | BNF | photographer `couple/` |
| 7 | `cocktail` | Cocktail | Tribunes du vélodrome (La Cipale, 18h) | photographer `cocktail/` (+ guests) |
| 8 | `dinner` | Dîner | Restaurant du vélodrome | photographer `diner et soiree/` (+ guests) |
| 9 | `party` | Soirée | Tribunes du vélodrome | photographer `diner et soiree/` (+ guests) |

Chapters, their order, titles (French + Tunisian Arabic) and the folder → Chapter(s) mapping are data stored in the DB, seeded with this table and editable in the GUI.

### Inputs

`wedding-photo/` (in the repo dir, **must be added to `.gitignore`**: it isn't there yet) is two read-only NFS mounts from kDrive (drive 129999) via `rclone nfsmount`, VFS cache capped at 2 × 2.5 GB. Mount/unmount scripts: `scripts/mount_wedding.sh`, `scripts/unmount_wedding.sh` (credentials from `KD_USER`/`KD_PASS`, never written to disk). Reads over the mount are slow: never scan a folder twice when once will do, and cache what you read.

- **`photographer/`**, 626 JPEGs (Nikon D800E) in 6 folders: `preparatifs` 86, `mairie` 122, `groupe` 60, `couple` 108, `cocktail` 147, `diner et soiree` 103. The file creation dates are export dates (22–28 July), not capture times. Whether `DateTimeOriginal` survived is checked in Phase 0. If it didn't, the order inside a folder comes from the camera's file number (`DSC_9562`).
- **`guests/`**, one flat folder: 354 JPEGs and 87 videos. **Videos are ignored.** The JPEGs have **no EXIF at all** (WhatsApp/Signal/Telegram re-encodes). Filenames give the time a file was *sent or saved*, not taken: `WhatsApp Image 2025-07-20 at 18.05.35.jpeg`, `signal-2025-07-20-194953_002.jpeg`, `photo_2025-07-21 20.12.34.jpeg`. When that date is 19 July, it is an **upper bound** on the capture time. Otherwise it says nothing.
- HEIC/HEIF may show up in later uploads. Every format must go through the same decode path and be displayed from the WebP cache.

### Editions (see `docs/adr/0001-editions-as-spread-subsets.md`)

There is one master sequence of Spreads. An Edition = all Core Spreads + its Group's Group Spread.

| Edition | Group | Contents |
|---|---|---|
| `ours` | everyone | every Spread, including all 4 Group Spreads |
| `bride_parents` | `bride_family` | Core + `bride_family` Group Spread |
| `dad` | `groom_dad_family` | Core + `groom_dad_family` Group Spread |
| `mom` | `groom_mom_family` | Core + `groom_mom_family` Group Spread |
| — | `friends` | Group Spread only in `ours` |

There's one page target, for `ours` (default 48 pages). Each family Edition is `ours` minus 3 Spreads (6 pages). The Group Spreads sit together by default in `mairie_after`, where the Group Portraits were taken, so in a family Edition they read as *the* group-photo Spread. They can be moved on the Album page.

### Printer (default, configurable)

Flexilivre, "Grand paysage" 29.7 × 21 cm, hardcover, printed in France, PDF upload. See §9.1.

### Hardware

MacBook Air M1, 8 GB RAM, no GPU. Peak RAM ≤ ~4 GB. Free disk is limited (68 GB at the time of writing, 13 GB of which is the NFS cache); keep the budgets in §1.

### Text

Every Chapter title is in **French and Tunisian Arabic, at about the same size**. The Arabic is written and proofread by Emna and entered in the GUI. The tool never writes Arabic.

---

## 1. Rules for the coding agent (put these in `CLAUDE.md`)

1. **GUI first.** Every feature ships with its GUI. A stage isn't done until it can be started, watched, cancelled and inspected from the browser. Every setting is editable from the browser. CLI subcommands call the same functions.
2. **Use the dev environment.** All native dependencies (vips, libclang, openssl, …) come from `devenv.nix`. Run builds and tests inside `devenv shell` and add missing system packages there, never with Homebrew.
3. **Never load full-size photos into your context.** To check results visually, render a low-res contact sheet or Spread preview (≤ 1200 px wide), one image per check, only when needed. Read text reports, not images.
4. Every stage is idempotent and incremental (it skips Photos already processed at the current `processor_version`).
5. User decisions are Overrides (§2.3). They must survive any regeneration.
6. Memory: decode for analysis at ≤ 1024 px long side (libvips `thumbnail`). Originals are decoded only at final render, one at a time.
7. **Disk budget:** the WebP cache lives in `.iris-cache/webp/` in this repo dir and stays ≤ 300 MB. Print crops go to a temp dir that is deleted after each PDF export. Render and export Editions one at a time. The Pipeline page shows disk use.
8. Never send images to any network service except through the VLM stages (§8), which enforce the privacy settings.
9. Each phase ends with its acceptance check passing before moving on.
10. Follow the architecture principles in §1.1 and the glossary in `CONTEXT.md`.

### 1.1 Architecture principles

The original iris is CLI-oriented: each processor is a batch command that prints progress to a terminal. This project is **interaction-oriented**: the user drives everything from the GUI, often while jobs run, and expects fast feedback.

- **Modules with clear boundaries** (Cargo workspace or top-level modules, agent's choice):
  - `core`: domain types (`Photo`, `Shot`, `Person`, `Group`, `Chapter`, `Spread`, `Template`, `Slot`, `Edition`, `Override`). No I/O.
  - `store`: SQLite persistence behind repository traits.
  - `analysis`: processors (metadata, quality, faces, embeddings, shots, chapters).
  - `album`: selection, templates, fitting, Editions.
  - `render`: renderer implementations.
  - `jobs`: job runner, stage registry, progress.
  - `web`: axum + askama + htmx.
  - `cli`: thin wrapper.
- **Trait-oriented.** Program against traits at every seam, so you can swap components and unit-test with fakes:
  - `Stage` (name, dependencies, version, `is_stale`, `run(ctx, &dyn ProgressSink, &CancelToken)`): every processor and album step implements it.
  - `ProgressSink` (terminal vs GUI).
  - `ImageSource` (decode with orientation, any format incl. HEIC, thumbnails).
  - `FaceAnalyzer`, `ImageEmbedder`, `QualityScorer`, `EyeStateClassifier`.
  - `VisionReviewer` (OpenRouter + fake).
  - `Selector`, `LayoutEngine`, `CropStrategy`.
  - `Renderer` (`render_preview(&SpreadLayout) -> Svg`, `render_print(&[SpreadLayout], &PrinterProfile) -> Pdf`).
  - `PrinterProfile`.
  - repositories (`PhotoRepo`, `PersonRepo`, `OverrideLog`, `JobRepo`, `SettingsRepo`, …).
- **Pure core, effects at the edges.** Selection, layout fitting and Override replay are pure functions of their inputs, so they're fast, testable and safe to re-run on every edit.
- **Interaction latency first.** Anything triggered by a click (swap, Crop, Template, Chapter move, Pin, Quota change, undo) answers in ≤ 1 s: precompute scores and candidate lists in jobs, keep hot data in memory, re-render only what changed. Long work is always a cancellable job with progress, never a blocking request.
- **Errors are visible.** Failures show up in the GUI (job page, per-Photo reasons, conflicts on Spreads), not only in logs.
- **Tests:** unit tests on the pure core (Crop, fitting, selection, Override replay, filename-time parsing), snapshot tests on rendered `SpreadLayout`s, and a small fixture photo set for end-to-end stage runs.

---

## 2. GUI

### 2.1 Stack
- **Server:** axum (`iris serve` → `http://127.0.0.1:3000/`), pages plus `/api/*` fragment/JSON endpoints.
- **Templates:** askama. Pages render full HTML; interactions return fragments swapped by htmx.
- **Client:** htmx 2 + the SSE extension, `Sortable.js` for drag-and-drop, and a small vanilla JS module for the Crop editor and keyboard shortcuts. No Node, no build step.
- **Assets:** vendored in `assets/` (pinned versions, no CDN, works offline), embedded with `rust-embed`.
- **Images in the GUI** always come from the WebP cache (400 px grid, 1200 px preview), never the originals. That covers HEIC/HEIF, which browsers can't show.
- **Styling:** one hand-written CSS file with CSS variables; the album's colour tokens (§9.5) are reused as UI accents.

### 2.2 Settings live in the DB
`config/iris.toml` only bootstraps the app: DB path, the two photo roots, cache dir. Everything else is stored in the DB, seeded with defaults and edited on the Settings page or where it's used: Chapters and folder mapping, Groups, Quotas, Couple Share, density, Hero limit, quality gate thresholds, printer profile, VLM model and budget, fonts and colours. A Settings change is an Override, so it can be undone. "Export settings" / "Import settings" (JSON) on the Settings page provides a backup.

### 2.3 Jobs and progress
- **`ProgressSink`** replaces direct `indicatif` use: `start(total)`, `inc(n)`, `message(..)`, `item_failed(photo_id, reason)`, `finish()`. There are two implementations: `TerminalSink` (indicatif, for the CLI) and `ChannelSink` (a `tokio::sync::broadcast` channel for the GUI).
- **`JobManager`:** a queue with **one worker** (memory), processors run in `spawn_blocking`. States: `queued → running → succeeded | failed | cancelled`. Cancellation via an `AtomicBool` checked between Photos. Tables: `jobs(id, stage, params_json, status, queued_at, started_at, finished_at, done, total, failed, error)` and `job_items(job_id, photo_id, status, reason)`.
- **Stage registry.** Each stage declares its dependencies and `processor_version`. A stage is **stale** when its inputs, settings or version changed since its last successful run.
- **Live updates.** `GET /api/events` is an SSE stream (≤ 4 updates/s per job). A client that reconnects gets the current state first.
- **Concurrency.** SQLite in WAL mode. You can edit while a job runs.

### 2.4 Overrides and undo
- `overrides(id, ts, kind, payload_json, undone)`. Kinds: `label_person`, `merge_persons`, `split_person`, `set_couple`, `set_group`, `set_quota`, `split_shot`, `merge_shots`, `move_chapter`, `pin`, `ban`, `set_hero`, `swap`, `place`, `crop`, `set_template`, `add_spread`, `delete_spread`, `reorder_spread`, `lock_spread`, `unlock_spread`, `set_text`, `set_setting`.
- **Overrides refer to Photos, not Shots or Slots.** A Pin or Ban applies to whichever Shot contains that Photo, so it survives Shot splits and re-runs of the Shot stage.
- **Lock.** Any manual layout change (swap, place, Crop, Template) locks its Spread automatically. The user can also lock or unlock a Spread with the padlock. Unlocking gives the Spread back to the generator. Locked Spreads are fixed points for regeneration. Everything else in the Proposal is disposable.
- **Priority:** Ban > Pin > Rejected. A Pinned Rejected Shot is used; a Banned Shot never is.
- Current state = Proposal + replay of non-undone Overrides. Undo/redo flips `undone` on the latest Override (`⌘Z` / `⇧⌘Z`). An undo history panel lists recent Overrides in plain words ("Pinned DSC_9562", "Quota of Tata Samia → 4").
- If an Override can no longer apply (e.g. the Photo is now in another Chapter), it shows up as a **conflict** on the Spread or the People page, never silently dropped.

### 2.5 The user journey

The navigation follows the order the user works in. Each step shows whether it's done, and what's blocking the next one.

1. **Index**: watch the pipeline run.
2. **People**: name Persons, mark the Couple, fill Groups, set Quotas.
3. **Chapters**: check the Timeline, drag misplaced Guest Photos.
4. **Album**: generate, review, edit Spreads, iterate.
5. **Text & design**: titles, cover, colours, fonts.
6. **Export**: preflight, render, download versioned PDFs.

A header bar shows the running job (stage, progress, ETA, Cancel) on every page, so the user never has to go back to the Pipeline page to know what's happening.

### 2.6 Pages

| Page | Purpose |
|---|---|
| **Pipeline** (home) | Each stage as a card: status, done/total, progress bar, ETA, failed count, last run, stale badge, Run / Re-run / Cancel. "Run all stale" queues them in dependency order. Click a failed count → the Photos with their reasons. Disk use (cache, temp, exports) and the mount status of both folders. VLM stages show the estimated calls and cost, and need a confirmation click. |
| **Photos** | Grid of all Photos, filters: source, Chapter, Person, Group, Rejected (with reason), Banned, Pinned, used/unused in Edition X, Shot. Infinite scroll. Detail drawer: metadata, scores, faces, Chapter probabilities, the other Photos of its Shot; actions Pin, Ban, move Chapter, split Shot, open in Spread. |
| **People** | Persons by face count, with name and the faces. Mark the Couple. Merge (multi-select), split (select faces), confirm soft-assigned faces. **Groups board:** one column per Group, drag Persons in (a Person can be in several). **Quota** per Person (stepper) next to the **actual count** in the current Proposal, red when below Quota. Couple Share shown at the top. Any change re-runs selection in the background and updates counts in place. |
| **Timeline** | One strip per Chapter, thumbnails in order; Guest Photos with low confidence outlined; drag a Photo to another Chapter. |
| **Album** | Edition switcher; all Spreads as thumbnails, in order, with Chapter dividers; Group Spreads badged with their Group; drag to reorder; padlocks; page count vs target; Couple Share and unmet Quotas at a glance. "Regenerate" re-runs selection and layout around Locked Spreads. |
| **Spread editor** | See §2.7. |
| **Text & design** | Chapter titles (FR + Arabic side by side, live RTL preview), cover text, dedication; colours, fonts and motifs with live preview. |
| **Settings** | Everything in §2.2. |
| **Export** | Per Edition: preflight, "Render print PDF", the list of past versions with download/delete (§10). |

### 2.7 Spread editor
- Large preview of the Spread (Typst SVG, §9.4), with the clickable Slots overlaid as HTML.
- **Candidate tray** under the preview: for the selected Slot, alternative Shots ranked by the selector (same Chapter, similar Persons, fits the Slot's aspect and resolution), plus a CLIP text search box ("cutting the cake", "dancing").
- **Interactions:** drag tray → Slot (`place`); Slot → Slot (`swap`); Slot → another Spread in the mini-map (move). Click a Slot to enter Crop mode: pan/zoom, face boxes drawn, warning when a face is cut or effective resolution < 250 ppi. The Template picker only shows Templates compatible with the current Shots. Also: Pin, Ban, mark Hero, lock, delete Spread, insert Spread after.
- Each action posts an Override and returns the re-rendered Spread fragment, in ≤ 1 s.
- Keyboard: ←/→ previous/next Spread, `S` swap mode, `B` ban, `P` pin, `L` lock, `⌘Z`/`⇧⌘Z`.

---

## 3. Phase 0: Fixes and inventory

### 3.1 Bug: EXIF orientation is never applied
`processors/metadata.rs` reads `orientation`, but `image_io::open_image` ignores it. Portrait photos are analysed sideways: face detection misses faces, bboxes are in the raw frame, quality and CLIP see rotated content.
- `open_image` applies the orientation (image 0.25: `ImageDecoder::orientation()` + `DynamicImage::apply_orientation`; for HEIC, `irot`/`imir` or EXIF).
- Store `width`/`height` **after** orientation.
- Bump the versions of `metadata`, `faces`, `quality` and `embed`, and re-run them.
- Unit test with an orientation-6 JPEG fixture.

### 3.2 Capture time and order
- `taken_at_utc` (unix ms, nullable) and `taken_at_source` (`exif_original` | `filename_upper_bound` | `none`).
- Photographer Photos: `DateTimeOriginal` + `SubSecTimeOriginal` + `OffsetTimeOriginal`; without an offset, Europe/Paris (UTC+2 on that date). Also `sequence` = the camera file number (`DSC_9562` → 9562), used for ordering when the time is missing.
- Guest Photos: parse the filename's send/save time (WhatsApp `WhatsApp Image YYYY-MM-DD at HH.MM.SS[ (n)]`, Signal `signal-YYYY-MM-DD-HHMMSS[_nnn]`, Telegram `photo_YYYY-MM-DD HH.MM.SS`). Store it as `sent_at`. It's only used as an upper bound on capture time, and only when it falls on 19 July. Never treat it as the capture time.
- **No Device concept, no clock-offset correction.** Guest Photos have no EXIF to align.

### 3.3 Source
`source` (`photographer` | `guest`) and `source_folder` (the photographer's sub-folder; empty for guests). Videos (`.mp4`, `.mov`) are skipped during discovery and counted on the inventory page.

### 3.4 Inventory
`inventory` stage, shown as a report page: counts by source / folder / format; skipped videos; share of Photos with a capture time; resolution histogram per source; guest `sent_at` histogram per day; exact duplicates (BLAKE3).

**Acceptance:** the orientation test passes; the face count rises after the re-run (before/after on the inventory page); the inventory says whether photographer `DateTimeOriginal` is present.

---

## 4. Phase 1: GUI foundation

- askama + htmx + SSE + rust-embed wiring; base layout, navigation following §2.5, and the running-job header.
- `ProgressSink` refactor of all existing processors; `JobManager`; stage registry with staleness; `jobs` tables; `/api/events`.
- DB-backed settings (§2.2) and the Settings page.
- **Pipeline page** working for the existing stages (scan, metadata, quality, embed, faces, cluster-faces, webp-cache). Drop OCR and geo unless they're harmless.
- **WebP cache stage** in `.iris-cache/webp/`: 400 px and 1200 px, all formats including HEIC/HEIF, ≤ 300 MB total.
- **Photos page** with the filters available so far, and the **Override log** with undo/redo (first used by Pin/Ban).

**Acceptance:** from a fresh DB, the whole index runs from the browser with live progress; a job can be cancelled mid-way and resumed; failures are listed with reasons; the CLI still works with terminal progress bars.

---

## 5. Phase 2: Indexing extensions (each one a GUI stage)

### 5.1 Shots (`shots` stage)
- 64-bit dHash on a 32×32 grey thumbnail → `photos.phash`.
- Two Photos are in the same Shot when Hamming ≤ 8 **or** CLIP cosine ≥ 0.95 (thresholds in Settings). For photographer bursts, also require |Δsequence| small or |Δt| ≤ 10 s. This covers both copies (forwards, re-compressions) and bursts.
- Representative = highest resolution, then highest `jpeg_q_est`, then photographer over guest.
- The user can split or merge Shots on the Photos page (Override).

### 5.2 Quality v2 (`quality_v2` stage)
The current score is a global Tenengrad plus exposure. It rewards busy backgrounds and ignores faces and resolution. Keep the old fields, and add:

| Field | How |
|---|---|
| `megapixels` | from oriented dims |
| `max_print_mm` | longest side printable at 250 ppi = px / 250 × 25.4 |
| `jpeg_q_est` | estimated from the quantisation tables (DQT); WhatsApp ≈ 70–80 |
| `face_sharpness` | Tenengrad on each face crop resized to 128 px; photo value = min over important faces (Couple + largest faces) |
| `eyes_open` | per face: small ONNX eye-state classifier through `ort` on crops placed from the 5-point landmarks; fallback is the VLM review (§8) |
| `clip_aesthetic` | zero-shot with MobileCLIP: "a beautiful professional wedding photograph" vs "a blurry, badly lit amateur photo", "a screenshot", "a photo of a document" |
| `quality_v2` | weighted combination |

**Rejected** (thresholds in Settings): guests `quality_v2 < 0.55` or `jpeg_q_est < 60` or `megapixels < 1.5` or a screenshot; photographer `quality_v2 < 0.35`; any Photo with blurry important faces or the Couple's eyes closed. Each rejection stores a reason. Settings has a **threshold tuning** view: the 30 worst Photos that pass and the 30 best that fail, updating live as the sliders move.

### 5.3 Face assignment (after `cluster-faces`)
- HDBSCAN noise faces go to the nearest Person if cosine distance < threshold, flagged `soft` (to confirm on the People page).
- Faces smaller than 2 % of the image height (background crowd) don't count toward Quotas or the Couple Share.

**Acceptance:** all three stages run from the Pipeline page; the threshold tuning view works.

---

## 6. Phase 3: People, Groups, Quotas

- People page (§2.6): naming, Couple roles (`bride`, `groom`), merge/split, soft-face confirmation.
- Groups seeded empty: `bride_family`, `groom_dad_family`, `groom_mom_family`, `friends`. Each family Group is tied to its Edition; `friends` has no Edition. A Person can be in several Groups. Groups can be renamed or added in the GUI.
- **Quota** per Person, default 0 (the Couple has no Quota; they're covered by the Couple Share). The People page shows actual count vs Quota from the latest Proposal.

**Acceptance:** the Couple are identified with ≥ 95 % recall on a hand-checked sample of 50 Photos (spot-check from the Photos page filtered by Person).

---

## 7. Phase 4: Chapters (`chapters` stage + Timeline page)

### 7.1 Photographer Photos
- Folder → Chapter(s) mapping (seeded from §0, editable). A folder that maps to one Chapter is assigned directly (`preparatifs` → `prep_bride`, `groupe` → `mairie_after`, `couple` → `bnf_couple`, `cocktail` → `cocktail`).
- A folder that maps to several Chapters (`mairie` → `mairie_before`, `mairie_ceremony`, `mairie_after`; `diner et soiree` → `dinner`, `party`) is split into **contiguous blocks** of its ordered sequence (time, else camera file number). The split maximises CLIP zero-shot scores per Chapter ("people waiting in a town hall hallway", "wedding rings exchange in a town hall", "couple leaving a town hall under applause"; "guests seated at a dinner table", "people dancing at a party at night"). This is done by dynamic programming over the sequence, with Chapters in fixed order.

### 7.2 Guest Photos
- **Visual vote:** k-NN (k = 10) on CLIP against Photographer Photos, weighted by similarity.
- **`prep_groom` has no photographer reference.** Score it with CLIP zero-shot prompts ("a groom getting dressed in a suit", "men getting ready in an apartment") and with the presence of the groom without the bride. Any Photo the user moves to `prep_groom` on the Timeline becomes a reference for the others.
- **Time bound:** when `sent_at` falls on 19 July, Chapters that start after `sent_at` are impossible.
- Store the top Chapter, its probability, and the margin to the second. Low confidence (margin < 0.15) → outlined on the Timeline and queued for the VLM tie-break (§8).

### 7.3 Order within a Chapter
Photographer Photos by time (else file number). Guest Photos are inserted next to their nearest Photographer Photo (CLIP).

**Acceptance:** the Timeline shows all 9 Chapters with a believable `prep_groom`; dragging a Photo to another Chapter survives a re-run of the stage.

---

## 8. Phase 5: VLM checks (`vlm_*` stages; budgeted, cached, private)

- Client: `reqwest` → OpenRouter chat completions; model and provider in Settings.
- Privacy on every request: `provider: { order: ["mistral"], allow_fallbacks: false, data_collection: "deny", zdr: true }`. Check the exact field names against the OpenRouter provider-routing docs when implementing. If the model has no ZDR Mistral endpoint, **fail closed** (no request) and say so on the Pipeline page.
- Model: a Mistral vision model (current id from `/api/v1/models`, filtered on image input).
- Images: 768 px long side, JPEG q80, EXIF stripped.
- Cache: `vlm_cache(blake3, task, prompt_version, response_json)`.
- Budget: `max_calls` (default 400); the estimated calls and cost need a confirmation click.

Stages:
1. `vlm_chapter_tiebreak`: low-confidence Guest Photos only.
2. `vlm_pick_review`: every selected Guest Photo and every Hero/cover candidate → `{eyes_closed, mid_blink_or_mid_speech, unflattering, heads_cut, motion_blur, keep_score 0–1, note}`. Flagged picks are shown in the Spread editor with the reason and a one-click "replace with next candidate".
3. `vlm_spread_review` (optional): a low-res Spread render; flags awkward pairings or repeated Persons/poses as notes on the Spread.

---

## 9. Phase 6: Selection, layout and the Spread editor

### 9.1 Printer profiles (seeded from `config/printers/*.toml`, chosen in Settings)

**`flexilivre_a4_landscape_hard` (default)**
- Trim 297 × 210 mm, single pages in the PDF, Cover = first and last page, no crop marks, no spine.
- The first and last interior Pages face the endpapers, so each is a Spread with one blank side.
- Safe zone ≥ 5 mm from the trim for faces and text.
- RGB accepted → export sRGB.
- Page count 24–400; check whether it must be even.
- **To confirm with Flexilivre before the final export:** bleed size and whether the PDF page size includes it; the exact hardcover Cover dimensions (wrap); minimum resolution. Until then, 3 mm bleed as a parameter.

**`saal_digital_a4_landscape` (premium alternative)**: separate Cover and interior PDFs; interior as double-page Spreads, one per PDF page; images ≤ 300 ppi; fonts embedded.

**`blurb_25x20_landscape` (fallback)**: Blurb PDF to Book; not A4.

The layout engine works in Spreads; the exporter writes single Pages or Spreads depending on the profile.

### 9.2 Selection (`select` stage)

**Size.** `ours` page target (default 48) → number of Spreads. Each Chapter's share of Spreads is proportional to its number of usable Shots, then smoothed (min 1 Spread per Chapter, `bnf_couple` and `mairie_ceremony` weighted up). Shares can be adjusted on the Album page.

**Density.** About **6 Shots per Spread** on average, with deliberate variety: some Spreads with 3, some with 9. **At most 3 Heroes** in the whole album, one Page in size by default (settings).

**Objective (greedy, submodular)**, per Chapter. Repeatedly add the Shot that maximises:

```
gain = w_q      · quality_v2
     + w_couple · [Couple present] · face_size_factor · couple_share_pressure
     + Σ_persons w_quota · max(0, quota(p) − count_so_far(p)) / quota(p)    # aim for Quotas
     + Σ_persons w_cover · 1/(1 + count_so_far(p))                          # diminishing coverage
     + w_photographer · [source = photographer]
     − w_div  · max cos_sim(shot, already selected in Chapter)
     − w_time · [same 30 s window as a selected Shot]
```

`couple_share_pressure` grows when the running Couple Share of Core Spreads drops below its target (70 %). Quotas and the Couple Share are **soft**: the selector aims for them, and the People and Album pages show what wasn't met.

Hard constraints: not Banned; not Rejected unless Pinned; one Photo per Shot; printable at the smallest Slot it could get. Pinned Shots are always selected. Locked Spreads are taken as they are, and their Shots count as already selected.

**Group Spreads.** For each Group, a Spread built mainly from Group Portraits (and other Shots) where its members are the majority of the identified faces, aiming to show every member at least once. They're placed together in `mairie_after` by default.

For each Slot, the selector also keeps a ranked list of alternatives, which feeds the candidate tray.

### 9.3 Templates and fitting
Templates are data (seeded from `config/templates/*.toml`): Page or Spread, Slots in mm (with gutter), each Slot with an aspect-ratio range. The starter set (~20) leans toward many photos: 3-, 4-, 5-, 6-, 7-, 9-Shot Spreads in several mixes of portrait/landscape; 1 large + 4 small; mosaic of guest candids; Group Spread (Group Portrait + portraits); Hero (one Shot on one Page, the other Page filled with smaller Shots); text Page (dedication, thank-you).

**Chapter title:** a text block (FR + AR, same size) placed on the first Spread of each Chapter, inside a Template that reserves room for it. Not a dedicated opener Page.

**Face-aware Crop:** for a Shot and a target aspect, choose the Crop that keeps all important faces with headroom (≥ 0.3 face height above), keeps the face centroid near a thirds line, and keeps as much area as possible. Reject the pairing if it would remove > 20 % of the area or cut a face. A user Crop replaces the computed one.

**Template choice per Chapter:** dynamic programming over the ordered Shot sequence (like line-breaking). Page cost = Crop loss + size-vs-importance mismatch (best Shots in the biggest Slots) + repetition penalty for the same Template on facing Spreads + a density-variety reward + DPI penalty (`max_print_mm` vs Slot size; hard fail below 200 ppi). Locked Spreads are fixed points.

### 9.4 Rendering

#### Decision record: which renderer?
Layout (where each Shot goes, how it's cropped) is our own Rust code in every option. The renderer only turns a `SpreadLayout` into a preview and a print PDF, behind the `Renderer` trait.

| Option | Preview | Print PDF | Text / Arabic | Verdict |
|---|---|---|---|---|
| **Typst** (library) | SVG via `typst-svg` | `typst-pdf`, fonts embedded | Real typesetting; Arabic shaping and RTL built in | **Default.** Preview and print come from the same document. Cost: implementing `World`. |
| Plain SVG | browser | `svg2pdf` + `pdf-writer` | preview/print shaping differs | Fallback if Typst blocks. |
| HTML/CSS | best | headless Chromium | excellent | Rejected for print (disk, control, not Rust). |
| Direct PDF | none | full control | by hand | Rejected. |

- Start with `TypstRenderer`. If the `World` setup takes more than a day, shell out to the `typst` CLI (add it to `devenv.nix`) behind the same trait, then switch.
- A golden test renders a fixed `SpreadLayout` with a French and an Arabic title and checks the PDF (page size, embedded fonts, image ppi).
- **Layout and rendering are separate.** `SpreadLayout` (serialisable JSON, all in mm) is computed by our code. Typst only places images at absolute positions and typesets text. Editor overlays are positioned from `SpreadLayout`, not from the rendered output.
- **Preview:** per-Spread SVG using the 1200 px WebP proxies, cached by a hash of the Spread state, rendered on a background thread.
- **Print:** images cropped by libvips to the exact Slot size at 300 ppi (sRGB, JPEG q95, light output sharpening) into a temp dir that's deleted after export.

### 9.5 Design system: the invitation, eighties-style
Keep the invitation's identity (aerial photo of the velodrome, oval track, emerald and red-orange) and restyle it in an **eighties** spirit: fun and colourful, but professional and wedding-appropriate. **Photo areas stay calm** (white or very light Pages, clean gutters). The colour and play live in titles, motifs, margins and page numbers.
- **Colours** (starting points, tuned on the Text & design page): paper `#FFFFFF`, ink `#1E1E1E`, emerald `#3E9C82`, red-orange `#E05A3A`, plus eighties accents (sunset pink, warm yellow, electric blue, used sparingly), light grey `#EDEDED`.
- **Motifs:** velodrome track lanes as **racing stripes** (2–4 parallel coloured lines, the eighties "speed stripes"), sunset gradient bands, a few geometric shapes (Memphis-inspired: dots, zig-zags, half-circles) at small scale in corners. Page numbers inside a small oval track.
- **Type:** titles in a retro display face (chrome/racing or bold geometric eighties feel, OFL), paired with a classic serif for small text. Arabic titles in a display-weight OFL Arabic face of matching weight (e.g. Lalezar, Reem Kufi, Aref Ruqaa), the same size as the French. Compare 2–3 candidates side by side on the Text & design page. Fonts bundled in `assets/fonts/`.
- **Cover** (same for every Edition): one Couple Hero photo, names, "19 juillet 2025 · Paris" in FR and AR; the back Cover has an aerial-velodrome motif with racing stripes.

**Acceptance (Phase 6):** from the GUI: set Quotas, run `select`, open the Album page, edit a Spread (swap, place from tray, Crop, change Template, undo), lock and unlock a Spread, switch Editions, and see each change re-rendered in ≤ 1 s. Re-running `select` keeps every Override and Locked Spread.

---

## 10. Phase 7: Export and order

**Export versions.** Each "Render print PDF" creates an **Export**: `exports/<edition>/v<N>_<date>.pdf` plus a JSON manifest (hash of the album state, Overrides count, page count, preflight results, printer profile). The Export page lists the versions per Edition, with download, "what changed since vN" (Spreads that differ), and delete. Only the latest version per Edition is kept by default (disk); older ones can be kept explicitly.

1. Preflight per Edition: page size incl. bleed, page count parity, every image ≥ 250 effective ppi, fonts embedded, no face or text in the 5 mm safe zone. Errors link to the Spread.
2. Order **one** copy of `ours` as a proof. Check colours (skin tones, the emerald, the eighties accents), Crop safety, Cover alignment, Arabic rendering.
3. Adjust (brightness/colour offset in the printer profile if prints come out dark).
4. Export the other Editions one at a time, order, then delete local PDFs once uploaded.

---

## 11. CLI (thin wrappers, mainly for the coding agent)

```
iris serve                               # the app: http://127.0.0.1:3000/
iris index                               # existing pipeline (+ orientation fix, shots, quality v2)
iris album inventory
iris album chapters [--report]
iris album select
iris album vlm <chapter_tiebreak|pick_review|spread_review> [--dry-run]
iris album render  --edition E (--preview | --print) [--printer P]
iris album preflight <file.pdf> --printer P
```

---

## 12. Open decisions

- Bleed and Cover specs: ask Flexilivre support before the final export (§9.1).
- Final `ours` page count after the first build (48 is a starting point; Flexilivre's 34.90 € hardcover price covers 24–30 pages, extra pages cost more).
- Which display faces (Latin and Arabic) best fit the eighties-velodrome look.
- Whether photographer `DateTimeOriginal` survived the export (Phase 0 inventory).
- Deadline (Christmas gift?) → work back from Flexilivre's production and delivery time, plus one proof cycle.

---

## 13. As built (implementation notes)

Where the implementation differs from the text above, this section wins.

- **Workspace:** `crates/iris-core` is the pure core (no I/O, fast tests); the app is the root crate.
- **One read per original:** the `ingest` stage reads each file once (hash, EXIF, JPEG quality estimate, 400/1200 px WebP proxies). Every later analysis uses the proxies, never the slow mount.
- **Stages:** scan → ingest → quality → embed → faces → cluster → shots → chapters → select, plus optional `vlm_chapter_tiebreak`, `vlm_pick_review` and one `export:<edition>` per Edition. Staleness = version, a dependency ran since, or the settings/decisions the stage reads changed.
- **Quality v2 and Rejected** are computed on the fly (`iris_core::quality`) from stored measurements, so the gate thresholds and the Couple apply at once. No eye-state model is bundled: closed eyes come only from the optional Mistral review.
- **People:** clusters are raw; Persons are clusters with the People Overrides replayed. Every People Override names a face (its anchor), so names, roles, Groups and Quotas survive re-clustering.
- **Layout:** instead of a fixed library of ~20 slot grids, a page Template is a justified arrangement (`rows:2-3`, `cols:1-2`, `hero`) computed from the photos' own aspect ratios: no crop except Heroes (face-aware). Spreads hold 3–9 Shots; a dynamic program cuts each Chapter into its Spread budget.
- **Edits = Locks:** every layout edit stores the edited Spread as a `LockSpread` Override; the shown album is the generated Proposal with Locks applied, so edits show at once and survive rebuilds.
- **Rendering:** the Typst CLI behind the `Renderer` trait (the plan's fallback). Previews are PNGs cached by a hash of what they show. Print: originals cropped by libvips at 300 ppi into a temp dir, single pages with 3 mm bleed, cover first and last, temp files deleted after.
- **Low-resolution photos:** a Slot is capped so its photo prints at ≥ 250 ppi (`fit_resolution`; the photo shrinks and is centred); the Spread editor shows each Slot's ppi. Hero Slots are not capped yet.
- **Page geometry:** margins 10 mm (12 mm at the bottom, for the page number), 3 mm gaps, an 18 mm title band under the top margin. Bleed (3 mm) grows edge Slots and their Crop at the same scale.
- **Design (supersedes the motifs of §9.5):** calm and editorial. Titles side by side, French left and Tunisian Arabic right at the same size (Righteous / Lalezar), over a hairline with a short emerald accent. No racing stripes, dots or oval page numbers: plain page numbers in the outer corners (white over a full-bleed photo). The back cover keeps the velodrome as two fine emerald ovals. The eighties live in the title faces and the accent colour. Default Arabic titles are in Tunisian.
- **Fonts:** print fonts are static instances (`assets/fonts/`); Typst mis-renders variable fonts. The variable originals serve the web UI (`assets/web-fonts/`).
- **Conflicts:** a Banned Photo on a Locked Spread warns on that Spread; a Pinned Photo with no room is listed on the Album page and in preflight. A Group with fewer than 3 photos gets no Group Spread and gives its Spread back to the Chapters. Other Conflicts (moved Chapter, People Overrides that no longer resolve) are not shown yet.
- **Preflight:** reads the PDF as written (page size with bleed, even interior of 24–400 pages, every font embedded), adds Typst warnings, titles missing a language, low ppi, cut faces, faces in the safe zone and Conflicts; each problem links to its Spread. The golden PDF test checks size, page count and both title fonts; image ppi is checked from the layout, not the PDF.
- **Exports:** `exports/<edition>/v<N>_<date>.pdf` + a manifest with a deterministic album-state hash, pages, fonts, preflight problems and Spread ids. The Export page compares that hash for freshness; the latest file and explicitly kept older versions remain on disk and in history, while other older files and history entries are pruned together.
- **Not done yet:** `vlm_spread_review`; Saal and Blurb printer profiles (they fail with a clear message); drag-and-drop of a Spread across Chapters.
