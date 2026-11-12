//! Petits utilitaires de fichiers partagés par les apps Iris.
use serde_json::Value;
use std::path::{Path, PathBuf};

/// `%APPDATA%\<app>`
pub fn data_dir(app: &str) -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join(app)
}
/// `Documents\<app>`
pub fn documents_dir(app: &str) -> PathBuf {
    dirs::document_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Documents")).join(app)
}
/// `Musique\<app>`
pub fn music_dir(app: &str) -> PathBuf {
    dirs::audio_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Music")).join(app)
}

/// Lit un JSON (tolère le BOM UTF-8). `None` si absent ou illisible.
pub fn read_json(f: &Path) -> Option<Value> {
    let s = std::fs::read_to_string(f).ok()?;
    serde_json::from_str(s.trim_start_matches('\u{feff}')).ok()
}

/// Écrit un JSON de façon atomique (fichier temporaire puis renommage), en créant le dossier.
pub fn write_json(f: &Path, v: &Value) -> std::io::Result<()> {
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut tmp = f.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default())?;
    std::fs::rename(&tmp, f)
}

/// Ouvre un fichier ou un dossier dans l'Explorateur.
pub fn open_path(p: &Path) {
    let _ = std::process::Command::new("explorer").arg(p).spawn();
}
/// Ouvre l'Explorateur avec le fichier sélectionné.
pub fn show_in_folder(p: &Path) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
}
/// Ouvre une URL http(s) dans le navigateur par défaut.
pub fn open_url(url: &str) {
    if url.starts_with("http://") || url.starts_with("https://") {
        let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn json_roundtrip() {
        let d = std::env::temp_dir().join(format!("iris-store-test-{}", std::process::id()));
        let f = d.join("a.json");
        let v = serde_json::json!({ "x": 1, "s": "é" });
        super::write_json(&f, &v).unwrap();
        assert_eq!(super::read_json(&f), Some(v));
        let _ = std::fs::remove_dir_all(d);
    }
}
