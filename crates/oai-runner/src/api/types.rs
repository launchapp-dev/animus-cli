use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub type_: String,
    pub function: FunctionSchema,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Deserialize)]
pub struct StreamChunk {
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<UsageInfo>,
}

#[derive(Debug, Deserialize)]
pub struct StreamChoice {
    pub delta: StreamDelta,
    #[serde(default)]
    #[allow(dead_code)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct StreamDelta {
    #[serde(default)]
    #[allow(dead_code)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct StreamToolCall {
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<StreamFunctionCall>,
}

#[derive(Debug, Deserialize, Default)]
pub struct StreamFunctionCall {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UsageInfo {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    #[allow(dead_code)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<JsonSchemaSpec>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonSchemaSpec {
    pub name: String,
    pub strict: bool,
    pub schema: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_request_serializes_without_response_format() {
        let request = ChatRequest {
            model: "minimax/MiniMax-M2.1".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: true,
            tools: None,
            max_tokens: Some(4096),
            response_format: None,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["model"], "minimax/MiniMax-M2.1");
        assert!(json.get("response_format").is_none());
    }

    #[test]
    fn chat_request_serializes_with_json_schema_response_format() {
        let schema = json!({
            "type": "object",
            "required": ["kind", "verdict"],
            "properties": {
                "kind": { "const": "phase_decision" },
                "verdict": { "type": "string" }
            }
        });
        let request = ChatRequest {
            model: "minimax/MiniMax-M2.1".to_string(),
            messages: vec![],
            stream: true,
            tools: None,
            max_tokens: None,
            response_format: Some(ResponseFormat {
                type_: "json_schema".to_string(),
                json_schema: Some(JsonSchemaSpec {
                    name: "phase_output".to_string(),
                    strict: true,
                    schema,
                }),
            }),
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["response_format"]["type"], "json_schema");
        assert_eq!(json["response_format"]["json_schema"]["name"], "phase_output");
        assert_eq!(json["response_format"]["json_schema"]["strict"], true);
        assert_eq!(
            json["response_format"]["json_schema"]["schema"]["required"],
            json!(["kind", "verdict"])
        );
    }

    #[test]
    fn chat_message_skips_none_fields() {
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: None,
            tool_call_id: None,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert!(json.get("content").is_none());
        assert!(json.get("tool_calls").is_none());
        assert!(json.get("tool_call_id").is_none());
    }

    #[test]
    fn stream_chunk_deserializes_tool_call_delta() {
        let raw = r#"{
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "function": { "name": "read_file", "arguments": "{\"path\":" }
                    }]
                }
            }]
        }"#;
        let chunk: StreamChunk = serde_json::from_str(raw).unwrap();
        assert_eq!(chunk.choices.len(), 1);
        let tc = chunk.choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(tc[0].index, 0);
        assert_eq!(tc[0].id.as_deref(), Some("call_abc"));
        assert_eq!(tc[0].function.as_ref().unwrap().name.as_deref(), Some("read_file"));
    }

    #[test]
    fn stream_chunk_deserializes_content_delta() {
        let raw = r#"{
            "choices": [{
                "delta": { "content": "Hello world" },
                "finish_reason": null
            }]
        }"#;
        let chunk: StreamChunk = serde_json::from_str(raw).unwrap();
        assert_eq!(
            chunk.choices[0].delta.content.as_deref(),
            Some("Hello world")
        );
    }

    #[test]
    fn tool_definition_serializes_to_openai_shape() {
        let tool = ToolDefinition {
            type_: "function".to_string(),
            function: FunctionSchema {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                parameters: json!({"type": "object", "properties": {}}),
            },
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "read_file");
    }
}
