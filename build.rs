fn main() {
    println!("cargo:rerun-if-changed=src/avif_bridge.c");

    let avif = pkg_config::Config::new()
        .atleast_version("1.0")
        .cargo_metadata(false)
        .probe("libavif")
        .expect("libavif >= 1.0 is required; install libavif-dev or libavif");

    let mut bridge = cc::Build::new();
    bridge.file("src/avif_bridge.c");
    for include in &avif.include_paths {
        bridge.include(include);
    }
    bridge.compile("imageguide_avif_bridge");

    // Emit libavif after the static bridge so linkers using --as-needed retain it.
    pkg_config::Config::new()
        .atleast_version("1.0")
        .probe("libavif")
        .expect("libavif disappeared between configure and link");
}
