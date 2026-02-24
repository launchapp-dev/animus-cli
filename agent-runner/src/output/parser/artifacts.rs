use protocol::{ArtifactInfo, ArtifactType};

pub(super) fn extract_artifact(line: &str) -> Option<ArtifactInfo> {
    let path = line
        .split_once(':')
        .and_then(|(_, rest)| rest.split_whitespace().next())?;

    Some(ArtifactInfo {
        artifact_id: format!("artifact_{}", chrono::Utc::now().timestamp_millis()),
        artifact_type: infer_artifact_type(path),
        file_path: Some(path.to_string()),
        size_bytes: None,
        mime_type: None,
    })
}

fn infer_artifact_type(path: &str) -> ArtifactType {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext.to_lowercase().as_str() {
        "rs" | "js" | "ts" | "py" | "go" | "java" | "cpp" | "c" | "h" => ArtifactType::Code,
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" => ArtifactType::Image,
        "txt" | "md" | "pdf" | "doc" | "docx" => ArtifactType::Document,
        "json" | "yaml" | "yml" | "toml" | "xml" | "csv" => ArtifactType::Data,
        _ => ArtifactType::File,
    }
}
