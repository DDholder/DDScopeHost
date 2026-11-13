fn main() {
    tauri_build::build();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        if let Ok(out_dir) = std::env::var("OUT_DIR") {
            let resource_lib = std::path::Path::new(&out_dir).join("resource.lib");
            if resource_lib.exists() {
                println!("cargo:rustc-link-arg-tests={}", resource_lib.display());
            }
        }
    }
}
