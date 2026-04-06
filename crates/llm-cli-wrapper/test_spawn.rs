use tokio::process::Command;
use std::process::Stdio;

#[tokio::main]
async fn main() {
    let path = "/usr/local/bin/claude";
    let args = vec!["Say hello"];
    
    println!("Testing spawn with path: {:?}", path);
    println!("Args: {:?}", args);
    
    let mut cmd = Command::new(path);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    
    match cmd.spawn() {
        Ok(child) => {
            println!("✓ Spawn successful!");
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                child.wait_with_output()
            ).await {
                Ok(Ok(output)) => {
                    println!("✓ Command completed");
                    println!("Exit code: {:?}", output.status.code());
                    println!("Output: {} bytes", output.stdout.len());
                }
                Ok(Err(e)) => println!("✗ Wait error: {}", e),
                Err(_) => println!("✗ Timeout"),
            }
        }
        Err(e) => {
            println!("✗ Spawn error: {}", e);
            println!("Error kind: {:?}", e.kind());
        }
    }
}
