use std::{fs::File, io::Read, path::Path};

use anyhow::{Context, Result};

pub fn blake3_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 1024 * 64];

    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn hashes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.txt");
        let mut file = File::create(&path).unwrap();
        write!(file, "iris").unwrap();

        assert_eq!(
            blake3_file(&path).unwrap(),
            blake3::hash(b"iris").to_hex().to_string()
        );
    }
}
