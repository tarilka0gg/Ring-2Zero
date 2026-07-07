fn main() {
    println!("cargo:rerun-if-changed=src_c/pw_capture.c");
    println!("cargo:rerun-if-changed=build.rs");

    // Compile the PipeWire capture helper only when the feature is requested
    if std::env::var("CARGO_FEATURE_PIPEWIRE_CAPTURE").is_ok() {
        cc::Build::new()
            .file("src_c/pw_capture.c")
            .include("/usr/include/pipewire-0.3")
            .include("/usr/include/spa-0.2")
            .include("/usr/include/dbus-1.0")
            .include("/usr/lib64/dbus-1.0/include")
            .flag("-fno-strict-aliasing")
            .flag("-fno-strict-overflow")
            .compile("pw_capture");

        println!("cargo:rustc-link-lib=pipewire-0.3");
        println!("cargo:rustc-link-lib=dbus-1");
        println!("cargo:rustc-link-search=native=/usr/lib64");
    }
}
