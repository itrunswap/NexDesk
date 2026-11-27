use std::{env, path::Path};

fn main() {
    build_video_pixels();
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => {
            // The packaged WebRTC portal backend uses GIO even in an X11 build.
            pkg_config::probe_library("gio-2.0")
                .expect("GIO development files are required for Linux desktop capture");
        }
        Ok("windows") => build_windows_h264(),
        _ => {}
    }
}

fn build_video_pixels() {
    let source = Path::new("../../native/video");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-env-changed=LK_CUSTOM_WEBRTC");
    if webrtc_sys_build::custom_dir().is_none() {
        webrtc_sys_build::download_webrtc().expect("WebRTC SDK for pixel conversion");
    }
    let mut build = cc::Build::new();
    build
        .include(webrtc_sys_build::webrtc_dir().join("include/third_party/libyuv/include"))
        .include(source)
        .file(source.join("sdr_pixels.c"));
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let apple = Path::new("../../native/macos/video_pixels.c");
        println!("cargo:rerun-if-changed={}", apple.display());
        build.file(apple);
        println!("cargo:rustc-link-lib=framework=CoreVideo");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
    }
    build.compile("nexdesk_video_pixels");
}

fn build_windows_h264() {
    let source = Path::new("../../native/windows");
    let implementation = source.join("mf_h264.cpp");
    println!("cargo:rerun-if-changed={}", implementation.display());
    println!(
        "cargo:rerun-if-changed={}",
        source.join("mf_h264.h").display()
    );
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .define("NOMINMAX", None)
        .flag_if_supported("/EHsc")
        .file(implementation)
        .compile("nexdesk_mf_h264");
    for library in ["mfplat", "mfuuid", "ole32", "strmiids"] {
        println!("cargo:rustc-link-lib={library}");
    }
}
