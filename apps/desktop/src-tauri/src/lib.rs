//! Shell desktop de LARM (Tauri 2). La GUI no lleva logica de negocio: usa la API del daemon.
#![forbid(unsafe_code)]

pub mod api;
pub mod commands;
mod instance;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _instance =
        match instance::acquire(&std::env::temp_dir().join("larm-desktop-instance.lock")) {
            Ok(instance::Instance::Primary(file)) => file,
            Ok(instance::Instance::AlreadyRunning) => {
                eprintln!("LARM ya está abierto. Usa la ventana existente.");
                return;
            }
            Err(message) => {
                eprintln!("{message}");
                return;
            }
        };
    tauri::Builder::default()
        .manage(api::ApiTransport::default())
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::smoke_enabled,
            commands::smoke_report,
            commands::inspect_adapter_package,
            api::api_request
        ])
        .run(tauri::generate_context!())
        .map_err(|e| {
            eprintln!("error fatal del shell Tauri: {e}");
        })
        .ok();
}
