//! The screens (ui/*.slint) compiled in, the release number (CROSSCOM_RELEASE, from
//! release.toml), and for Windows the program's icon and version details.

include!("../build/release.rs");

fn main() {
    let (product, version) = release();
    slint_build::compile_with_config("ui/app.slint", slint_build::CompilerConfiguration::new().with_style("fluent".into())).expect("the launcher's screens (ui/*.slint)");

    // Decided by the target, not the build host, so a cross-build gets them too.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winres::WindowsResource::new()
            .set("ProductName", &product)
            .set("FileDescription", "Cross-Com Enhanced launcher")
            .set("FileVersion", &version)
            .set("ProductVersion", &version)
            .set_icon("icon.ico")
            .compile()
            .expect("the Windows resources");
        println!("cargo:rerun-if-changed=icon.ico");
    }
}
