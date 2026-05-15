#[derive(Debug, Clone)]
pub struct Photo {
    pub id: i64,
    pub blake3_hash: String,
    pub path: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct NewPhoto {
    pub path: String,
    pub blake3_hash: String,
    pub file_size: i64,
    pub modified_at_unix: i64,
}

#[derive(Debug, Clone)]
pub struct PhotoMetadata {
    pub taken_at: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub camera_model: Option<String>,
    pub orientation: Option<i32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct GeoCandidate {
    pub id: i64,
    pub gps_lat: f64,
    pub gps_lon: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeoLocation {
    pub city: Option<String>,
    pub region: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ImageQuality {
    pub tenengrad_sharpness: f64,
    pub exposure_score: f64,
    pub screenshot_score: f64,
    pub quality_score: f64,
}

#[derive(Debug, Clone)]
pub struct OcrResult {
    pub raw_text: String,
    pub cleaned_text: String,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct FaceDetection {
    pub face_index: i64,
    pub bbox_x1: f64,
    pub bbox_y1: f64,
    pub bbox_x2: f64,
    pub bbox_y2: f64,
    pub detection_score: f64,
    pub landmarks_json: Option<String>,
    pub gender: Option<String>,
    pub age: Option<u8>,
    pub embedding: Vec<f32>,
}
