//! RELEASE TOOLING: makes the update signing key pair, signs a release zip, and checks one.
//!
//! - `newjen-sign keygen` prints a new private key and its public key. Only `keygen` ever
//!   prints a private key; store it as the `NEWJEN_SIGNING_KEY` repository secret.
//! - `newjen-sign sign <file> [--allow-other-key]` reads the private key (hex) from the
//!   `NEWJEN_SIGNING_KEY` environment variable and writes `<file>.sig`, 128 lowercase hex
//!   characters. It refuses a key that is not `UPDATE_PUBLIC_KEY` in this checkout, because
//!   apps built from it would reject the signature; `--allow-other-key` is for a key rotation.
//! - `newjen-sign verify <file> <public key hex>` checks `<file>` against `<file>.sig`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use newjen_core::update::signature::{
    SIGNATURE_HEX_LEN, UPDATE_PUBLIC_KEY, decode_hex, encode_hex, verify_with,
};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

const KEY_ENV: &str = "NEWJEN_SIGNING_KEY";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["keygen"] => keygen(),
        ["sign", file] => sign(Path::new(file), false),
        ["sign", file, "--allow-other-key"] => sign(Path::new(file), true),
        ["verify", file, public] => verify(Path::new(file), public),
        _ => Err("usage: newjen-sign keygen | sign <file> [--allow-other-key] | verify <file> <public key hex>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("newjen-sign: {message}");
            ExitCode::FAILURE
        }
    }
}

fn keygen() -> Result<(), String> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| "could not generate a key")?;
    let pair =
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| "could not read the new key")?;
    let public = pair.public_key().as_ref();
    println!("Private key (keep secret; store as the {KEY_ENV} repository secret):");
    println!("{}", encode_hex(pkcs8.as_ref()));
    println!();
    println!("Public key (hex):");
    println!("{}", encode_hex(public));
    println!();
    println!(
        "Public key (Rust; paste over UPDATE_PUBLIC_KEY in crates/core/src/update/signature.rs):"
    );
    println!("pub const UPDATE_PUBLIC_KEY: [u8; 32] = [");
    for row in public.chunks(8) {
        let bytes: Vec<String> = row.iter().map(|byte| format!("0x{byte:02x}")).collect();
        println!("    {},", bytes.join(", "));
    }
    println!("];");
    Ok(())
}

fn key_pair() -> Result<Ed25519KeyPair, String> {
    let hex = std::env::var(KEY_ENV).map_err(|_| format!("{KEY_ENV} is not set"))?;
    let bytes = decode_hex(hex.trim()).ok_or_else(|| format!("{KEY_ENV} is not a valid key"))?;
    Ed25519KeyPair::from_pkcs8(&bytes).map_err(|_| format!("{KEY_ENV} is not a valid key"))
}

fn sig_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".sig");
    PathBuf::from(name)
}

fn sign(file: &Path, allow_other_key: bool) -> Result<(), String> {
    let pair = key_pair()?;
    let public: [u8; 32] = pair
        .public_key()
        .as_ref()
        .try_into()
        .map_err(|_| "unexpected public key length")?;
    if public != UPDATE_PUBLIC_KEY && !allow_other_key {
        return Err(format!(
            "{KEY_ENV} is key {}, not the UPDATE_PUBLIC_KEY compiled into this checkout, so apps would reject the signature. Fix the secret or the constant, or pass --allow-other-key for a key rotation.",
            encode_hex(&public)
        ));
    }
    let data =
        std::fs::read(file).map_err(|error| format!("cannot read {}: {error}", file.display()))?;
    let signature = encode_hex(pair.sign(&data).as_ref());
    debug_assert_eq!(signature.len(), SIGNATURE_HEX_LEN);
    verify_with(&public, &data, &signature).map_err(|_| "the new signature does not verify")?;
    let out = sig_path(file);
    std::fs::write(&out, format!("{signature}\n"))
        .map_err(|error| format!("cannot write {}: {error}", out.display()))?;
    println!(
        "signed {} with public key {}",
        file.display(),
        encode_hex(&public)
    );
    Ok(())
}

fn verify(file: &Path, public_hex: &str) -> Result<(), String> {
    let public: [u8; 32] = decode_hex(public_hex.trim())
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("the public key must be 64 hex characters")?;
    let data =
        std::fs::read(file).map_err(|error| format!("cannot read {}: {error}", file.display()))?;
    let sig = sig_path(file);
    let signature = std::fs::read_to_string(&sig)
        .map_err(|error| format!("cannot read {}: {error}", sig.display()))?;
    verify_with(&public, &data, &signature).map_err(|error| format!("{error:?}"))?;
    println!("{} is signed by {}", file.display(), encode_hex(&public));
    Ok(())
}
