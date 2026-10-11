//! SHA3-384, used to bind a published Snap revision to exact local bytes.
//!
//! The Snap Store reports each revision's `sha3-384`, and verifying that value
//! against the file we uploaded is what makes a channel advance trustworthy.
//! The tooling lockfile pins `sha2` only, and this package is built and tested
//! offline, so the digest is implemented here rather than pulled from the
//! network at build time. It is a public standard with published test vectors,
//! not a secret construction: `tests/sha3.rs` checks the NIST known-answer
//! vectors, including multi-block and boundary-length inputs, and the store
//! comparison fails closed if the digest disagrees.

const RATE: usize = 104; // 1600 - 2*384 bits
const OUTPUT: usize = 48; // 384 bits

const ROUND_CONSTANTS: [u64; 24] = [
    0x0000_0000_0000_0001,
    0x0000_0000_0000_8082,
    0x8000_0000_0000_808a,
    0x8000_0000_8000_8000,
    0x0000_0000_0000_808b,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8009,
    0x0000_0000_0000_008a,
    0x0000_0000_0000_0088,
    0x0000_0000_8000_8009,
    0x0000_0000_8000_000a,
    0x0000_0000_8000_808b,
    0x8000_0000_0000_008b,
    0x8000_0000_0000_8089,
    0x8000_0000_0000_8003,
    0x8000_0000_0000_8002,
    0x8000_0000_0000_0080,
    0x0000_0000_0000_800a,
    0x8000_0000_8000_000a,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8080,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8008,
];

const ROTATIONS: [u32; 24] = [
    1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14, 27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44,
];

const PI: [usize; 24] = [
    10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4, 15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1,
];

fn permute(state: &mut [u64; 25]) {
    for constant in ROUND_CONSTANTS {
        // Theta
        let mut column = [0u64; 5];
        for x in 0..5 {
            column[x] = state[x] ^ state[x + 5] ^ state[x + 10] ^ state[x + 15] ^ state[x + 20];
        }
        let mut delta = [0u64; 5];
        for x in 0..5 {
            delta[x] = column[(x + 4) % 5] ^ column[(x + 1) % 5].rotate_left(1);
        }
        for x in 0..5 {
            for y in 0..5 {
                state[x + 5 * y] ^= delta[x];
            }
        }

        // Rho and Pi
        let mut carried = state[1];
        for index in 0..24 {
            let target = PI[index];
            let saved = state[target];
            state[target] = carried.rotate_left(ROTATIONS[index]);
            carried = saved;
        }

        // Chi
        for y in 0..5 {
            let row = [
                state[5 * y],
                state[5 * y + 1],
                state[5 * y + 2],
                state[5 * y + 3],
                state[5 * y + 4],
            ];
            for x in 0..5 {
                state[5 * y + x] = row[x] ^ ((!row[(x + 1) % 5]) & row[(x + 2) % 5]);
            }
        }

        // Iota
        state[0] ^= constant;
    }
}

/// SHA3-384 of a byte stream, lowercase hex, matching the Store's `sha3-384`.
pub fn hex(mut reader: impl std::io::Read) -> crate::Result<String> {
    let mut state = [0u64; 25];
    let mut buffer = vec![0u8; RATE];
    let mut filled = 0usize;
    let mut fed = false;
    loop {
        let read = reader.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        fed = true;
        filled += read;
        if filled < RATE {
            continue;
        }
        absorb(&mut state, &buffer);
        filled = 0;
    }
    // Pad the final block: 0x06 ... 0x80, which distinguishes SHA3 from
    // original Keccak.
    buffer[filled] = 0x06;
    for byte in buffer.iter_mut().skip(filled + 1) {
        *byte = 0;
    }
    buffer[RATE - 1] |= 0x80;
    absorb(&mut state, &buffer);
    debug_assert!(
        fed || filled == 0,
        "empty input still absorbs one padded block"
    );

    let mut out = String::with_capacity(OUTPUT * 2);
    for lane in state.iter().take(OUTPUT / 8) {
        for byte in lane.to_le_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    Ok(out)
}

fn absorb(state: &mut [u64; 25], block: &[u8]) {
    for (index, lane) in block.as_chunks::<8>().0.iter().enumerate() {
        state[index] ^= u64::from_le_bytes(*lane);
    }
    permute(state);
}
