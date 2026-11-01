//! Server bundles: `skiff-server-<version>-<os>-<arch>.tar.gz`, built by the
//! release job and signed with minisign. The app carries the public key and
//! checks a bundle before it uses it. A bundle that fails the check is never
//! unpacked, uploaded or run.
//!
//! The signature's trusted comment names the file (`file:<name>`), so a
//! valid signature of another bundle, such as an older version, does not
//! pass for this one.

use serde::{Deserialize, Serialize};

/// The public half of the release key that signs server bundles (minisign,
/// key id 4B7D9FC893B9C075). Its secret half is the release job's
/// `SKIFF_SERVER_SIGNING_KEY`, and the job checks that the two match.
pub const PUBLIC_KEY: &str = include_str!("server-bundle.pub");

/// `manifest.json` in a bundle.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub protocol: u32,
    pub os: String,
    pub arch: String,
    pub target: String,
    pub commit: String,
    /// The build id: the commit, with `-dirty` for a tree with changes, or
    /// `dev-<hash of the binaries>` for a dev build's own bundle. One
    /// version can have two builds; the server keeps each in its own
    /// folder. Empty in a bundle from before it: its `commit` counts.
    #[serde(default)]
    pub build: String,
    /// SHA-256 of each file in the bundle, by its path in the bundle.
    pub files: std::collections::BTreeMap<String, String>,
}

/// The bundle name for a version, OS and arch, as the release job writes it.
pub fn bundle_name(version: &str, os: &str, arch: &str) -> String {
    format!("skiff-server-{version}-{os}-{arch}.tar.gz")
}

/// Checks `bytes` against its `.minisig` with this build's key.
pub fn verify(name: &str, bytes: &[u8], minisig: &str) -> Result<(), String> {
    verify_with(PUBLIC_KEY, name, bytes, minisig)
}

/// Checks `bytes`, named `name`, against `minisig` and the minisign public
/// key `key` (its base64 line, or the whole `.pub` file).
pub fn verify_with(key: &str, name: &str, bytes: &[u8], minisig: &str) -> Result<(), String> {
    let refused = |why: &str| Err(format!("{name} is not signed by Skiff ({why}). Skiff did not use it."));
    let line = key.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("untrusted comment:")).unwrap_or_default();
    let Ok(public) = minisign_verify::PublicKey::from_base64(line) else {
        return refused("the built-in key is not a minisign key");
    };
    let Ok(sig) = minisign_verify::Signature::decode(minisig) else {
        return refused("the signature file is not a minisign signature");
    };
    if public.verify(bytes, &sig, false).is_err() {
        return refused("the signature does not match");
    }
    if sig.trusted_comment() != format!("file:{name}") {
        return refused("the signature is for another file");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway key, made for this test with minisign 0.12; its secret
    // half was deleted. It signs only the message below.
    const KEY: &str = "untrusted comment: minisign public key 3E49236E79CBBE7C\nRWR8vst5biNJPtw43YgsPkt7jCLT3q7bDPRl24Lho80Og6qV9MYWraRt\n";
    const NAME: &str = "skiff-server-0.0.0-linux-x86_64.tar.gz";
    const BYTES: &[u8] = b"skiff-server test bundle\n";
    const SIG: &str = "untrusted comment: signature from minisign secret key\n\
RUR8vst5biNJPtPkMbm6s3C8IFtGIeslMRudbLZF8PH94skH8tngLjiBy9FWDKlD4Es4UHHJQFDRYIU56lIT8tj522enoIrVLAU=\n\
trusted comment: file:skiff-server-0.0.0-linux-x86_64.tar.gz\n\
mNE7KC4NXZ3+jDHRVzwnNFPVzEL8fHMG754TiG5vCdXtT98GpoPcMuF221TKeqvTRx5ahcm/NQmYrNFUwsg5CQ==\n";

    #[test]
    fn a_signed_bundle_passes_and_anything_else_is_refused() {
        assert_eq!(verify_with(KEY, NAME, BYTES, SIG), Ok(()));
        // The base64 line alone works too.
        assert_eq!(verify_with(KEY.lines().nth(1).unwrap(), NAME, BYTES, SIG), Ok(()));
        let tampered = b"skiff-server test bundlE\n";
        assert!(verify_with(KEY, NAME, tampered, SIG).unwrap_err().contains("does not match"));
        // A valid signature of another file does not pass for this one.
        let other = bundle_name("0.0.1", "linux", "x86_64");
        assert!(verify_with(KEY, &other, BYTES, SIG).unwrap_err().contains("another file"));
        let wrong_key = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        assert!(verify_with(wrong_key, NAME, BYTES, SIG).is_err());
        assert!(verify_with(KEY, NAME, BYTES, "not a signature").is_err());
        assert!(verify_with("", NAME, BYTES, SIG).is_err());
    }

    /// The committed key is the release key: a message it signed when it
    /// was made passes, and the test key's bundle does not.
    #[test]
    fn the_app_key_is_the_release_key() {
        let name = "skiff-server-0.0.0-check.tar.gz";
        let sig = "untrusted comment: signature from minisign secret key\n\
RUR1wLmTyJ99S+ltS844PgTk6/K+AoSZkOyKhCna7yj7Xdjl/xQ8Mue1X3CdJe8ZEngb2nu+6yIM8NamJ46ejyYma04kigEIHQg=\n\
trusted comment: file:skiff-server-0.0.0-check.tar.gz\n\
6HJYwzn1k9R6cXPTR4cK0CX4R4u0gx8/k0Dl8IyryUaAKg6ezIE/sfwIEEqfgSuONouHO3+b4DMXCxG6HS1mAw==\n";
        assert!(PUBLIC_KEY.contains("4B7D9FC893B9C075"));
        assert_eq!(verify(name, b"skiff-server release key check\n", sig), Ok(()));
        assert!(verify(name, b"skiff-server release key check?\n", sig).is_err());
        assert!(verify(NAME, BYTES, SIG).is_err(), "the test key is not the release key");
    }

    #[test]
    fn the_manifest_reads_as_the_release_job_writes_it() {
        let m: Manifest = serde_json::from_str(
            r#"{"name":"skiff-server","version":"1.6.6","protocol":27,"os":"linux","arch":"x86_64","target":"x86_64-unknown-linux-musl","commit":"e8368d7","files":{"skiffd":"ab","skiff-host":"cd","skiff":"ef","systemd/skiffd.service":"01","systemd/skiff-host.service":"02"}}"#,
        )
        .unwrap();
        assert_eq!(bundle_name(&m.version, &m.os, &m.arch), "skiff-server-1.6.6-linux-x86_64.tar.gz");
        assert_eq!(m.files.len(), 5);
    }
}
