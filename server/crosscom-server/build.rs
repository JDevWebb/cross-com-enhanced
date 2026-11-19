//! The release number from release.toml (CROSSCOM_RELEASE), which /api/info and the log give.

include!("../../build/release.rs");

fn main() {
    release();
}
