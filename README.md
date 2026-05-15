# IRIS: Image Retrieval & Intelligent Sorting

IRIS is a simple yet powerful tool to organize your photos, it has 4 main functions:

## Indexing and feature extraction:

Scans photo directories recursively and looks for images, supports heic, jpeg, png and webp.
For every image a feature extraction step is performed and stored in a database:

- Unique signation using BLAKE3 
- EXIF -> DEVICE, TIME and
- GEOGRAPHY with geo
- CLIP EMBEDDING -> Semantic extraction
- OCR -> Analyze the text present in the image
- IMAGE QUALITY assesment
- FACE RECOGNITION AND IDENTIFICATION 
