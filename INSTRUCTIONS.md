# IRIS: Image Retrieval & Intelligent Sorting

Build a local-first Rust photo indexer.

This phase should produce a working CLI that:
1. Recursively scans image directories configured in a TOML config file.
2. Stores discovered images in SQLite.
3. Uses BLAKE3 as canonical image identity.
4. Extracts metadata.
5. Generates MobileCLIP embeddings.
6. Runs lazy OCR only on likely text-heavy images.
7. Computes basic image quality metrics.
8. Stores vectors using sqlite-vec.
9. Can be safely rerun without duplicating work.

# This is phase 1

Do not build the web UI yet.
Do not implement duplicates, events, or face clustering yet.
Do not add authentication, cloud sync, video support, or tagging.

## Tech Stack

Use Rust.

Required crates:

- `rusqlite`
- `sqlite-vec`
- `blake3`
- `walkdir`
- `image`
- `imageproc`
- `kamadak-exif`
- `open_clip_inference`
- `ocr-rs`
- `anyhow`
- `chrono`
- `serde`
- `tracing`
- `heic` to deal with HEIC images
- `reverse_geocoder` for offline reverse geocoding
- `isocountry` for country names from ISO country codes

Use `sqlite-vec` from:
https://github.com/asg017/sqlite-vec

Use MobileCLIP model:
`RuteNL/MobileCLIP2-S3-OpenCLIP-ONNX`

Use OCR models:
- `PP-OCRv5_mobile_det.mnn`
- `latin_PP-OCRv5_mobile_rec_infer.mnn`
- `ppocr_keys_latin.txt`

from:
https://github.com/zibo-chen/rust-paddle-ocr/tree/next/models

## Goal

Discover and track photos from configured library directories, store information in an sqlite + sqlite-vec database.
Write the whole code in Rust.


The goal is to scan a directory of images and to extract: 
metadata + embeddings + OCR + quality.

You will need those crates for the database. 
cargo add rusqlite
cargo add sqlite-vec

## more info here
https://github.com/asg017/sqlite-vec

## Responsibilities

### File discovery scanning (discovery.rs)

Scan configured TOML `library_paths`:

e.g. all the files in 

```
./Photos
```

for:

* jpg
* jpeg
* JPG
* png
* heic
* webp

Ignore videos or other files! 

For Heic use: the HEIC crate https://docs.rs/heic/latest/heic/ 

### Application config

Use a TOML config for the application, for example:

```toml
database_path = "iris.db"
library_paths = ["./Photos"]
ocr_models_dir = "./models"
ocr_edge_density_threshold = 0.08
scan_batch_size = 500
process_batch_size = 128
embedding_dimensions = 768
```

### Incremental detection

Detect:

* new files
* modified files
* deleted/missing files

Deleted files must be marked as missing and preserved in the database.

Use `mtime` and file size as the fast check before recomputing the BLAKE3 hash.

Use:

```
BLAKE3 hash (see below)
```
Use rust crate blake3 as canonical content identity. Phase 1 stores one row per image path.

https://docs.rs/blake3/latest/blake3/



### Metadata extraction (metadata.rs)


Extract deterministic metadata from files.

* timestamp
* GPS
* camera model
* orientation
* dimensions

use the Rust crate exif.

https://docs.rs/kamadak-exif/latest/exif/


### Embedding extraction (embedding.rs)

## Goal

Generate semantic representations of images.


Use the rust open-clip-inference-rs crate:

https://github.com/RuurdBijlsma/open-clip-inference-rs/

Example usage:
 
```rust
use open_clip_inference::{VisionEmbedder, TextEmbedder, Clip};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model_id = "RuteNL/MobileCLIP2-S3-OpenCLIP-ONNX";
    let vision = VisionEmbedder::from_model_id(model_id).build()?;
    let text = TextEmbedder::from_model_id(model_id).build()?;

    let img = image::open(Path::new("assets/img/cat_face.jpg"))?;
    let img_emb = vision.embed_image(&img)?;
    // Now you may put the embeddings in a database like Postgres with PgVector to set up semantic image search.

    let text_embs = text.embed_text("a cat")?;
    // You can search with the text embedding through images using cosine similarity.
    // All embeddings produced are already l2 normalized.

    Ok(())
}
```

Use the model: RuteNL/MobileCLIP2-S3-OpenCLIP-ONNX

Generate in the sqlite :

```txt
embedding: vector<float16>
```

### OCR text embedding

If OCR exists, i.e if there is some text:

* embed OCR text
* same embedding space
* store separately from the image embedding for the same photo


### OCR (ocs.rs)

Extract searchable text from images. Don't compute OCR every time, use a lazy OCR computation based on an edge-density heuristic. Start with threshold `0.08` and make it configurable from the TOML config.


### OCR extraction

Use the rust crate ocr_rs that uses PaddleOCR

https://docs.rs/ocr-rs/latest/ocr_rs/

With local model files under `models/`, downloaded from: https://github.com/zibo-chen/rust-paddle-ocr/tree/next/models 

```
## OCR MODEL TO USE
PP-OCRv5_mobile_det.mnn
latin_PP-OCRv5_mobile_rec_infer.mnn
ppocr_keys_latin.txt
```

### Text cleanup

Very basic text cleanup:

* whitespace
* encoding
* line breaks

### OCR confidence

If the API allows an OCR confidence, use: 
Store:

```txt
ocr_confidence
```

otherwiser just boolean ? 

Also store the text embedding and the raw text 

# Quality Analysis Module (image_quality.rs)

## Goal

Detect low-quality images.

### Blur detection

Compute:

```txt
Tenengrad sharpness
```

use the imageproc crate.

example implementation:

```

use image::{DynamicImage, imageops::FilterType};
use imageproc::gradients::{horizontal_sobel, vertical_sobel};

pub fn tenengrad_sharpness(img: &DynamicImage) -> f64 {
    let img = resize_max_dim(img, 512);
    let gray = img.to_luma8();

    let gx = horizontal_sobel(&gray);
    let gy = vertical_sobel(&gray);

    let (w, h) = gray.dimensions();

    if w == 0 || h == 0 {
        return 0.0;
    }

    let mut sum = 0.0;

    for y in 0..h {
        for x in 0..w {
            let sx = gx.get_pixel(x, y)[0] as f64;
            let sy = gy.get_pixel(x, y)[0] as f64;

            sum += sx * sx + sy * sy;
        }
    }

    sum / (w as f64 * h as f64)
}

fn resize_max_dim(img: &DynamicImage, max_dim: u32) -> DynamicImage {
    let w = img.width();
    let h = img.height();

    if w <= max_dim && h <= max_dim {
        return img.clone();
    }

    let scale = max_dim as f32 / w.max(h) as f32;
    let new_w = (w as f32 * scale).round() as u32;
    let new_h = (h as f32 * scale).round() as u32;

    img.resize(new_w.max(1), new_h.max(1), FilterType::Triangle)
}
```


### Exposure analysis

Detect:

* overexposed
* underexposed

### Screenshot detection (screenshot_detection.rs)

Heuristics:

* OCR density
* edge density
* UI geometry

### Quality scoring

Generate:

```txt
quality_score
```

## Outputs

Updates:

```sql
photos.blur_score
photos.screenshot_score
photos.quality_score
```

### Database registration

Insert photo entry if unseen.

Creates the sqlite database if it doesn't exist, make sure to use sqlite-vec extension 


You can do a simple CLI with 

cargo run -- scan
cargo run -- metadata
cargo run -- geo
cargo run -- embed
cargo run -- ocr
cargo run -- quality
cargo run -- all

all is doing scan -> metadata -> geo -> quality -> embeddings -> lazy OCR

You are in a test environement with a Photos/ directory containing a few of them. Bear in mind that in reallity I need to index 30000 pictures.


About code quality:

Yes. Add a small **code quality / architecture** section. Keep it strict so the assistant does not produce spaghetti.

You can add this:

````md
## Code Quality Requirements

Keep the code simple, explicit, and trait-oriented.

### General Rules

- Prefer small modules with clear responsibilities.
- Avoid large “god” structs.
- Avoid global mutable state.
- Avoid clever abstractions.
- Prefer readable code over generic code.
- Every module should expose a small public API.
- Use `anyhow::Result` at application boundaries.
- Use specific error types only if it makes the code clearer.
- Processing should be resumable and idempotent.
- A failed image must not stop the whole pipeline.

### Trait-Oriented Design

Use traits for pipeline components so modules are easy to test and replace.

Suggested traits:

```rust
pub trait PhotoProcessor {
    fn name(&self) -> &'static str;
    fn process(&self, ctx: &AppContext, photo: &Photo) -> anyhow::Result<()>;
}
````

For batch processors:

```rust
pub trait BatchProcessor {
    fn name(&self) -> &'static str;
    fn run(&self, ctx: &AppContext) -> anyhow::Result<()>;
}
```

Example implementations:

```rust
pub struct MetadataProcessor;
pub struct QualityProcessor;
pub struct ImageEmbeddingProcessor;
pub struct LazyOcrProcessor;
```

Each should implement either `PhotoProcessor` or `BatchProcessor`.

### App Context

Use a shared context object:

```rust
pub struct AppContext {
    pub config: Config,
    pub db: Database,
}
```

Keep it boring. No service locator magic.

### Database Access

Do not spread raw SQL everywhere.

Create a `Database` wrapper:

```rust
pub struct Database {
    conn: rusqlite::Connection,
}
```

Expose methods like:

```rust
impl Database {
    pub fn upsert_photo(&self, input: NewPhoto) -> anyhow::Result<i64>;
    pub fn mark_photo_missing(&self, photo_id: i64) -> anyhow::Result<()>;
    pub fn photos_missing_metadata(&self, limit: usize) -> anyhow::Result<Vec<Photo>>;
    pub fn save_metadata(&self, photo_id: i64, metadata: PhotoMetadata) -> anyhow::Result<()>;
    pub fn save_quality(&self, photo_id: i64, quality: ImageQuality) -> anyhow::Result<()>;
    pub fn save_ocr_result(&self, photo_id: i64, result: OcrResult) -> anyhow::Result<()>;
    pub fn save_image_embedding(&self, photo_id: i64, embedding: &[f32]) -> anyhow::Result<()>;
}
```

The rest of the app should not manually write SQL unless necessary.

### Data Types

Use explicit structs instead of passing loose tuples.

```rust
pub struct Photo {
    pub id: i64,
    pub blake3_hash: String,
    pub path: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

pub struct PhotoMetadata {
    pub taken_at: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub camera_model: Option<String>,
    pub orientation: Option<i32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

pub struct ImageQuality {
    pub tenengrad_sharpness: f64,
    pub exposure_score: f64,
    pub screenshot_score: f64,
    pub quality_score: f64,
}

pub struct OcrResult {
    pub raw_text: String,
    pub cleaned_text: String,
    pub confidence: Option<f64>,
}
```

### Pipeline Design

The pipeline should be explicit:

```rust
pub fn run_all(ctx: &AppContext) -> anyhow::Result<()> {
    scanner::scan(ctx)?;
    metadata::run(ctx)?;
    geo::run(ctx)?;
    image_quality::run(ctx)?;
    embeddings::run_image_embeddings(ctx)?;
    ocr::run_lazy_ocr(ctx)?;
    embeddings::run_ocr_text_embeddings(ctx)?;
    Ok(())
}
```

Avoid hidden background work.

### Logging

Use `tracing`.

Log:

* number of files scanned
* number of new images
* number of missing images
* number of OCR skipped/done/failed
* number of embeddings generated
* per-file errors at warning level

### Testing

Add unit tests for pure logic:

* BLAKE3 hashing helper
* image extension detection
* OCR likelihood rule
* Tenengrad sharpness function
* exposure score
* text cleanup
* path scanning filter

Do not require real ML models in unit tests.

### Performance Rules

* Decode each image at most once per processor.
* Resize before expensive image-quality operations.
* Use batch database writes inside transactions.
* Do not recompute embeddings if already done.
* Do not rerun OCR if `ocr_status = done` or `skipped`.

### Style

Run before completion:

```bash
cargo fmt
cargo clippy -- -D warnings
cargo test
```

````
I would especially keep these three constraints:

```md
- Do not spread raw SQL everywhere.
- Use traits for replaceable processors.
- Keep the pipeline explicit and resumable.
````



What are the things I should clarify ?
