fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=icon.rc");
        println!("cargo:rerun-if-changed=../../assets/branding/melon-engine.ico");
        embed_resource::compile_for("icon.rc", ["melon_host"], embed_resource::NONE)
            .manifest_required()
            .expect("compile MelonEngine icon resource");
    }
}
