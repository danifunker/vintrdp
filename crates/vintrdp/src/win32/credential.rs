//! Saved passwords in Credential Manager, looked up at run time: it arrived
//! in Windows XP, and on anything older passwords simply aren't saved.
//!
//! Entries are written exactly as the `keyring` crate wrote them before (a
//! generic credential named `{id}.vintrdp`, user name `{id}`, the password as
//! UTF-16LE, enterprise persistence), so passwords saved by earlier versions
//! still load.

use std::ffi::c_void;

use windows::Win32::Security::Credentials::{CRED_PERSIST_ENTERPRISE, CRED_TYPE_GENERIC, CREDENTIALW};
use windows::core::PWSTR;

use super::proc;

type CredWriteW = unsafe extern "system" fn(*const CREDENTIALW, u32) -> i32;
type CredReadW = unsafe extern "system" fn(*const u16, u32, u32, *mut *mut CREDENTIALW) -> i32;
type CredDeleteW = unsafe extern "system" fn(*const u16, u32, u32) -> i32;
type CredFree = unsafe extern "system" fn(*const c_void);

/// CRED_MAX_CREDENTIAL_BLOB_SIZE
const MAX_BLOB: usize = 5 * 512;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn target(id: &str, service: &str) -> Vec<u16> {
    wide(&format!("{id}.{service}"))
}

/// Whether this Windows can keep passwords.
pub fn available() -> bool {
    // SAFETY: advapi32's exports with their prototypes.
    unsafe {
        proc::<CredWriteW>("advapi32.dll", "CredWriteW").is_some()
            && proc::<CredReadW>("advapi32.dll", "CredReadW").is_some()
            && proc::<CredDeleteW>("advapi32.dll", "CredDeleteW").is_some()
            && proc::<CredFree>("advapi32.dll", "CredFree").is_some()
    }
}

pub fn save(service: &str, id: &str, password: &str) -> Result<(), String> {
    // SAFETY: advapi32's export with its prototype.
    let Some(write) = (unsafe { proc::<CredWriteW>("advapi32.dll", "CredWriteW") }) else {
        return Err("this version of Windows has no Credential Manager".into());
    };
    let mut blob: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
    if blob.len() > MAX_BLOB {
        return Err("the password is too long".into());
    }
    let (mut name, mut user, mut empty, mut empty2) = (target(id, service), wide(id), wide(""), wide(""));
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(name.as_mut_ptr()),
        Comment: PWSTR(empty.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_ENTERPRISE,
        TargetAlias: PWSTR(empty2.as_mut_ptr()),
        UserName: PWSTR(user.as_mut_ptr()),
        ..Default::default()
    };
    // SAFETY: the structure and everything it points to outlive the call.
    let ok = unsafe { write(&credential, 0) } != 0;
    let error = std::io::Error::last_os_error();
    blob.fill(0);
    if ok { Ok(()) } else { Err(error.to_string()) }
}

pub fn load(service: &str, id: &str) -> Option<String> {
    // SAFETY: advapi32's exports with their prototypes.
    let (read, free) = unsafe { (proc::<CredReadW>("advapi32.dll", "CredReadW")?, proc::<CredFree>("advapi32.dll", "CredFree")?) };
    let name = target(id, service);
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: a NUL-terminated name; on success we own `credential` until CredFree.
    unsafe {
        if read(name.as_ptr(), CRED_TYPE_GENERIC.0, 0, &mut credential) == 0 || credential.is_null() {
            return None;
        }
        let c = &*credential;
        let blob = if c.CredentialBlob.is_null() { &mut [][..] } else { std::slice::from_raw_parts_mut(c.CredentialBlob, c.CredentialBlobSize as usize) };
        let password = (blob.len() % 2 == 0)
            .then(|| String::from_utf16(&blob.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect::<Vec<_>>()).ok())
            .flatten();
        blob.fill(0);
        free(credential.cast());
        password
    }
}

pub fn delete(service: &str, id: &str) {
    // SAFETY: advapi32's export with its prototype, and a NUL-terminated name.
    unsafe {
        if let Some(delete) = proc::<CredDeleteW>("advapi32.dll", "CredDeleteW") {
            delete(target(id, service).as_ptr(), CRED_TYPE_GENERIC.0, 0);
        }
    }
}
