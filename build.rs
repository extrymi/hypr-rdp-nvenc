fn main() {
    #[cfg(feature = "nvenc")]
    native_bindings();
}

#[cfg(feature = "nvenc")]
fn native_bindings() {
    use std::path::PathBuf;
    let wrapper = "src/nvenc/native/wrapper.h";
    for path in [
        wrapper,
        "src/nvenc/native/abi.c",
        "tests/native/fake_driver.c",
        "vendor/nv-codec-headers/nvEncodeAPI.h",
        "vendor/nv-codec-headers/dynlink_cuda.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let bindings = bindgen::Builder::default()
        .header(wrapper)
        .clang_arg("-Ivendor/nv-codec-headers")
        .allowlist_type("NV_.*|GUID|HYPR_.*")
        .allowlist_function("hypr_nvenc_.*")
        .blocklist_var("NV_ENC_.*GUID")
        .allowlist_var("NV_.*|NVENCAPI_.*|HYPR_.*|CUDA_SUCCESS|CU_CTX_SCHED_BLOCKING_SYNC")
        .prepend_enum_name(false)
        .derive_default(true)
        .layout_tests(true)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("generate bindings from pinned NVENC headers");
    bindings
        .write_to_file(PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("nvenc.rs"))
        .expect("write generated NVENC bindings");
    // Independent C layout/bitfield checks and a CPU-only foreign-API fixture.
    // Fixture objects have no references from production code and are not linked
    // into the release executable. No NVIDIA libraries are linked at build time.
    cc::Build::new()
        .include("vendor/nv-codec-headers")
        .include("src/nvenc/native")
        .file("src/nvenc/native/abi.c")
        .file("tests/native/fake_driver.c")
        .flag_if_supported("-std=c11")
        .warnings(true)
        .extra_warnings(true)
        .compile("hypr_nvenc_abi");
    // A separate shared object exercises dlopen/dlsym without a GPU. Production
    // never references this path and has no configurable library override.
    let path = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("libhypr_nvenc_test.so");
    let compiler = cc::Build::new().get_compiler();
    let status = compiler
        .to_command()
        .args([
            "-shared",
            "-fPIC",
            "-std=c11",
            "-Ivendor/nv-codec-headers",
            "-Isrc/nvenc/native",
            "tests/native/fake_driver.c",
            "-o",
        ])
        .arg(&path)
        .status()
        .expect("build CPU test driver shared object");
    assert!(status.success(), "build CPU test driver shared object");
    println!("cargo:rustc-env=HYPR_NVENC_TEST_SO={}", path.display());
}
