fn main() {
    println!("cargo:rerun-if-changed=native/OverlayMac.m");
    cc::Build::new()
        .file("native/OverlayMac.m")
        .flag("-fobjc-arc")
        .flag("-mmacosx-version-min=26.0")
        .compile("overlay_mac");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=ApplicationServices");
    println!("cargo:rustc-link-lib=framework=QuartzCore");
    println!("cargo:rustc-link-lib=framework=CoreImage");
}
