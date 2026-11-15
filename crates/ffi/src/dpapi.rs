//! Windows: protects the vault master key with DPAPI (bound to the Windows
//! user account), for "Unlock with Windows Hello". The app asks Windows Hello
//! before it calls `unprotect`.
//!
//! What this does and doesn't protect against: DPAPI ties the data to this
//! Windows account on this PC, so a copied vault file or a stolen disk image
//! can't be opened without the account's credentials. A program already
//! running as the same Windows user could call DPAPI too; the Windows Hello
//! prompt is a check by MyTerm, not by the encryption. The master password
//! remains the stronger unlock. (TPM-bound keys via Windows Hello's
//! KeyCredentialManager are the planned next step.)

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    /// Extra entropy so other DPAPI users can't trivially decrypt our blob.
    const ENTROPY: &[u8] = b"MyTerm vault device key v1";

    fn blob(d: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: d.len() as u32, pbData: d.as_ptr() as *mut u8 }
    }

    fn take(out: CRYPT_INTEGER_BLOB, wipe: bool) -> Vec<u8> {
        // SAFETY: DPAPI returned a LocalAlloc'd buffer of cbData bytes.
        unsafe {
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            if wipe {
                std::ptr::write_bytes(out.pbData, 0, out.cbData as usize);
            }
            LocalFree(out.pbData as _);
            v
        }
    }

    pub fn protect(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: all pointers are valid for the call; out is filled by DPAPI.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            anyhow::bail!("Windows couldn't protect the key: {}", std::io::Error::last_os_error());
        }
        Ok(take(out, false))
    }

    pub fn unprotect(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: as above.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            anyhow::bail!("Windows couldn't unlock the key: {}", std::io::Error::last_os_error());
        }
        Ok(take(out, true))
    }
}

#[cfg(windows)]
pub use imp::{protect, unprotect};

#[cfg(not(windows))]
pub fn protect(_data: &[u8]) -> anyhow::Result<Vec<u8>> {
    anyhow::bail!("only used on Windows")
}

#[cfg(not(windows))]
pub fn unprotect(_data: &[u8]) -> anyhow::Result<Vec<u8>> {
    anyhow::bail!("only used on Windows")
}
