use std::path::PathBuf;
use std::sync::Arc;

use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::server::serve;
use larm_adapter_sdk::source::SourceHandler;
use larm_source_p048::{P048Adapter, Profile};

#[tokio::main]
async fn main() {
    if let Err(reason) = run().await {
        eprintln!("larm-source-p048: {reason}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), &'static str> {
    let args: Vec<String> = std::env::args().collect();
    let manifest_arg = args
        .windows(2)
        .find(|pair| pair[0] == "--manifest")
        .ok_or("manifest_argument_missing")?;
    let manifest_arg = PathBuf::from(&manifest_arg[1]);
    let manifest_path = if manifest_arg.is_absolute() {
        manifest_arg
    } else {
        std::env::current_exe()
            .map_err(|_| "executable_path_unavailable")?
            .parent()
            .ok_or("executable_parent_unavailable")?
            .parent()
            .ok_or("package_root_unavailable")?
            .join(manifest_arg)
    };
    let manifest_text =
        std::fs::read_to_string(manifest_path).map_err(|_| "manifest_unavailable")?;
    let manifest = AdapterManifest::parse(&manifest_text).map_err(|_| "manifest_invalid")?;
    let work = std::env::current_dir().map_err(|_| "work_directory_unavailable")?;
    let profile_text =
        std::fs::read(work.join("profile.json")).map_err(|_| "profile_unavailable")?;
    let profile: Profile = serde_json::from_slice(&profile_text).map_err(|_| "profile_invalid")?;
    let adapter = Arc::new(
        P048Adapter::new(manifest, profile, &work).map_err(|_| "adapter_configuration_invalid")?,
    );
    let handler = SourceHandler::new(adapter).map_err(|_| "source_handler_invalid")?;
    serve(
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        Arc::new(handler),
    )
    .await
    .map_err(|_| "protocol_server_failed")?;
    Ok(())
}
