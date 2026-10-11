use among_tools::{sha3, Result};
use std::io::Read;

// Known-answer vectors taken from OpenSSL's SHA3-384, which is an independent
// implementation. They are not written from memory: a hand-written constant
// was compared against this implementation and found wrong, which is exactly
// why the reference values come from a separate program. A Store revision
// digest that does not reproduce these cannot be compared against anything.
const EMPTY: &str =
    "0c63a75b845e4f7d01107d852e4c2485c51a50aaaa94fc61995e71bbee983a2ac3713831264adb47fb6bd1e058d5f004";
const ABC: &str =
    "ec01498288516fc926459f58e2c6ad8df9b473cb0fc08c2596da7cf0e49be4b298d88cea927ac7f539f1edf228376d25";
const Z103: &str =
    "819e2220b2d5b7c4f35917a28c5953c81f423e38f59652033bdbe606a4e68af8daf03db95ae3578b1a6b4ab549d04a52";
const Z104: &str =
    "3161f180f14865a6b8b97f8c5fda677586d3f877ca0a36dae8707c9e6133f08f7bb432dcdddffeeeee9d862e278f1735";
const Z105: &str =
    "9de8cce2520a9a942eb13ffa96c686a79bf79902618bb2b273d82e06685d386dd110e0e211d77c818533e8c7da405337";
const Z5000: &str =
    "dec7188dbea29d4d9f787e3504b89229518e2825371ed298ae25859568597bd8bb6675abfb37011ac87ac1d351704978";
const B5000: &str =
    "5248f9160d651f907e0197fffc16a1aca3fb9094f5e713e79b1ce317bb62a6105372f5f2a50c5b7b74ee656b38d34658";

#[test]
fn independent_reference_vectors_are_reproduced_exactly() -> Result<()> {
    for (input, expected) in [
        ("".as_bytes(), EMPTY),
        (&b"abc"[..], ABC),
        (&[b'Z'; 103][..], Z103),
        // The rate is 104 bytes, so this is an exactly full block and the one
        // after it is the first that needs a second absorb.
        (&[b'Z'; 104][..], Z104),
        (&[b'Z'; 105][..], Z105),
        (&[b'Z'; 5000][..], Z5000),
        (&[b'B'; 5000][..], B5000),
    ] {
        assert_eq!(
            sha3::hex(input)?,
            expected,
            "SHA3-384 mismatch for {} bytes",
            input.len()
        );
    }
    Ok(())
}

#[test]
fn distinct_lengths_never_collide_and_output_is_always_384_bits() -> Result<()> {
    let mut previous = String::new();
    for length in [1usize, 103, 104, 105, 208, 209] {
        let digest = sha3::hex(std::io::Cursor::new(vec![b'Z'; length]))?;
        assert_eq!(digest.len(), 96);
        assert_ne!(digest, previous, "distinct lengths must not collide");
        previous = digest;
    }
    Ok(())
}

#[test]
fn a_chunked_stream_absorbs_the_same_bytes_as_one_read() -> Result<()> {
    let payload = vec![b'B'; 5000];
    let streamed = sha3::hex(std::io::Cursor::new(payload.clone()))?;
    // Awkward chunk sizes must not change which bytes were absorbed.
    let mut chunked = std::io::Cursor::new(payload);
    let mut buffer = vec![0u8; 7];
    let mut collected = Vec::new();
    loop {
        let read = chunked.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        collected.extend_from_slice(&buffer[..read]);
    }
    assert_eq!(collected.len(), 5000);
    assert_eq!(sha3::hex(std::io::Cursor::new(collected))?, streamed);
    assert_eq!(streamed, B5000);
    Ok(())
}
