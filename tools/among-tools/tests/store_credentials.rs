use among_tools::{
    store_credentials::{
        authorization, base64_decode, base64_urlsafe_encode, bind_discharge, hmac_sha256,
    },
    Result,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// RFC 4231 test cases 1 and 2. The binding is HMAC-SHA256 throughout, so an
/// error here would make every bound discharge wrong.
#[test]
fn hmac_matches_the_rfc_4231_vectors() {
    assert_eq!(
        hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    assert_eq!(
        hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

/// RFC 4648 vectors, plus the URL-safe alphabet macaroons use and the
/// unpadded form both directions must accept.
#[test]
fn base64_matches_the_rfc_4648_vectors_in_both_alphabets() -> Result<()> {
    for (plain, encoded) in [
        ("", ""),
        ("f", "Zg"),
        ("fo", "Zm8"),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg"),
        ("fooba", "Zm9vYmE"),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64_urlsafe_encode(plain.as_bytes()), encoded);
        assert_eq!(base64_decode(encoded)?, plain.as_bytes());
    }
    assert_eq!(base64_decode("Zm8=")?, b"fo");
    assert_eq!(base64_decode("-_8")?, [0xfb, 0xff]);
    assert_eq!(base64_decode("+/8=")?, [0xfb, 0xff]);
    assert!(base64_decode("Zm9v!").is_err());
    assert!(base64_decode("Z").is_err());
    Ok(())
}

/// A version 1 macaroon: packets `LLLLkey value\n`, the hex length counting
/// the whole packet, ending with the raw 32-byte signature.
fn v1(identifier: &str, caveats: &[&str], signature: [u8; 32]) -> Vec<u8> {
    let packet = |key: &[u8], value: &[u8]| {
        let mut body = key.to_vec();
        body.push(b' ');
        body.extend_from_slice(value);
        body.push(b'\n');
        let mut packet = format!("{:04x}", body.len() + 4).into_bytes();
        packet.extend(body);
        packet
    };
    let mut bytes = packet(b"location", b"login.ubuntu.com");
    bytes.extend(packet(b"identifier", identifier.as_bytes()));
    for caveat in caveats {
        bytes.extend(packet(b"cid", caveat.as_bytes()));
    }
    bytes.extend(packet(b"signature", &signature));
    bytes
}

fn bound(root_signature: [u8; 32], discharge_signature: [u8; 32]) -> [u8; 32] {
    let key = [0u8; 32];
    let mut both = hmac_sha256(&key, &root_signature).to_vec();
    both.extend_from_slice(&hmac_sha256(&key, &discharge_signature));
    hmac_sha256(&key, &both)
}

/// Binding replaces only the discharge signature, with the value pymacaroons'
/// `HashSignaturesBinder` computes; every other byte is serialized unchanged.
#[test]
fn a_version_1_discharge_is_bound_to_the_root_signature_and_otherwise_unchanged() -> Result<()> {
    let root = v1("root-id", &["account = example"], [7; 32]);
    let discharge = v1("discharge-id", &["time-before 2027-01-01"], [9; 32]);
    let encoded = bind_discharge(
        &base64_urlsafe_encode(&root),
        &base64_urlsafe_encode(&discharge),
    )?;
    let expected = v1(
        "discharge-id",
        &["time-before 2027-01-01"],
        bound([7; 32], [9; 32]),
    );
    assert_eq!(base64_decode(&encoded)?, expected);
    assert_ne!(expected, discharge);
    Ok(())
}

/// A version 2 discharge ends with field 6, length 32 and the signature; only
/// those 32 bytes change.
#[test]
fn a_version_2_discharge_has_only_its_trailing_signature_replaced() -> Result<()> {
    let root = v1("root-id", &[], [3; 32]);
    let mut discharge = vec![2, 2, 3, b'd', b'i', b'd', 0, 0, 6, 32];
    discharge.extend([5; 32]);
    let encoded = bind_discharge(
        &base64_urlsafe_encode(&root),
        &base64_urlsafe_encode(&discharge),
    )?;
    let mut expected = discharge[..10].to_vec();
    expected.extend(bound([3; 32], [5; 32]));
    assert_eq!(base64_decode(&encoded)?, expected);
    Ok(())
}

#[test]
fn malformed_macaroons_are_refused() {
    let good = base64_urlsafe_encode(&v1("root-id", &[], [1; 32]));
    let truncated = v1("discharge-id", &[], [2; 32]);
    for discharge in [
        base64_urlsafe_encode(&truncated[..truncated.len() - 5]),
        base64_urlsafe_encode(b"0005x"),
        base64_urlsafe_encode(&[2, 6, 31]),
        base64_urlsafe_encode(&[0xff; 40]),
    ] {
        assert!(bind_discharge(&good, &discharge).is_err(), "{discharge}");
    }
}

/// The exported credential is base64 of `{"t":"u1-macaroon","v":{"r","d"}}`;
/// the header carries the root as exported and the bound discharge. Older
/// exports without the type field carry the same two macaroons.
#[test]
fn the_authorization_header_is_built_from_the_exported_login() -> Result<()> {
    let root = base64_urlsafe_encode(&v1("root-id", &[], [7; 32]));
    let discharge = base64_urlsafe_encode(&v1("discharge-id", &[], [9; 32]));
    let header = format!(
        "Macaroon root={root}, discharge={}",
        base64_urlsafe_encode(&v1("discharge-id", &[], bound([7; 32], [9; 32])))
    );
    for document in [
        serde_json::json!({"t":"u1-macaroon","v":{"r":root,"d":discharge}}),
        serde_json::json!({"r":root,"d":discharge}),
    ] {
        let exported = base64_urlsafe_encode(document.to_string().as_bytes());
        assert_eq!(authorization(&exported)?, header);
    }
    for document in [
        serde_json::json!({"t":"macaroon","v":"candid-token"}),
        serde_json::json!({"t":"u1-macaroon","v":{"r":root}}),
    ] {
        let exported = base64_urlsafe_encode(document.to_string().as_bytes());
        assert!(authorization(&exported).is_err(), "{document}");
    }
    assert!(authorization("/home/user/exported-credentials").is_err());
    Ok(())
}
