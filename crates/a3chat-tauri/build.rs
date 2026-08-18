fn main() {
    // OUT_DIR is needed by `tauri::generate_context!` which writes
    // capability / window metadata into the build output. The macro
    // is invoked by `tauri::generate_context!()` in `commands.rs`,
    // which is only compiled when the `desktop` feature is enabled.
    println!("cargo:rerun-if-changed=tauri.conf.json");
}
