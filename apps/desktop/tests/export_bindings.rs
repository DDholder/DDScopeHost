#[test]
fn export_specta_bindings() {
    let out_path = ddscope_desktop_lib::default_bindings_output_path();
    ddscope_desktop_lib::export_ts_bindings(&out_path)
        .expect("failed to export specta typescript bindings");
    let generated =
        std::fs::read_to_string(&out_path).expect("generated bindings.ts should be readable");
    assert!(!generated.trim().is_empty());
    assert!(generated.contains("getEngineInfo"));
}
