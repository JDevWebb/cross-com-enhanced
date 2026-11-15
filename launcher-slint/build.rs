//! The screens (ui/*.slint) compiled in, and for Windows the program's icon and version details.

fn main() {
    slint_build::compile_with_config("ui/app.slint", slint_build::CompilerConfiguration::new().with_style("fluent".into())).expect("the launcher's screens (ui/*.slint)");

    // Decided by the target, not the build host, so a cross-build gets them too.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
        winres::WindowsResource::new()
            .set("ProductName", "Cross-Com Enhanced")
            .set("FileDescription", "Cross-Com Enhanced launcher")
            .set("FileVersion", &version)
            .set("ProductVersion", &version)
            .set_icon("icon.ico")
            .compile()
            .expect("the Windows resources");
        println!("cargo:rerun-if-changed=icon.ico");
    }
}
