fn main() {
    println!("cargo:rerun-if-changed=src/avif_bridge.c");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let avif = vcpkg::Config::new()
            .cargo_metadata(false)
            .find_package("libavif")
            .expect("libavif is required; install libavif[aom] with vcpkg");
        compile_bridge(&avif.include_paths);
        for line in avif.cargo_metadata {
            println!("{line}");
        }
        return;
    }

    let avif = pkg_config::Config::new()
        .atleast_version("1.0")
        .statik(std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos"))
        .cargo_metadata(false)
        .probe("libavif")
        .expect("libavif >= 1.0 is required; install libavif-dev or libavif");
    compile_bridge(&avif.include_paths);

    // Emit libavif after the static bridge so linkers using --as-needed retain it.
    pkg_config::Config::new()
        .atleast_version("1.0")
        .statik(std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos"))
        .probe("libavif")
        .expect("libavif disappeared between configure and link");
}

fn compile_bridge(include_paths: &[std::path::PathBuf]) {
    let mut bridge = cc::Build::new();
    bridge.file("src/avif_bridge.c");
    for include in include_paths {
        bridge.include(include);
    }
    bridge.compile("imageguide_avif_bridge");
}
