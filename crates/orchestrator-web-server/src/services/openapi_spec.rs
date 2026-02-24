use serde_json::Value;

pub(crate) fn build_openapi_spec() -> Value {
    let mut spec = serde_json::from_str::<Value>(include_str!("../../openapi.json"))
        .expect("openapi.json must be valid JSON");

    if let Some(info) = spec.get_mut("info").and_then(Value::as_object_mut) {
        info.insert(
            "version".to_string(),
            Value::String(env!("CARGO_PKG_VERSION").to_string()),
        );
    }

    spec
}
