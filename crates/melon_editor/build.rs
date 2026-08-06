fn main() {
    println!("cargo:rerun-if-changed=ui/editor.slint");
    println!("cargo:rerun-if-changed=ui/theme.slint");

    if std::env::var_os("CARGO_FEATURE_SLINT_POC").is_some() {
        slint_build::compile("ui/editor.slint").expect("failed to compile Slint editor UI");
    }
}
