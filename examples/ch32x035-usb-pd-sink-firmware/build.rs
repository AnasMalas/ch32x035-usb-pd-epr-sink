fn main() {
    println!("cargo:rerun-if-changed=memory-black-box.x");
    if std::env::var_os("CARGO_FEATURE_PERSISTENT_BLACK_BOX").is_some() {
        let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
        std::fs::write(output.join("memory.x"), include_bytes!("memory-black-box.x"))
            .expect("write black-box memory.x");
        println!("cargo:rustc-link-search={}", output.display());
    }
    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
}
