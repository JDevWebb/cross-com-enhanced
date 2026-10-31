//! The client DLLs this launcher installs: built into it (the `embed-dlls` feature), so the
//! launcher is the only file a player needs.

/// A client DLL: its file name and its contents.
pub type File = (&'static str, &'static [u8]);

/// The DLLs built in, or `None` in a build without them.
#[must_use]
pub fn files() -> Option<[File; 2]> {
    #[cfg(feature = "embed-dlls")]
    {
        Some([
            ("ubiorbitapi_r2_loader.dll", include_bytes!(concat!(env!("OUT_DIR"), "/ubiorbitapi_r2_loader.dll"))),
            ("uplay_r1_loader.dll", include_bytes!(concat!(env!("OUT_DIR"), "/uplay_r1_loader.dll"))),
        ])
    }
    #[cfg(not(feature = "embed-dlls"))]
    {
        None
    }
}
