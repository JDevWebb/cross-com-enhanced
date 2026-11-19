// Reads release.toml at the repository root for a build script (include!'d by the server's, the
// client's and the launcher's) and exports it to the crate as CROSSCOM_PRODUCT ("Cross-Com
// Enhanced") and CROSSCOM_RELEASE (e.g. "0.1.0").

/// The product name and release number from release.toml, found by going up from the crate.
#[allow(dead_code)]
fn release() -> (String, String) {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let path = manifest
        .ancestors()
        .map(|dir| dir.join("release.toml"))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("no release.toml above {}", manifest.display()));
    println!("cargo:rerun-if-changed={}", path.display());
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let field = |key: &str| {
        text.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
            .unwrap_or_else(|| panic!("release.toml has no {key}"))
    };
    let (name, version) = (field("name"), field("version"));
    println!("cargo:rustc-env=CROSSCOM_PRODUCT={name}");
    println!("cargo:rustc-env=CROSSCOM_RELEASE={version}");
    (name, version)
}
