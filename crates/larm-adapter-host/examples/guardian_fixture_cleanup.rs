//! Explicit cleanup for an aborted native test's retained public descriptor.
//! Never changes Core jobs/resources or claims a failed test passed.
#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use larm_adapter_host::guardian::{GuardianDescriptor, NativeGuardian};
    use std::{path::PathBuf, time::Duration};
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("descriptor path required")?,
    );
    if !path.is_absolute() || std::fs::metadata(&path)?.len() > 512 * 1024 {
        return Err("invalid fixture descriptor".into());
    }
    let descriptor: GuardianDescriptor = serde_json::from_slice(&std::fs::read(&path)?)?;
    let owner = NativeGuardian::attach(descriptor).await?;
    owner.stop().await?;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if owner.require_closed().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    owner.release().await?;
    println!(
        "{}",
        serde_json::json!({"status":"PASS","live_signed_native_closure":true,"witness_retired":true,"core_resources_released":false,"failed_test_accepted":false})
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Native Windows fixture cleanup only");
    std::process::exit(1);
}
