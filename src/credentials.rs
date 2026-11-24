//! Acceso directo al Administrador de credenciales de Windows.
//!
//! Se usa el mismo `TargetName`/`UserName` que la app Python
//! (`StreamHub` / `oauth_token`) para que el token existente se reutilice sin
//! volver a autenticarse. El blob se guarda como UTF-16LE, igual que keyring
//! de Python.

use anyhow::{bail, Result};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};

const SERVICE: &str = "StreamHub";
const USER: &str = "oauth_token";

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn load_token() -> Option<String> {
    let target = wide(SERVICE);
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if ok == 0 || credential.is_null() {
        return None;
    }

    let value = unsafe {
        let cred = &*credential;
        let value = if cred.CredentialBlob.is_null() || cred.CredentialBlobSize == 0 {
            String::new()
        } else {
            let bytes =
                std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize);
            decode_blob(bytes)
        };
        CredFree(credential as *const _);
        value
    };

    let value = value.trim().trim_start_matches('\u{feff}').to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

pub fn save_token(token: &str) -> Result<()> {
    let token = token.trim();
    if token.is_empty() {
        bail!("el token está vacío");
    }

    let target = wide(SERVICE);
    let username = wide(USER);
    let mut blob: Vec<u8> = token
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();

    let mut credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_ptr() as *mut u16,
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: username.as_ptr() as *mut u16,
        ..Default::default()
    };

    #[allow(clippy::unnecessary_mut_passed)]
    let ok = unsafe { CredWriteW(&mut credential, 0) };
    if ok == 0 {
        bail!(
            "no se pudo escribir en el Administrador de credenciales: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

pub fn delete_token() -> Result<()> {
    let target = wide(SERVICE);
    let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
            bail!(
                "no se pudo borrar la credencial: {}",
                error
            );
        }
    }
    Ok(())
}

fn decode_blob(bytes: &[u8]) -> String {
    // keyring de Python escribe UTF-16LE sin BOM. Para tokens ASCII, los bytes
    // impares son cero; si no lo son, asumimos UTF-8.
    if bytes.len() >= 2 && bytes.len().is_multiple_of(2) {
        let half = bytes.len() / 2;
        let zero_high_bytes = bytes
            .iter()
            .skip(1)
            .step_by(2)
            .filter(|byte| **byte == 0)
            .count();
        if zero_high_bytes * 2 >= half {
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            return String::from_utf16_lossy(&units)
                .trim_start_matches('\u{feff}')
                .to_string();
        }
    }
    String::from_utf8_lossy(bytes).to_string()
}
