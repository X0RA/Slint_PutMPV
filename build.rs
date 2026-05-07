fn main() {
    // CARGO_CFG_TARGET_OS is the artifact target (build scripts ignore cfg(target_os) for cross-compile).
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winres::WindowsResource::new();
        res.set_icon("ui/assets/appicon.ico");
        res.compile()
            .expect("failed to compile Windows icon resource");
    }

    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());

    slint_build::compile_with_config("ui/app.slint", config).unwrap();
}
