//! Incrusta `larm-daemon.manifest` en el ejecutable (MSVC): declara
//! compatibilidad con Windows 10/11 para que el sistema no falsee versiones
//! (P011, «version lie») y fija `asInvoker`.

fn main() {
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    println!("cargo:rerun-if-changed=larm-daemon.manifest");
    if windows && msvc {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        let manifest = std::path::Path::new(&dir).join("larm-daemon.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
