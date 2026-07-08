fn main() {
    println!("cargo:rerun-if-changed=src_c/pw_capture.c");
    println!("cargo:rerun-if-changed=build.rs");

    // Compile the PipeWire capture helper only when the feature is requested
    if std::env::var("CARGO_FEATURE_PIPEWIRE_CAPTURE").is_ok() {
        let pipewire = pkg_config::Config::new()
            .probe("libpipewire-0.3")
            .expect("libpipewire-0.3 not found (install libpipewire-0.3-dev / pipewire-devel)");
        let dbus = pkg_config::Config::new()
            .probe("dbus-1")
            .expect("dbus-1 not found (install libdbus-1-dev / dbus-devel)");

        let mut build = cc::Build::new();
        build.file("src_c/pw_capture.c")
            .flag("-fno-strict-aliasing")
            .flag("-fno-strict-overflow");

        for path in pipewire.include_paths.iter().chain(dbus.include_paths.iter()) {
            build.include(path);
        }

        build.compile("pw_capture");
    }
}
