//! `cargo xtask dev-identity [--remove]`: a local code-signing identity for `cargo xtask bundle`.
//!
//! macOS binds Camera, Microphone and Screen Recording to an app's *designated requirement*. An
//! ad-hoc signature's requirement is the build's own hash, so every rebuild is a stranger to
//! macOS: Camera and Microphone are asked again, and Screen Recording stays listed as allowed
//! while every capture start stalls. Signed with a certificate the requirement reads "this bundle
//! id, signed by this certificate", which is as true of the next build as of this one.
//!
//! The identity is a self-signed certificate ([`NAME`], code signing only, ten years) whose
//! private key lives in a keychain file of its own: `signing.keychain-db` in
//! `~/Library/Application Support/FilmCraft Dev Signing` (or `FILMCRAFT_DEV_SIGNING_DIR`), next to
//! that keychain's random password (`password`, readable by the user only). Nothing else is
//! touched: not the login keychain, not the keychain search list, no trust setting (the
//! certificate is no trust anchor; it only makes two builds recognisable as the same app).
//! Whoever can run programs as this user can sign with it, as with any development certificate.
//!
//! A locked keychain makes `codesign` (and `security show-keychain-info`) wait on a password
//! window, so the keychain is always unlocked first, with its own password, and never asked
//! anything while it may be locked.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The certificate's common name (what `codesign -dvv` shows as the authority).
pub const NAME: &str = "FilmCraft Dev Signing";
const KEYCHAIN: &str = "signing.keychain-db";
const PASSWORD: &str = "password";
const SECURITY: &str = "/usr/bin/security";
/// macOS's own (LibreSSL): its private keys are in the form `security import` reads.
const OPENSSL: &str = "/usr/bin/openssl";

/// The local signing identity: its keychain file and the certificate's SHA-1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub keychain: PathBuf,
    pub sha1: String,
}

/// Where the identity lives: `FILMCRAFT_DEV_SIGNING_DIR`, else under the user's Application Support.
pub fn dir() -> Result<PathBuf, String> {
    if let Some(d) = std::env::var_os("FILMCRAFT_DEV_SIGNING_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d));
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Application Support/FilmCraft Dev Signing"))
}

/// The identities `security find-identity` lists: `(SHA-1, name)` of each
/// `  1) 0123…CDEF "Name" (…)` line (the listing prints an identity once per section).
pub fn parse_identities(listing: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in listing.lines() {
        let Some((_, rest)) = line.trim_start().split_once(") ") else { continue };
        let Some((sha1, rest)) = rest.split_once(' ') else { continue };
        if sha1.len() != 40 || !sha1.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let Some(name) = rest.strip_prefix('"').and_then(|r| r.split_once('"')).map(|(n, _)| n) else { continue };
        if !out.iter().any(|(s, _)| s == sha1) {
            out.push((sha1.to_string(), name.to_string()));
        }
    }
    out
}

/// A path as one double-quoted word of a `security -i` line.
fn quoted(p: &Path) -> Result<String, String> {
    let s = p.to_str().ok_or_else(|| format!("{}: not a UTF-8 path", p.display()))?;
    if s.contains(['"', '\\', '\n', '\r']) {
        return Err(format!("{s}: a signing folder cannot have quotes, backslashes or line breaks in its path"));
    }
    Ok(format!("\"{s}\""))
}

/// Run `security` commands from its standard input, so a password never shows in a process list.
fn security_script(script: &str) -> Result<(), String> {
    let mut child =
        Command::new(SECURITY).arg("-i").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| format!("{SECURITY}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(script.as_bytes()).map_err(|e| format!("{SECURITY}: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("{SECURITY}: {e}"))?;
    let err = String::from_utf8_lossy(&out.stderr);
    // `security -i` exits 0 even when a line failed: a failure is what it wrote to stderr
    if out.status.success() && err.trim().is_empty() { Ok(()) } else { Err(format!("security: {}", err.trim())) }
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.stdin(Stdio::null()).output().map_err(|e| format!("{cmd:?}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("failed: {cmd:?}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn password(dir: &Path) -> Result<String, String> {
    let p = dir.join(PASSWORD);
    let pw = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let pw = pw.trim();
    if pw.len() < 16 || !pw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{}: not the keychain password `cargo xtask dev-identity` wrote", p.display()));
    }
    Ok(pw.to_string())
}

/// The identity in `dir`, when `cargo xtask dev-identity` made one there.
pub fn find(dir: &Path) -> Option<Identity> {
    let keychain = dir.join(KEYCHAIN);
    if !keychain.is_file() || !dir.join(PASSWORD).is_file() {
        return None;
    }
    // listing certificates never needs the keychain unlocked
    let listing = run(Command::new(SECURITY).args(["find-identity", "-p", "codesigning"]).arg(&keychain)).ok()?;
    let sha1 = parse_identities(&listing).into_iter().find(|(_, n)| n == NAME)?.0;
    Some(Identity { keychain, sha1 })
}

/// Unlock the identity's keychain with its own password (it locks at logout and restart).
pub fn unlock(dir: &Path, id: &Identity) -> Result<(), String> {
    let pw = password(dir)?;
    security_script(&format!("unlock-keychain -p \"{pw}\" {}\n", quoted(&id.keychain)?))
}

#[cfg(unix)]
fn private(p: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).map_err(|e| format!("{}: {e}", p.display()))
}

#[cfg(not(unix))]
fn private(_: &Path, _: u32) -> Result<(), String> {
    Ok(())
}

const CERT_CONF: &str = "[req]\ndistinguished_name = dn\nx509_extensions = ext\nprompt = no\n[dn]\nCN = FilmCraft Dev Signing\n[ext]\nbasicConstraints = critical, CA:FALSE\nkeyUsage = critical, digitalSignature\nextendedKeyUsage = critical, codeSigning\n";

/// Make the identity in `dir` (which must not hold one yet).
pub fn create(dir: &Path) -> Result<Identity, String> {
    let keychain = dir.join(KEYCHAIN);
    if keychain.exists() {
        return Err(format!("{} exists already (`cargo xtask dev-identity --remove` deletes it)", keychain.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    private(dir, 0o700)?;
    let pw = run(Command::new(OPENSSL).args(["rand", "-hex", "24"]))?.trim().to_string();
    if pw.len() != 48 {
        return Err(format!("{OPENSSL} rand gave no password"));
    }
    let pw_file = dir.join(PASSWORD);
    std::fs::write(&pw_file, format!("{pw}\n")).map_err(|e| format!("{}: {e}", pw_file.display()))?;
    private(&pw_file, 0o600)?;
    let (conf, key, cert) = (dir.join("cert.conf"), dir.join("key.pem"), dir.join("cert.pem"));
    let made = (|| -> Result<(), String> {
        std::fs::write(&conf, CERT_CONF).map_err(|e| format!("{}: {e}", conf.display()))?;
        run(Command::new(OPENSSL)
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650", "-config"])
            .arg(&conf)
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&cert))?;
        private(&key, 0o600)?;
        let kc = quoted(&keychain)?;
        // no auto-lock: the keychain stays open until logout
        security_script(&format!("create-keychain -p \"{pw}\" {kc}\nset-keychain-settings {kc}\nunlock-keychain -p \"{pw}\" {kc}\n"))?;
        run(Command::new(SECURITY).arg("import").arg(&key).arg("-k").arg(&keychain).args(["-t", "priv", "-T", "/usr/bin/codesign"]))?;
        run(Command::new(SECURITY).arg("import").arg(&cert).arg("-k").arg(&keychain).args(["-t", "cert"]))?;
        // lets codesign use the key without a "codesign wants to sign" window
        security_script(&format!("set-key-partition-list -S apple-tool:,apple:,codesign: -s -k \"{pw}\" {kc}\n"))
    })();
    for f in [&conf, &key, &cert] {
        let _ = std::fs::remove_file(f);
    }
    if let Err(e) = made {
        let _ = remove(dir);
        return Err(e);
    }
    find(dir).ok_or_else(|| format!("the new keychain {} lists no \"{NAME}\" identity", keychain.display()))
}

/// Delete the identity: the keychain file and its password.
pub fn remove(dir: &Path) -> Result<(), String> {
    let keychain = dir.join(KEYCHAIN);
    if keychain.exists() {
        // also drops it from the keychain list of this login session
        let _ = run(Command::new(SECURITY).arg("delete-keychain").arg(&keychain));
        if keychain.exists() {
            std::fs::remove_file(&keychain).map_err(|e| format!("{}: {e}", keychain.display()))?;
        }
    }
    let pw = dir.join(PASSWORD);
    if pw.exists() {
        std::fs::remove_file(&pw).map_err(|e| format!("{}: {e}", pw.display()))?;
    }
    let _ = std::fs::remove_dir(dir);
    Ok(())
}

pub fn run_identity(args: &[&str]) -> Result<(), String> {
    let mut remove_it = false;
    for a in args {
        match *a {
            "--remove" => remove_it = true,
            _ => return Err(format!("dev-identity: unknown argument `{a}` (usage: cargo xtask dev-identity [--remove])")),
        }
    }
    if !cfg!(target_os = "macos") {
        println!("dev-identity: a macOS code-signing identity; nothing to do on this host");
        return Ok(());
    }
    let dir = dir()?;
    if remove_it {
        remove(&dir)?;
        println!("dev-identity: removed {} (the next bundle is signed ad hoc; macOS asks for its permissions again)", dir.display());
        return Ok(());
    }
    let (id, made) = match find(&dir) {
        Some(id) => (id, false),
        None => (create(&dir)?, true),
    };
    println!("dev-identity: \"{NAME}\" {} in {}", id.sha1, id.keychain.display());
    if made {
        println!(
            "dev-identity: `cargo xtask bundle` now signs with it, so macOS keeps the app's Camera, Microphone and Screen Recording permissions across rebuilds."
        );
        println!("dev-identity: once, to drop the permission records of earlier ad-hoc builds: cargo xtask bundle --reset-permissions --open");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_read_from_the_listing() {
        let listing = "\nPolicy: Code Signing\n  Matching identities\n  1) D61A97A9F361BDE014B4AF6161AA2E8CFF0B6DF5 \"FilmCraft Dev Signing\" (CSSMERR_TP_NOT_TRUSTED)\n  2) 0123456789ABCDEF0123456789ABCDEF01234567 \"Apple Development: A (B) \"\n     2 identities found\n\n  Valid identities only\n  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"Apple Development: A (B) \"\n     1 valid identities found\n";
        assert_eq!(
            parse_identities(listing),
            vec![
                ("D61A97A9F361BDE014B4AF6161AA2E8CFF0B6DF5".to_string(), NAME.to_string()),
                ("0123456789ABCDEF0123456789ABCDEF01234567".to_string(), "Apple Development: A (B) ".to_string()),
            ]
        );
        for bad in [
            "",
            "     0 identities found",
            "  1) short \"x\"",
            "  1) D61A97A9F361BDE014B4AF6161AA2E8CFF0B6DFZ \"x\"",
            "  1) D61A97A9F361BDE014B4AF6161AA2E8CFF0B6DF5 no quotes",
            "1) \u{e9}",
        ] {
            assert!(parse_identities(bad).is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn paths_are_quoted_or_refused() {
        assert_eq!(quoted(Path::new("/a b/c.keychain-db")).unwrap(), "\"/a b/c.keychain-db\"");
        for bad in ["/a\"b", "/a\\b", "/a\nb"] {
            assert!(quoted(Path::new(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn nothing_is_found_in_an_empty_folder() {
        let d = std::env::temp_dir().join(format!("filmcraft-xtask-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(find(&d), None);
        // a password file alone is no identity, and a bad one is refused
        std::fs::write(d.join(PASSWORD), "hunter2\n").unwrap();
        assert_eq!(find(&d), None);
        assert!(password(&d).is_err());
        std::fs::write(d.join(PASSWORD), "0123456789abcdef0123456789abcdef0123456789abcdef\n").unwrap();
        assert_eq!(password(&d).unwrap().len(), 48);
        remove(&d).unwrap();
        assert!(!d.exists());
    }

    /// The whole round trip on a Mac: make the identity in a temporary folder, sign two different
    /// files with it and see one designated requirement, then remove it.
    #[cfg(target_os = "macos")]
    #[test]
    fn two_builds_signed_with_the_identity_share_one_requirement() {
        let d = std::env::temp_dir().join(format!("filmcraft xtask identity {}", std::process::id()));
        let _ = remove(&d);
        let _ = std::fs::remove_dir_all(&d);
        let id = create(&d).unwrap();
        assert_eq!(find(&d), Some(id.clone()));
        assert!(!d.join("key.pem").exists(), "the private key stays in the keychain only");
        assert!(create(&d).is_err(), "never made twice");
        unlock(&d, &id).unwrap();
        // two different programs stand in for two builds: this test and cargo
        let builds = [std::env::current_exe().unwrap(), PathBuf::from(env!("CARGO"))];
        let mut reqs = Vec::new();
        let mut sizes = Vec::new();
        for (i, src) in builds.iter().enumerate() {
            let f = d.join(format!("build{i}"));
            std::fs::copy(src, &f).unwrap();
            sizes.push(std::fs::metadata(&f).unwrap().len());
            run(Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", &id.sha1, "--identifier", "org.filmcraft.xtask.identity-test", "--keychain"])
                .arg(&id.keychain)
                .arg(&f))
            .unwrap();
            run(Command::new("/usr/bin/codesign").args(["--verify", "--strict"]).arg(&f)).unwrap();
            reqs.push(run(Command::new("/usr/bin/codesign").args(["-d", "-r-"]).arg(&f)).unwrap());
        }
        assert_ne!(sizes[0], sizes[1], "two different programs");
        assert!(reqs[0].contains("certificate leaf = H\""), "{}", reqs[0]);
        assert!(reqs[0].contains(&id.sha1.to_lowercase()), "{}", reqs[0]);
        assert_eq!(reqs[0], reqs[1]);
        for i in 0..builds.len() {
            std::fs::remove_file(d.join(format!("build{i}"))).unwrap();
        }
        remove(&d).unwrap();
        assert!(!d.exists());
        assert_eq!(find(&d), None);
    }
}
