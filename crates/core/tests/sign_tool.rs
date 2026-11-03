//! `newjen-sign`: key generation, signing and checking, through the real binary.

use std::path::Path;
use std::process::{Command, Output};

use newjen_core::update::signature::{
    SIGNATURE_HEX_LEN, UNSET_PUBLIC_KEY, decode_hex, verify_with,
};

fn tool(args: &[&str], key: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_newjen-sign"));
    command.args(args).env_remove("NEWJEN_SIGNING_KEY");
    if let Some(key) = key {
        command.env("NEWJEN_SIGNING_KEY", key);
    }
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn lines_after<'a>(output: &'a str, heading: &str) -> &'a str {
    let rest = output.split_once(heading).unwrap().1;
    rest.lines().nth(1).unwrap().trim()
}

#[test]
fn keygen_signs_and_verifies_and_never_echoes_the_private_key() {
    let generated = tool(&["keygen"], None);
    assert!(generated.status.success());
    let out = text(&generated.stdout);
    let private = lines_after(&out, "Private key");
    let public = lines_after(&out, "Public key (hex):");
    assert_eq!(decode_hex(public).unwrap().len(), 32);
    assert!(out.contains("pub const UPDATE_PUBLIC_KEY: [u8; 32] = ["));

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Newjen-9.9.9-arm64.zip");
    std::fs::write(&file, b"release bytes").unwrap();
    let file_arg = file.to_str().unwrap();

    let refused = tool(&["sign", file_arg], Some(private));
    assert!(
        !refused.status.success(),
        "the placeholder constant differs from this key"
    );
    assert!(!Path::new(&format!("{file_arg}.sig")).exists());
    let refusal = text(&refused.stderr);
    assert!(
        !refusal.contains(private),
        "the private key is never printed"
    );

    let signed = tool(&["sign", file_arg, "--allow-other-key"], Some(private));
    assert!(signed.status.success(), "{}", text(&signed.stderr));
    assert!(!text(&signed.stdout).contains(private));
    assert!(!text(&signed.stderr).contains(private));
    let signature = std::fs::read_to_string(format!("{file_arg}.sig")).unwrap();
    assert_eq!(signature.trim().len(), SIGNATURE_HEX_LEN);
    assert!(
        signature
            .trim()
            .chars()
            .all(|c| matches!(c, '0'..='9' | 'a'..='f'))
    );

    let key: [u8; 32] = decode_hex(public).unwrap().try_into().unwrap();
    assert_eq!(verify_with(&key, b"release bytes", &signature), Ok(()));
    assert!(verify_with(&UNSET_PUBLIC_KEY, b"release bytes", &signature).is_err());

    assert!(tool(&["verify", file_arg, public], None).status.success());
    std::fs::write(&file, b"tampered").unwrap();
    assert!(!tool(&["verify", file_arg, public], None).status.success());
}

#[test]
fn signing_needs_a_valid_key_in_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.zip");
    std::fs::write(&file, b"x").unwrap();
    let file_arg = file.to_str().unwrap();
    assert!(!tool(&["sign", file_arg], None).status.success());
    let bad = tool(&["sign", file_arg], Some("not-a-key"));
    assert!(!bad.status.success());
    assert!(!text(&bad.stderr).contains("not-a-key"));
}
