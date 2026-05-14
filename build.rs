fn main() {
    println!("cargo:rerun-if-env-changed=PUTMPV_VERSION");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");

    let display_version = std::env::var("PUTMPV_VERSION")
        .ok()
        .filter(|version| !version.trim().is_empty())
        .unwrap_or_else(|| {
            let package_version = std::env::var("CARGO_PKG_VERSION")
                .unwrap_or_else(|_| "0.0.0".to_string());
            match std::env::var("GITHUB_SHA") {
                Ok(sha) if sha.len() >= 7 => {
                    format!("{package_version}-dev+{}", &sha[..7])
                }
                _ => format!("{package_version}-dev"),
            }
        });
    println!("cargo:rustc-env=PUTMPV_DISPLAY_VERSION={display_version}");

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
