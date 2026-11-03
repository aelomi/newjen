//! Ed25519 signatures on release zips. `newjen-sign` makes them in CI; the app checks them
//! against [`UPDATE_PUBLIC_KEY`] before it unzips anything.

use ring::signature::{ED25519, UnparsedPublicKey};

/// The key every release zip must be signed with. Its private half is the `NEWJEN_SIGNING_KEY`
/// secret in the release workflow.
pub const UPDATE_PUBLIC_KEY: [u8; 32] = [
    0x6d, 0xd2, 0x62, 0xf6, 0x6d, 0xaa, 0x8f, 0x86, 0x9d, 0x54, 0xb1, 0x5f, 0xaf, 0x3c, 0x76, 0x4d,
    0xdf, 0x5c, 0x5d, 0xe9, 0x71, 0xd6, 0x59, 0xe7, 0x2b, 0x94, 0x41, 0x5c, 0xec, 0xdf, 0x1f, 0x7d,
];

/// What [`UPDATE_PUBLIC_KEY`] holds before a real key is set.
pub const UNSET_PUBLIC_KEY: [u8; 32] = [0; 32];

/// A `.sig` file holds 64 signature bytes as 128 lowercase hex characters.
pub const SIGNATURE_HEX_LEN: usize = 128;

/// Why a download was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureError {
    /// No real public key is compiled in.
    KeyNotSet,
    /// The `.sig` file is not 128 hex characters.
    Malformed,
    /// The signature does not match the bytes and key.
    Mismatch,
}

/// Checks `signature_hex` (the `.sig` file's text) against `data` with the compiled-in key.
pub fn verify(data: &[u8], signature_hex: &str) -> Result<(), SignatureError> {
    verify_with(&UPDATE_PUBLIC_KEY, data, signature_hex)
}

/// [`verify`] with a chosen key, for tests and `newjen-sign verify`.
///
/// The all-zero key is refused outright: it decodes to a low-order curve point, and a point
/// like that must never be trusted to vouch for anything.
pub fn verify_with(
    public_key: &[u8; 32],
    data: &[u8],
    signature_hex: &str,
) -> Result<(), SignatureError> {
    if *public_key == UNSET_PUBLIC_KEY {
        return Err(SignatureError::KeyNotSet);
    }
    let signature = parse_signature(signature_hex)?;
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(data, &signature)
        .map_err(|_| SignatureError::Mismatch)
}

fn parse_signature(text: &str) -> Result<Vec<u8>, SignatureError> {
    let text = text.trim();
    if text.len() != SIGNATURE_HEX_LEN {
        return Err(SignatureError::Malformed);
    }
    decode_hex(text).ok_or(SignatureError::Malformed)
}

/// Lowercase hex.
pub fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Hex of either case back to bytes; `None` for odd length or a non-hex character.
pub fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| Some((digit(pair[0])? << 4) | digit(pair[1])?))
        .collect()
}

fn digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    fn key_pair() -> (Ed25519KeyPair, [u8; 32]) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = pair.public_key().as_ref().try_into().unwrap();
        (pair, public)
    }

    fn sign(pair: &Ed25519KeyPair, data: &[u8]) -> String {
        encode_hex(pair.sign(data).as_ref())
    }

    #[test]
    fn accepts_a_signature_made_with_the_matching_key() {
        let (pair, public) = key_pair();
        let signature = sign(&pair, b"zip bytes");
        assert_eq!(signature.len(), SIGNATURE_HEX_LEN);
        assert_eq!(verify_with(&public, b"zip bytes", &signature), Ok(()));
        assert_eq!(
            verify_with(&public, b"zip bytes", &format!("{signature}\n")),
            Ok(())
        );
    }

    #[test]
    fn rejects_changed_data_a_different_key_and_a_changed_signature() {
        let (pair, public) = key_pair();
        let (_, other) = key_pair();
        let signature = sign(&pair, b"zip bytes");
        let mismatch = Err(SignatureError::Mismatch);
        assert_eq!(verify_with(&public, b"zip byteS", &signature), mismatch);
        assert_eq!(verify_with(&other, b"zip bytes", &signature), mismatch);
        let flipped = format!(
            "{}{}",
            &signature[..127],
            if signature.ends_with('0') { '1' } else { '0' }
        );
        assert_eq!(verify_with(&public, b"zip bytes", &flipped), mismatch);
    }

    #[test]
    fn rejects_text_that_is_not_a_signature() {
        let (_, public) = key_pair();
        let malformed = Err(SignatureError::Malformed);
        assert_eq!(verify_with(&public, b"x", ""), malformed);
        assert_eq!(verify_with(&public, b"x", "abcd"), malformed);
        assert_eq!(verify_with(&public, b"x", &"g".repeat(128)), malformed);
        assert_eq!(verify_with(&public, b"x", &"a".repeat(130)), malformed);
    }

    #[test]
    fn the_unset_key_verifies_nothing() {
        assert_eq!(
            verify_with(&UNSET_PUBLIC_KEY, b"x", &"0".repeat(128)),
            Err(SignatureError::KeyNotSet)
        );
        if UPDATE_PUBLIC_KEY == UNSET_PUBLIC_KEY {
            assert_eq!(
                verify(b"x", &"0".repeat(128)),
                Err(SignatureError::KeyNotSet)
            );
        }
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 1, 15, 16, 254, 255];
        assert_eq!(encode_hex(&bytes), "00010f10feff");
        assert_eq!(decode_hex("00010F10feff").as_deref(), Some(&bytes[..]));
        assert_eq!(decode_hex("0"), None);
        assert_eq!(decode_hex("zz"), None);
    }
}
