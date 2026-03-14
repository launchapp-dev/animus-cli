use protocol::{AgentRunEvent, OutputStreamType, RunId, Timestamp, ToolCallInfo};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::output::{parser::ParsedEvent, OutputParser};

pub(super) fn spawn_stream_forwarders(
    stdout: ChildStdout,
    stderr: ChildStderr,
    run_id: RunId,
    tool: String,
    output_tx: mpsc::Sender<AgentRunEvent>,
) {
    spawn_stdout_forwarder(stdout, run_id.clone(), tool, output_tx.clone());
    spawn_stderr_forwarder(stderr, run_id, output_tx);
}

fn spawn_stdout_forwarder(
    stdout: ChildStdout,
    run_id: RunId,
    tool: String,
    output_tx: mpsc::Sender<AgentRunEvent>,
) {
    tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut parser = OutputParser::new(&tool);

        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let evt = AgentRunEvent::OutputChunk {
                        run_id: run_id.clone(),
                        stream_type: OutputStreamType::Stdout,
                        text: line.clone(),
                    };
                    let _ = output_tx.send(evt).await;

                    for parsed in parser.parse_line(&line) {
                        let event = match parsed {
                            ParsedEvent::ToolCall {
                                tool_name,
                                parameters,
                                ..
                            } => Some(AgentRunEvent::ToolCall {
                                run_id: run_id.clone(),
                                tool_info: ToolCallInfo {
                                    tool_name,
                                    parameters,
                                    timestamp: Timestamp::now(),
                                },
                            }),
                            ParsedEvent::Artifact(artifact_info) => Some(AgentRunEvent::Artifact {
                                run_id: run_id.clone(),
                                artifact_info,
                            }),
                            ParsedEvent::Thinking(content) => Some(AgentRunEvent::Thinking {
                                run_id: run_id.clone(),
                                content,
                            }),
                            ParsedEvent::Output(_) => None,
                        };

                        if let Some(evt) = event {
                            let _ = output_tx.send(evt).await;
                        }
                    }
                }
                Ok(None) => {
                    debug!(run_id = %run_id.0.as_str(), "CLI stdout stream closed");
                    break;
                }
                Err(e) => {
                    warn!(
                        run_id = %run_id.0.as_str(),
                        error = %e,
                        "Failed to read CLI stdout stream"
                    );
                    break;
                }
            }
        }
    });
}

fn spawn_stderr_forwarder(
    stderr: ChildStderr,
    run_id: RunId,
    output_tx: mpsc::Sender<AgentRunEvent>,
) {
    tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let evt = AgentRunEvent::OutputChunk {
                        run_id: run_id.clone(),
                        stream_type: OutputStreamType::Stderr,
                        text: line,
                    };
                    let _ = output_tx.send(evt).await;
                }
                Ok(None) => {
                    debug!(run_id = %run_id.0.as_str(), "CLI stderr stream closed");
                    break;
                }
                Err(e) => {
                    warn!(
                        run_id = %run_id.0.as_str(),
                        error = %e,
                        "Failed to read CLI stderr stream"
                    );
                    break;
                }
            }
        }
    });
}
