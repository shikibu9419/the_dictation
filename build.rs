fn main() {
    build_mlx();
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
    println!("cargo:rustc-link-lib=framework=Carbon");
}

// Pinned MLX C API and Metal runtime for src/qwen.
fn build_mlx() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    if target_os != "macos" {
        panic!("Native Qwen is only supported on macOS. Current target OS: {target_os}");
    }
    if target_arch != "aarch64" {
        eprintln!(
            "Warning: MLX is optimized for Apple Silicon (aarch64). \
             Current target arch: {target_arch}. Metal GPU acceleration may not be available."
        );
    }

    let mlx_c_dir = std::path::PathBuf::from("vendor/mlx-c");
    if !mlx_c_dir.join("CMakeLists.txt").exists() {
        panic!(
            "mlx-c submodule not found. Please run:\n\
             \n\
             git submodule update --init --recursive\n\
             \n\
             to clone the mlx-c dependency."
        );
    }

    // Build mlx-c via CMake
    let dst = cmake::Config::new(&mlx_c_dir)
        .define(
            "CMAKE_PROJECT_INCLUDE_BEFORE",
            std::path::Path::new("cmake/PinMLX.cmake")
                .canonicalize()
                .unwrap(),
        )
        .define("MLX_C_BUILD_EXAMPLES", "OFF")
        .define("MLX_BUILD_TESTS", "OFF")
        .define("MLX_BUILD_EXAMPLES", "OFF")
        .define("MLX_BUILD_BENCHMARKS", "OFF")
        .define("BUILD_SHARED_LIBS", "OFF")
        .build();

    // Link paths
    let lib_dir = dst.join("lib");
    // Cargo's OUT_DIR is <profile>/build/<crate-hash>/out. MLX resolves its
    // Metal resource next to the executable after distribution.
    let profile = dst.ancestors().nth(3).expect("Cargo profile directory");
    std::fs::copy(lib_dir.join("mlx.metallib"), profile.join("mlx.metallib"))
        .expect("Copy MLX Metal runtime next to QwenNative");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    // Also check lib64 (some CMake configs use this)
    let lib64_dir = dst.join("lib64");
    if lib64_dir.exists() {
        println!("cargo:rustc-link-search=native={}", lib64_dir.display());
    }

    // Link mlx-c and mlx static libraries
    println!("cargo:rustc-link-lib=static=mlxc");
    println!("cargo:rustc-link-lib=static=mlx");

    // Link macOS system frameworks required by MLX
    println!("cargo:rustc-link-lib=framework=Metal");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=Accelerate");
    println!("cargo:rustc-link-lib=framework=MetalPerformanceShaders");

    // Link C++ standard library
    println!("cargo:rustc-link-lib=c++");

    println!("cargo:rerun-if-changed=cmake/PinMLX.cmake");

    // Rerun if mlx-c sources change
    println!("cargo:rerun-if-changed=vendor/mlx-c/CMakeLists.txt");
    println!("cargo:rerun-if-changed=vendor/mlx-c/mlx/c/");
}
