fn main() {
    println!("cargo:rerun-if-env-changed=CODEX_RICH_VERSION");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-ObjC");
    }
}
