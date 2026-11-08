use sha2::{Digest, Sha256};

/// Deterministic, bounded filesystem component for an opaque native identity.
pub(crate) fn identity_component(native_id: &str) -> String {
    if is_uuid(native_id) {
        return native_id.to_owned();
    }
    const MAX_INLINE_ID_BYTES: usize = 118;
    if native_id.len() > MAX_INLINE_ID_BYTES {
        return format!("id-sha256-{:x}", Sha256::digest(native_id.as_bytes()));
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut component = String::with_capacity(3 + native_id.len() * 2);
    component.push_str("id-");
    for byte in native_id.bytes() {
        component.push(HEX[usize::from(byte >> 4)] as char);
        component.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    component
}

fn is_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Key under which two names or paths denote the same file on a
/// case-insensitive, normalization-insensitive filesystem (APFS default):
/// NFC-normalized, then lowercased.
pub(crate) fn filesystem_key(name: &str) -> String {
    nfc(name).to_lowercase()
}

/// Canonical spelling of generated View paths, independent of filesystem rules.
pub(crate) fn nfc(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfc().collect()
}
