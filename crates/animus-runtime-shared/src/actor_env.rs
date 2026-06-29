//! Back-compat-safe env channel for relaying a run's [`Actor`] from the
//! daemon scheduler to the workflow runner.
//!
//! Owner-scoped schedules (a [`WorkflowSchedule`](animus_config_protocol::WorkflowSchedule)
//! with an `owner_id`) cause the daemon to mint a system [`Actor`] and run the
//! dispatched workflow AS that user. The daemon spawns the workflow runner as a
//! detached subprocess via plain CLI args, so the minted identity is relayed on
//! the [`ANIMUS_ACTOR_JSON_ENV`] environment variable rather than a new CLI
//! flag: an env var is IGNORED by an older runner (no arg-parse error), while a
//! newer runner reads it and threads the actor into its
//! `WorkflowRunInput.actor`, so downstream provider + plugin channels scope to
//! the owner. This keeps owner-scoped schedules additive and back-compat.
//!
//! TRUST BOUNDARY: the actor encoded here MUST originate only from a trusted,
//! config-authored source (the schedule's `owner_id`, asserted at
//! config-authoring time — see `WorkflowSchedule::owner_id`). It is NEVER
//! derived from runtime or agent-generated content.

use animus_actor::Actor;

/// Environment variable carrying the JSON-encoded [`Actor`] a daemon-spawned
/// workflow runner should run as. Absent / empty means a system/global run.
pub const ANIMUS_ACTOR_JSON_ENV: &str = "ANIMUS_ACTOR_JSON";

/// Encode `actor` as the JSON payload for [`ANIMUS_ACTOR_JSON_ENV`]. Returns
/// `None` (omit the env var → global scope) when serialization fails rather
/// than aborting the dispatch.
pub fn encode_actor_env(actor: &Actor) -> Option<String> {
    serde_json::to_string(actor).ok()
}

/// Decode the [`ANIMUS_ACTOR_JSON_ENV`] payload back into an [`Actor`]. Returns
/// `None` for an unset / empty / malformed value (the run stays global rather
/// than failing).
pub fn decode_actor_env(raw: Option<&str>) -> Option<Actor> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    serde_json::from_str(raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_round_trips_through_env_payload() {
        let actor = Actor { user_id: "alice".into(), claims: vec!["admin".into()], tenant_id: Some("team-7".into()) };
        let encoded = encode_actor_env(&actor).expect("encode");
        let decoded = decode_actor_env(Some(&encoded)).expect("decode");
        assert_eq!(actor, decoded);
    }

    #[test]
    fn empty_and_malformed_payloads_decode_to_none() {
        assert!(decode_actor_env(None).is_none());
        assert!(decode_actor_env(Some("")).is_none());
        assert!(decode_actor_env(Some("   ")).is_none());
        assert!(decode_actor_env(Some("not json")).is_none());
    }
}
