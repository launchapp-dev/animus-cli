use protocol::*;

#[test]
fn test_output_stream_type() {
    let stdout = OutputStreamType::Stdout;
    let stderr = OutputStreamType::Stderr;
    let system = OutputStreamType::System;

    assert_eq!(serde_json::to_string(&stdout).unwrap(), "\"stdout\"");
    assert_eq!(serde_json::to_string(&stderr).unwrap(), "\"stderr\"");
    assert_eq!(serde_json::to_string(&system).unwrap(), "\"system\"");
}
