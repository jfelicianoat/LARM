//! Punto de entrada del shell desktop.
#![forbid(unsafe_code)]

fn main() {
    if std::env::args().skip(1).any(|arg| arg == "--ui-build-info") {
        println!(
            "{}",
            serde_json::json!({"ui_manifest_sha256": env!("LARM_UI_MANIFEST_SHA256")})
        );
        return;
    }
    larm_desktop_lib::run()
}
