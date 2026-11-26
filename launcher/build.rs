//! With `embed-dlls`, copies the client DLLs from CROSSCOM_CLIENT_DIR into the build, for
//! `client::files` to include. Gives `update` the release key's public half, from
//! scripts/verify-release.sh (where `sign-release.sh --keygen` writes it), and where releases are
//! listed. A test build can set both: CROSSCOM_RELEASE_KEY (the key, its SubjectPublicKeyInfo
//! in base64) and CROSSCOM_UPDATE_FROM (an HTTPS address answering as GitHub's latest release).

fn main() {
    println!("cargo:rerun-if-env-changed=CROSSCOM_RELEASE_KEY");
    println!("cargo:rerun-if-env-changed=CROSSCOM_UPDATE_FROM");
    let script = std::path::Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("../scripts/verify-release.sh");
    println!("cargo:rerun-if-changed={}", script.display());
    let key = std::env::var("CROSSCOM_RELEASE_KEY").unwrap_or_else(|_| release_key(&std::fs::read_to_string(&script).unwrap_or_default()));
    println!("cargo:rustc-env=CROSSCOM_RELEASE_KEY={key}");
    let from = std::env::var("CROSSCOM_UPDATE_FROM").unwrap_or_else(|_| "https://api.github.com/repos/JDevWebb/cross-com-enhanced/releases/latest".into());
    println!("cargo:rustc-env=CROSSCOM_UPDATE_FROM={from}");

    println!("cargo:rerun-if-env-changed=CROSSCOM_SERVER_DIR");
    if std::env::var_os("CARGO_FEATURE_EMBED_SERVER").is_some() {
        let dir = std::env::var_os("CROSSCOM_SERVER_DIR").expect("embed-server needs CROSSCOM_SERVER_DIR: the folder with crosscom-server.exe");
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
        let from = std::path::Path::new(&dir).join("crosscom-server.exe");
        println!("cargo:rerun-if-changed={}", from.display());
        std::fs::copy(&from, out.join("crosscom-server.exe")).unwrap_or_else(|e| panic!("{}: {e}", from.display()));
    }

    println!("cargo:rerun-if-env-changed=CROSSCOM_CLIENT_DIR");
    if std::env::var_os("CARGO_FEATURE_EMBED_DLLS").is_none() {
        return;
    }
    let dir = std::env::var_os("CROSSCOM_CLIENT_DIR").expect("embed-dlls needs CROSSCOM_CLIENT_DIR: the folder with the client DLLs");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    for dll in ["ubiorbitapi_r2_loader.dll", "uplay_r1_loader.dll"] {
        let from = std::path::Path::new(&dir).join(dll);
        println!("cargo:rerun-if-changed={}", from.display());
        std::fs::copy(&from, out.join(dll)).unwrap_or_else(|e| panic!("{}: {e}", from.display()));
    }
}

/// The base64 between `RELEASE_KEY_PEM="-----BEGIN PUBLIC KEY-----` and its END line, or "" when
/// the key hasn't been made yet (`RELEASE_KEY_PEM=""`).
fn release_key(script: &str) -> String {
    let mut lines = script.lines().skip_while(|l| !l.starts_with("RELEASE_KEY_PEM=\"-----BEGIN PUBLIC KEY-----"));
    if lines.next().is_none() {
        return String::new();
    }
    lines.take_while(|l| !l.contains("-----END PUBLIC KEY-----")).map(str::trim).collect()
}
