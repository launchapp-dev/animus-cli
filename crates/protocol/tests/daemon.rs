use protocol::*;

#[test]
fn test_daemon_event_agent_status_changed() {
    let evt = DaemonEvent::AgentStatusChanged {
        agent_id: AgentId("agent-001".into()),
        status: AgentStatus::Running,
        elapsed_ms: Some(12345),
    };

    let json = serde_json::to_string(&evt).unwrap();
    assert!(json.contains("agent_status_changed"));
    assert!(json.contains("running"));
}

#[test]
fn test_daemon_event_agent_output_chunk() {
    let evt = DaemonEvent::AgentOutputChunk {
        agent_id: AgentId("agent-002".into()),
        stream_type: OutputStreamType::Stderr,
        text: "Error message".into(),
    };

    let json = serde_json::to_string(&evt).unwrap();
    assert!(json.contains("agent_output_chunk"));
    assert!(json.contains("stderr"));
}

#[test]
fn test_project_stats() {
    let stats = ProjectStats {
        total_requirements: 25,
        total_tasks: 50,
        completed_tasks: 15,
        active_agents: 3,
        total_cost: 12.34,
        recent_activity: vec![ActivityEntry {
            id: "act-001".into(),
            timestamp: Timestamp::now(),
            activity_type: ActivityType::AgentStarted,
            title: "Developer agent started".into(),
            description: Some("Working on TASK-123".into()),
            metadata: None,
        }],
    };

    let json = serde_json::to_string(&stats).unwrap();
    let parsed: ProjectStats = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.active_agents, 3);
    assert_eq!(parsed.recent_activity.len(), 1);
}

#[test]
fn test_activity_entry() {
    let activity = ActivityEntry {
        id: "act-456".into(),
        timestamp: Timestamp::now(),
        activity_type: ActivityType::TaskCompleted,
        title: "Task completed successfully".into(),
        description: Some("TASK-789 finished".into()),
        metadata: Some(serde_json::json!({
            "task_id": "TASK-789",
            "duration_ms": 45000
        })),
    };

    let json = serde_json::to_string(&activity).unwrap();
    let parsed: ActivityEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.activity_type, ActivityType::TaskCompleted);
    assert!(parsed.metadata.is_some());
}

#[test]
fn test_output_stream_type() {
    let stdout = OutputStreamType::Stdout;
    let stderr = OutputStreamType::Stderr;
    let system = OutputStreamType::System;

    assert_eq!(serde_json::to_string(&stdout).unwrap(), "\"stdout\"");
    assert_eq!(serde_json::to_string(&stderr).unwrap(), "\"stderr\"");
    assert_eq!(serde_json::to_string(&system).unwrap(), "\"system\"");
}
