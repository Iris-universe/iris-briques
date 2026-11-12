//! Secrets (mots de passe, jetons, clés d'API) chiffrés avec DPAPI : seul l'utilisateur Windows courant peut les relire.
//! + reprise des secrets d'une ancienne app Electron (safeStorage = AES-256-GCM, clé dans « Local State » protégée par DPAPI).
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use std::path::Path;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};

const PREFIX: &str = "dpapi:";

fn dpapi(data: &[u8], protect: bool) -> Option<Vec<u8>> {
    unsafe {
        let mut input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = if protect {
            CryptProtectData(&mut input, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), 0, &mut out)
        } else {
            CryptUnprotectData(&mut input, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), 0, &mut out)
        };
        if ok == 0 || out.pbData.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        Some(v)
    }
}

/// Chiffre un secret ("" reste "").
pub fn protect(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    dpapi(s.as_bytes(), true).map(|v| format!("{PREFIX}{}", B64.encode(v))).unwrap_or_default()
}

/// Déchiffre un secret produit par [`protect`] ("" si illisible).
pub fn unprotect(s: &str) -> String {
    let Some(b) = s.strip_prefix(PREFIX) else { return String::new() };
    B64.decode(b).ok().and_then(|v| dpapi(&v, false)).and_then(|v| String::from_utf8(v).ok()).unwrap_or_default()
}

/// Clé safeStorage d'une app Electron (`<userData>ocal state`).
pub fn electron_key(user_data: &Path) -> Option<Vec<u8>> {
    let v = iris_store::read_json(&user_data.join("Local State"))?;
    let enc = B64.decode(v["os_crypt"]["encrypted_key"].as_str()?).ok()?;
    dpapi(enc.strip_prefix(b"DPAPI")?, false)
}

/// Déchiffre une valeur `safeStorage.encryptString(...).toString('base64')` d'Electron.
pub fn electron_decrypt(key: &[u8], b64: &str) -> String {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};
    (|| {
        let raw = B64.decode(b64).ok()?;
        let body = raw.strip_prefix(b"v10")?;
        if body.len() < 12 + 16 {
            return None;
        }
        let cipher = Aes256Gcm::new_from_slice(key).ok()?;
        let plain = cipher.decrypt(Nonce::from_slice(&body[..12]), &body[12..]).ok()?;
        String::from_utf8(plain).ok()
    })()
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        let c = super::protect("secret é");
        assert!(c.starts_with("dpapi:"));
        assert_eq!(super::unprotect(&c), "secret é");
        assert_eq!(super::unprotect(""), "");
    }
}

/// Relit un secret stocké soit par [`protect`], soit par safeStorage d'Electron (clé de `electron_user_data`).
pub fn unprotect_any(s: &str, electron_user_data: &Path) -> String {
    if s.starts_with(PREFIX) {
        return unprotect(s);
    }
    if s.is_empty() {
        return String::new();
    }
    electron_key(electron_user_data).map(|k| electron_decrypt(&k, s)).unwrap_or_default()
}
