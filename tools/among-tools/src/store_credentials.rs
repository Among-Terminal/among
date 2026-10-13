//! The Snap Store credential that `snapcraft export-login` produces, and the
//! `Authorization` header the Store's HTTPS API expects from it.
//!
//! `SNAPCRAFT_STORE_CREDENTIALS` holds the *contents* of the exported file
//! (Snapcraft 8.14 "Authenticate": "the contents of the credentials file must
//! be placed in the SNAPCRAFT_STORE_CREDENTIALS environment variable"). Those
//! contents are base64 of a JSON document `{"t":"u1-macaroon","v":{"r":ROOT,
//! "d":DISCHARGE}}` (craft-store `auth.py` decodes the variable with base64,
//! `creds.py` defines the document). A request authenticates with the root
//! macaroon and the discharge macaroon *bound* to the root's signature
//! (craft-store `ubuntu_one_store_client.py`, pymacaroons `prepare_for_request`):
//! the bound signature is `HMAC(K, HMAC(K, root_sig) || HMAC(K, discharge_sig))`
//! with a key of 32 zero bytes (pymacaroons `HashSignaturesBinder`,
//! `hmac_concat`).

use crate::Result;
use sha2::{Digest, Sha256};

/// HMAC-SHA256 (RFC 2104) over `data` with `key`.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.map(|value| value ^ byte);
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(data)
        .finalize();
    Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

fn sextet(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

/// Decodes standard or URL-safe base64, with or without padding.
pub fn base64_decode(text: &str) -> Result<Vec<u8>> {
    let text = text.trim().trim_end_matches('=');
    let mut output = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = sextet(byte).ok_or("Store credential is not base64")?;
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
        }
    }
    if text.len() % 4 == 1 {
        return Err("Store credential is not base64".into());
    }
    Ok(output)
}

/// URL-safe base64 without padding, as pymacaroons serializes macaroons.
pub fn base64_urlsafe_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |value, (index, byte)| {
            value | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..=chunk.len() {
            output.push(char::from(
                ALPHABET[(value >> (18 - 6 * index) & 63) as usize],
            ));
        }
    }
    output
}

/// The byte range of a macaroon's 32-byte signature in its binary form.
///
/// Version 1 is a sequence of packets `LLLLkey value\n` with a four-digit hex
/// length that counts the whole packet; the signature is the `signature`
/// packet. Version 2 starts with byte 2 and ends with field type 6, length 32
/// and the signature.
fn signature_range(binary: &[u8]) -> Result<std::ops::Range<usize>> {
    const MALFORMED: &str = "Store credential macaroon is malformed";
    match binary.first() {
        Some(2) => {
            let start = binary.len().checked_sub(32).ok_or(MALFORMED)?;
            if start < 3 || binary[start - 2..start] != [6, 32] {
                return Err(MALFORMED.into());
            }
            Ok(start..binary.len())
        }
        Some(first) if first.is_ascii_hexdigit() => {
            let mut index = 0;
            let mut signature = None;
            while index < binary.len() {
                let length = binary
                    .get(index..index + 4)
                    .and_then(|prefix| std::str::from_utf8(prefix).ok())
                    .and_then(|prefix| usize::from_str_radix(prefix, 16).ok())
                    .filter(|length| *length > 4 && index + length <= binary.len())
                    .ok_or(MALFORMED)?;
                let packet = &binary[index + 4..index + length];
                if let Some(value) = packet.strip_prefix(b"signature ") {
                    if value.len() != 33 || value.last() != Some(&b'\n') {
                        return Err(MALFORMED.into());
                    }
                    let start = index + 4 + "signature ".len();
                    signature = Some(start..start + 32);
                }
                index += length;
            }
            signature.ok_or_else(|| MALFORMED.into())
        }
        _ => Err(MALFORMED.into()),
    }
}

/// The discharge macaroon, serialized as before, with its signature bound to
/// the root macaroon's signature.
pub fn bind_discharge(root: &str, discharge: &str) -> Result<String> {
    let root = base64_decode(root)?;
    let mut discharge = base64_decode(discharge)?;
    let root_signature = &root[signature_range(&root)?];
    let range = signature_range(&discharge)?;
    let key = [0u8; 32];
    let mut both = hmac_sha256(&key, root_signature).to_vec();
    both.extend_from_slice(&hmac_sha256(&key, &discharge[range.clone()]));
    discharge[range].copy_from_slice(&hmac_sha256(&key, &both));
    Ok(base64_urlsafe_encode(&discharge))
}

/// The `Authorization` header value for the Store HTTPS API, from the
/// contents of an exported credential.
pub fn authorization(exported: &str) -> Result<String> {
    let decoded =
        base64_decode(exported).map_err(|_| "Store credential is not an exported login")?;
    let document: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|_| "Store credential is not an exported login")?;
    let macaroons = match document["t"].as_str() {
        Some("u1-macaroon") => &document["v"],
        None => &document,
        Some(_) => {
            return Err(
                "only Ubuntu One exported Store credentials are supported for API reads".into(),
            )
        }
    };
    let (Some(root), Some(discharge)) = (macaroons["r"].as_str(), macaroons["d"].as_str()) else {
        return Err("Store credential has no root and discharge macaroons".into());
    };
    Ok(format!(
        "Macaroon root={root}, discharge={}",
        bind_discharge(root, discharge)?
    ))
}
