//! Exports the Orbit methods under their MSVC C++ names.
//!
//! On 32-bit Windows rustc can't export a symbol whose name starts with `?` (the linker doesn't
//! find it), so `src/orbit.rs` gives each method a plain C name and the linker exports it again
//! under the name the game imports.

/// (the name the game imports, the function in `src/orbit.rs`)
const EXPORTS: &[(&str, &str)] = &[
    ("??0OrbitClient@orbitclient@mg@@QAE@XZ", "OrbitClient_new"),
    ("??1OrbitClient@orbitclient@mg@@QAE@XZ", "OrbitClient_drop"),
    ("?GetInstallationErrorNum@OrbitClient@orbitclient@mg@@QAEIXZ", "OrbitClient_GetInstallationErrorNum"),
    (
        "?GetInstallationErrorString@OrbitClient@orbitclient@mg@@QAEPAGPBD@Z",
        "OrbitClient_GetInstallationErrorString",
    ),
    ("?StartLauncher@OrbitClient@orbitclient@mg@@QAE_NIIPBD0@Z", "OrbitClient_StartLauncher"),
    ("?Update@OrbitClient@orbitclient@mg@@QAEXXZ", "OrbitClient_Update"),
    ("?GetRequestUniqueId@OrbitClient@orbitclient@mg@@QAEIXZ", "OrbitClient_GetRequestUniqueId"),
    (
        "?GetLoginDetails@OrbitClient@orbitclient@mg@@QAEXIPAVIGetLoginDetailsListener@23@@Z",
        "OrbitClient_GetLoginDetails",
    ),
    (
        "?GetSavegameReader@OrbitClient@orbitclient@mg@@QAEXIPAVIGetSavegameReaderListener@23@II@Z",
        "OrbitClient_GetSavegameReader",
    ),
    (
        "?GetSavegameWriter@OrbitClient@orbitclient@mg@@QAEXIPAVIGetSavegameWriterListener@23@II_N@Z",
        "OrbitClient_GetSavegameWriter",
    ),
    ("?Read@SavegameReader@orbitclient@mg@@QAEXIPAVISavegameReadListener@23@IPAXI@Z", "SavegameReader_Read"),
    ("?Close@SavegameReader@orbitclient@mg@@QAEXXZ", "SavegameReader_Close"),
    ("?SetName@SavegameWriter@orbitclient@mg@@QAE_NPAG@Z", "SavegameWriter_SetName"),
    ("?Write@SavegameWriter@orbitclient@mg@@QAEXIPAVISavegameWriteListener@23@PAXI@Z", "SavegameWriter_Write"),
    ("?Close@SavegameWriter@orbitclient@mg@@QAEX_N@Z", "SavegameWriter_Close"),
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if os == "windows" && arch == "x86" {
        for (mangled, function) in EXPORTS {
            println!("cargo:rustc-cdylib-link-arg=/EXPORT:{mangled}={function}");
        }
    }
}
