fn main() {
    if cfg!(target_os = "windows") {
        println!("cargo:rerun-if-changed=native/adlx_bridge.cpp");
        println!("cargo:rerun-if-changed=native/adlx_bridge.h");
        println!("cargo:rerun-if-changed=vendor/adlx/SDK/ADLXHelper/Windows/Cpp/ADLXHelper.cpp");
        println!("cargo:rerun-if-changed=vendor/adlx/SDK/Platform/Windows/WinAPIs.cpp");
        cc::Build::new()
            .cpp(true)
            .file("native/adlx_bridge.cpp")
            .file("vendor/adlx/SDK/ADLXHelper/Windows/Cpp/ADLXHelper.cpp")
            .file("vendor/adlx/SDK/Platform/Windows/WinAPIs.cpp")
            .include("vendor/adlx")
            .flag_if_supported("/std:c++17")
            .warnings(true)
            .compile("gpu_crash_recorder_adlx");

        embed_resource::compile("app.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
