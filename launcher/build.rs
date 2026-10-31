//! With `embed-dlls`, copies the client DLLs from CROSSCOM_CLIENT_DIR into the build, for
//! `client::files` to include.

fn main() {
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
