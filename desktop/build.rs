fn main() {
    // Windows: common controls v6 (the tray menu needs it) and per-monitor DPI awareness.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest::embed_manifest(embed_manifest::new_manifest("DroidLeap"))
            .expect("unable to embed the Windows manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
