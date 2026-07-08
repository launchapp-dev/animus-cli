//! Generic `--subject-id` dispatch resolution shared by `queue enqueue` and
//! `workflow run`.
//!
//! The legacy enqueue/run surface only exposed `--task-id` / `--requirement-id`
//! / `--title`, so a subject of an arbitrary BaaS dynamic kind (e.g.
//! `kind=blog`, id `BLOG-001`) could not be dispatched: `--task-id` is
//! validated as a *task* up front and any bare id was coerced to
//! `SubjectRef::task(...)`, which the owning backend rejects ("id 'BLOG-001'
//! is a 'blog' subject, not 'task'").
//!
//! [`resolve_subject_id_ref`] is the generic path. It accepts either a
//! **qualified** `kind:id` (trusts the explicit kind) or a **bare** id (probes
//! the installed subject backends through the [`SubjectKindProbe`] to discover
//! the subject's ACTUAL kind), then builds a `SubjectRef` carrying that real
//! kind so the downstream queue lease / runner dispatch resolves the subject
//! via `<kind>/get` instead of `task/get`.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::path::Path;

use orchestrator_daemon_runtime::{resolve_subject_dispatch, SubjectPluginDispatch};
use protocol::orchestrator::{SubjectRef, SUBJECT_KIND_REQUIREMENT, SUBJECT_KIND_TASK};
use serde_json::{json, Value};

use crate::{invalid_input_error, not_found_error};

/// Catch-all wildcard a dynamic-kind subject backend declares
/// (`subject_kind:*`). It is not a concrete kind, so it is excluded from the
/// bare-id probe candidate set.
const CATCH_ALL_KIND: &str = "*";

/// Build a [`SubjectRef`] for an explicit kind token plus a (possibly already
/// qualified) id. The built-in `task` / `requirement` prefixes canonicalize to
/// their namespaced kind constants via the dedicated constructors; every other
/// token is treated as a verbatim BaaS dynamic kind.
pub(crate) fn subject_ref_for_kind(kind: &str, id: impl Into<String>) -> SubjectRef {
    let id = id.into();
    if kind.eq_ignore_ascii_case("task") || kind == SUBJECT_KIND_TASK {
        SubjectRef::task(id)
    } else if kind.eq_ignore_ascii_case("requirement") || kind == SUBJECT_KIND_REQUIREMENT {
        SubjectRef::requirement(id)
    } else {
        SubjectRef::new(kind.to_string(), id)
    }
}

/// Build the dispatch [`SubjectRef`] for `kind`, choosing the native-id shape
/// that keeps [`SubjectRef::subject_key()`] — the queue dedupe / daemon
/// active-subject key — consistent across dispatch surfaces:
///
/// - **Built-in kinds** (`task` / `requirement` / `custom`) key on the bare `id`
///   field, so `subject_key()` is the id verbatim. The dedicated `--task-id` /
///   `--requirement-id` enqueue path stores (and therefore keys on) the
///   QUALIFIED `<kind>:<native>` id, so this path keeps the qualified shape too —
///   otherwise the same task would enqueue/dispatch under two different keys
///   (`task:TASK-1` vs `TASK-1`) depending on the flag used, defeating dedupe.
/// - **Dynamic kinds** key as `<kind>::<id>`, so an already-qualified native
///   double-prefixes (`transcript::transcript:TRANSCRIPT-001`). Store the BARE
///   native id so the key is the clean `<kind>::<native>` and the payload subject
///   id is the bare native the backend stores.
///
/// The built-in vs dynamic distinction is read from `subject_key()` itself
/// (rather than re-listing the built-in kind constants) so it stays in lockstep
/// with the protocol's keying rule: a ref built from the qualified id whose
/// `subject_key()` equals that qualified id keys on the bare `id` field (built-in)
/// and is kept as-is; anything else is a dynamic kind and is rebuilt bare.
fn subject_ref_preserving_key(kind: &str, qualified: &str, bare_native: &str) -> SubjectRef {
    let qualified_ref = subject_ref_for_kind(kind, qualified.to_string());
    if qualified_ref.subject_key() == qualified {
        qualified_ref
    } else {
        subject_ref_for_kind(kind, bare_native.to_string())
    }
}

/// Abstraction over the subject router used to resolve a bare id's real kind.
/// Split out as a trait so the resolution logic is unit-testable with a stub
/// router (no plugin processes spawned).
#[async_trait]
pub(crate) trait SubjectKindProbe: Send + Sync {
    /// Concrete kinds to probe for a bare id, in priority order.
    ///
    /// The `*` catch-all is deliberately EXCLUDED. It is not a concrete kind:
    /// resolving a bare id "under" it would build a `SubjectRef` with kind `*`
    /// (non-dispatchable), and a backend cannot resolve a bare id through the
    /// catch-all anyway — routing `*/get` fails (`subject '*:<id>' not found` /
    /// `subject '<kind>:<id>' is not kind '*'`), because the wildcard is not a
    /// real subject kind the backend serves. A runtime-declared kind that lives
    /// ONLY behind the catch-all (e.g. `transcript`, created via a portal
    /// `declare_kind` and absent from the plugin manifest) is therefore not
    /// enumerable from a bare id; such subjects must be dispatched with a
    /// qualified `<kind>:<native>` id (which routes `<kind>/get` through the
    /// catch-all and resolves).
    fn candidate_kinds(&self) -> Vec<String>;

    /// `true` when `<kind>/get` resolves a subject for the qualified id.
    async fn subject_exists(&self, kind: &str, qualified_id: &str) -> Result<bool>;
}

/// Resolve a `--subject-id` value to a kind-correct [`SubjectRef`].
///
/// - Qualified `kind:native` → trust the explicit kind, validate the subject
///   exists under it, and build `SubjectRef::new(kind, "kind:native")`.
/// - Bare `native` → probe each candidate kind via `<kind>/get` and build the
///   ref for the first kind that owns the id. Errors with an actionable hint
///   when nothing matches (e.g. a dynamic kind that is not enumerable from a
///   bare id — the caller must pass the qualified form).
pub(crate) async fn resolve_subject_id_ref(subject_id: &str, probe: &dyn SubjectKindProbe) -> Result<SubjectRef> {
    let trimmed = subject_id.trim();
    if trimmed.is_empty() {
        return Err(invalid_input_error("--subject-id must not be empty"));
    }

    // Qualified `<kind>:<native>`: the caller named the kind explicitly. Trust
    // it (this is the path BaaS dynamic kinds use), but validate existence so a
    // typo surfaces as a clean not-found instead of a dangling dispatch.
    if let Some((kind, native)) = trimmed.split_once(':') {
        let kind = kind.trim();
        let native = native.trim();
        if !kind.is_empty() && !native.is_empty() {
            // Existence is validated with the QUALIFIED id — the backend keys
            // subjects as `<kind>:<native>`. The stored SubjectRef shape is then
            // chosen to keep `subject_key()` consistent: dynamic kinds carry the
            // bare native id (avoiding the `<kind>::<kind>:<native>` double-prefix
            // that broke the transcript dispatch), while built-in kinds keep the
            // qualified id for dedupe parity with the --task-id / --requirement-id
            // path. See `subject_ref_preserving_key`.
            if !probe.subject_exists(kind, trimmed).await? {
                return Err(not_found_error(format!("subject '{trimmed}' not found under kind '{kind}'")));
            }
            return Ok(subject_ref_preserving_key(kind, trimmed, native));
        }
    }

    // Bare id: discover the kind by probing each registered concrete kind.
    let candidates = probe.candidate_kinds();
    if candidates.is_empty() {
        return Err(not_found_error(format!(
            "subject id '{trimmed}' has no kind qualifier and no subject backend kinds are installed to resolve it; \
             pass a qualified id like '<kind>:{trimmed}'"
        )));
    }
    for kind in &candidates {
        // Probe with the QUALIFIED id (backends key subjects `<kind>:<native>`),
        // then store the shape that keeps `subject_key()` consistent — bare native
        // for dynamic kinds (no `<kind>::<kind>:<native>` double-prefix), qualified
        // for built-ins (dedupe parity with the --task-id path). See
        // `subject_ref_preserving_key`.
        let qualified = crate::qualify_subject_id(trimmed, kind);
        if probe.subject_exists(kind, &qualified).await? {
            return Ok(subject_ref_preserving_key(kind, &qualified, trimmed));
        }
    }
    Err(not_found_error(format!(
        "subject id '{trimmed}' not found under any installed subject kind (probed: {}); \
         for BaaS dynamic kinds pass a qualified id like '<kind>:{trimmed}' (e.g. 'blog:{trimmed}'). \
         A runtime-declared kind served only by the '*' catch-all backend (e.g. 'transcript') is not \
         enumerable from a bare id and MUST be passed qualified.",
        candidates.join(", ")
    )))
}

/// Production [`SubjectKindProbe`] backed by the lazily-spawned subject router.
/// Each `subject_exists` call routes `<kind>/get` through the installed
/// subject_backend plugin(s); the catch-all (`*`) backend serves runtime
/// dynamic kinds.
pub(crate) struct RouterSubjectProbe {
    dispatch: SubjectPluginDispatch,
}

impl RouterSubjectProbe {
    /// Discover installed subject backends for `project_root` and build a probe
    /// over the resulting router. No plugin is spawned until a `<kind>/get`
    /// routes to it.
    pub(crate) async fn discover(project_root: &Path) -> Result<Self> {
        let resolution = resolve_subject_dispatch(project_root).await?;
        Ok(Self { dispatch: resolution.selected })
    }
}

/// `true` when a `<kind>/get` response carries a subject object (top-level or
/// `{ "subject": { ... } }` wrapped) with an `id`.
fn response_has_subject(value: &Value) -> bool {
    if value.is_null() {
        return false;
    }
    if let Some(inner) = value.get("subject") {
        return inner.get("id").is_some();
    }
    value.get("id").is_some()
}

#[async_trait]
impl SubjectKindProbe for RouterSubjectProbe {
    fn candidate_kinds(&self) -> Vec<String> {
        self.dispatch.kinds().iter().filter(|k| k.as_str() != CATCH_ALL_KIND).cloned().collect()
    }

    async fn subject_exists(&self, kind: &str, qualified_id: &str) -> Result<bool> {
        use animus_plugin_protocol::error_codes;
        // A backend rejecting the id (wrong kind, not found, bad params) is a
        // clean "does not exist under this kind" — map to `false` so the caller
        // tries the next candidate / emits the actionable not-found. But a
        // genuine infrastructure fault (timeout, uninitialized / cancelled
        // plugin) must NOT masquerade as not-found: surface it so a temporarily
        // unhealthy backend yields an actionable error instead of silently
        // failing to dispatch a valid subject.
        match self.dispatch.route_call(&format!("{kind}/get"), Some(json!({ "id": qualified_id }))).await {
            Ok(value) => Ok(response_has_subject(&value)),
            Err(err)
                if matches!(
                    err.code,
                    error_codes::TIMEOUT | error_codes::PLUGIN_NOT_INITIALIZED | error_codes::REQUEST_CANCELLED
                ) =>
            {
                Err(anyhow!("subject backend for kind '{kind}' is unavailable ({}): {}", err.code, err.message))
            }
            Err(_) => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Stub probe: a fixed candidate-kind list plus a map of
    /// `(kind, qualified_id)` pairs that "exist".
    struct StubProbe {
        candidates: Vec<String>,
        existing: HashMap<(String, String), bool>,
    }

    impl StubProbe {
        fn new(candidates: &[&str], existing: &[(&str, &str)]) -> Self {
            Self {
                candidates: candidates.iter().map(|s| s.to_string()).collect(),
                existing: existing.iter().map(|(k, id)| ((k.to_string(), id.to_string()), true)).collect(),
            }
        }
    }

    #[async_trait]
    impl SubjectKindProbe for StubProbe {
        fn candidate_kinds(&self) -> Vec<String> {
            self.candidates.clone()
        }
        async fn subject_exists(&self, kind: &str, qualified_id: &str) -> Result<bool> {
            Ok(self.existing.get(&(kind.to_string(), qualified_id.to_string())).copied().unwrap_or(false))
        }
    }

    #[test]
    fn subject_ref_for_kind_canonicalizes_builtins() {
        assert_eq!(subject_ref_for_kind("task", "task:TASK-1").kind(), SUBJECT_KIND_TASK);
        assert_eq!(subject_ref_for_kind("requirement", "requirement:REQ-1").kind(), SUBJECT_KIND_REQUIREMENT);
        // Dynamic kinds pass through verbatim, id preserved as-is.
        let blog = subject_ref_for_kind("blog", "blog:BLOG-001");
        assert_eq!(blog.kind(), "blog");
        assert_eq!(blog.id(), "blog:BLOG-001");
    }

    #[tokio::test]
    async fn qualified_blog_id_resolves_kind_blog() {
        let probe = StubProbe::new(&["task"], &[("blog", "blog:BLOG-001")]);
        let r = resolve_subject_id_ref("blog:BLOG-001", &probe).await.expect("qualified resolves");
        assert_eq!(r.kind(), "blog");
        // The native id is stored BARE (not the qualified input): the backend
        // keys on the bare id and `subject_key()` re-qualifies dynamic kinds.
        assert_eq!(r.id(), "BLOG-001");
    }

    #[tokio::test]
    async fn qualified_task_id_resolves_canonical_task_kind() {
        let probe = StubProbe::new(&[], &[("task", "task:TASK-1")]);
        let r = resolve_subject_id_ref("task:TASK-1", &probe).await.expect("qualified task resolves");
        assert_eq!(r.kind(), SUBJECT_KIND_TASK);
        // Built-in kinds keep the QUALIFIED id so `subject_key()` matches the
        // dedicated `--task-id` enqueue path (dedupe parity): a built-in's
        // subject_key is the bare id field, so it is not double-prefixed.
        assert_eq!(r.id(), "task:TASK-1");
        assert_eq!(r.subject_key(), "task:TASK-1");
    }

    #[tokio::test]
    async fn qualified_dynamic_id_stores_bare_native_id_without_double_prefix() {
        // Regression: enqueuing `transcript:TRANSCRIPT-001` used to build a
        // SubjectRef whose id kept the qualified prefix, so `subject_key()`
        // emitted the double-prefixed `transcript::transcript:TRANSCRIPT-001`
        // into the queue item + dispatch payload. The ref must carry the bare
        // native id + kind, yielding the clean queue key `transcript::TRANSCRIPT-001`.
        let probe = StubProbe::new(
            &["task", "requirement", "blog", "knowledge"],
            &[("transcript", "transcript:TRANSCRIPT-001")],
        );
        let r =
            resolve_subject_id_ref("transcript:TRANSCRIPT-001", &probe).await.expect("qualified transcript resolves");
        assert_eq!(r.kind(), "transcript");
        assert_eq!(r.id(), "TRANSCRIPT-001");
        assert_eq!(r.subject_key(), "transcript::TRANSCRIPT-001");
    }

    #[tokio::test]
    async fn qualified_missing_subject_errors_not_found() {
        let probe = StubProbe::new(&["task"], &[]);
        let err = resolve_subject_id_ref("blog:NOPE", &probe).await.expect_err("missing should fail");
        assert!(err.to_string().contains("not found under kind 'blog'"), "got: {err}");
    }

    #[tokio::test]
    async fn bare_blog_id_probes_to_kind_blog() {
        // Bare id, blog is among the registered concrete kinds and owns it.
        let probe = StubProbe::new(&["task", "blog"], &[("blog", "blog:BLOG-001")]);
        let r = resolve_subject_id_ref("BLOG-001", &probe).await.expect("bare blog resolves");
        assert_eq!(r.kind(), "blog");
        // Bare native id: the probe validates the qualified form but the ref
        // stores the bare id so `subject_key()` does not double-prefix.
        assert_eq!(r.id(), "BLOG-001");
        assert_eq!(r.subject_key(), "blog::BLOG-001");
    }

    #[tokio::test]
    async fn bare_task_id_probes_to_kind_task() {
        let probe = StubProbe::new(&["task", "blog"], &[("task", "task:TASK-9")]);
        let r = resolve_subject_id_ref("TASK-9", &probe).await.expect("bare task resolves");
        assert_eq!(r.kind(), SUBJECT_KIND_TASK);
        // Built-in: keep the qualified id so `subject_key()` matches the
        // dedicated `--task-id` path (dedupe parity).
        assert_eq!(r.id(), "task:TASK-9");
        assert_eq!(r.subject_key(), "task:TASK-9");
    }

    #[tokio::test]
    async fn bare_dynamic_id_only_behind_catch_all_is_unresolvable_with_hint() {
        // A runtime-declared kind served ONLY by the `*` catch-all (e.g.
        // `transcript`) is NOT in the concrete candidate set (it is absent from
        // the plugin manifest), and the catch-all cannot resolve a bare id. The
        // resolver must surface an actionable not-found telling the caller to
        // pass the qualified form, rather than mis-resolving under `task`.
        let probe = StubProbe::new(
            &["task", "requirement", "blog", "knowledge"],
            &[("transcript", "transcript:TRANSCRIPT-001")],
        );
        let err = resolve_subject_id_ref("TRANSCRIPT-001", &probe).await.expect_err("bare catch-all kind unresolvable");
        let msg = err.to_string();
        assert!(msg.contains("not found"), "got: {msg}");
        assert!(msg.contains(":TRANSCRIPT-001"), "hint suggests the qualified `<kind>:<native>` form: {msg}");
        assert!(msg.contains("catch-all"), "hint explains the catch-all limitation: {msg}");
        assert_eq!(crate::classify_cli_error_kind(&err), crate::CliErrorKind::NotFound);
    }

    #[tokio::test]
    async fn bare_id_unresolvable_errors_with_qualified_hint() {
        // Dynamic kind not enumerable from a bare id (only `task` registered).
        let probe = StubProbe::new(&["task"], &[]);
        let err = resolve_subject_id_ref("BLOG-001", &probe).await.expect_err("unresolvable should fail");
        let msg = err.to_string();
        assert!(msg.contains("not found"), "got: {msg}");
        assert!(msg.contains("blog:BLOG-001"), "hint suggests qualified form: {msg}");
        // A bare-id miss is a not-found, not an internal error: machine callers
        // must get the not_found exit class.
        assert_eq!(crate::classify_cli_error_kind(&err), crate::CliErrorKind::NotFound);
    }

    #[tokio::test]
    async fn empty_subject_id_rejected() {
        let probe = StubProbe::new(&["task"], &[]);
        let err = resolve_subject_id_ref("   ", &probe).await.expect_err("empty rejected");
        assert!(err.to_string().contains("must not be empty"), "got: {err}");
    }

    #[test]
    fn dispatch_for_resolved_blog_subject_preserves_kind() {
        use chrono::Utc;
        use protocol::{SubjectDispatch, SubjectDispatchExt};
        // Enqueuing a kind=blog subject must build a dispatch whose SubjectRef
        // carries kind=blog (not coerced to task), so the queue lease / runner
        // resolves it via `blog/get`.
        let subject_ref = subject_ref_for_kind("blog", "blog:BLOG-001");
        let dispatch =
            SubjectDispatch::for_subject_with_metadata(subject_ref, "draft-post", "manual-queue-enqueue", Utc::now());
        assert_eq!(dispatch.subject_kind(), Some("blog"));
        assert_eq!(dispatch.to_workflow_run_input().subject().unwrap().kind(), "blog");
        assert_eq!(dispatch.to_workflow_run_input().subject().unwrap().id(), "blog:BLOG-001");
    }

    #[test]
    fn response_has_subject_detects_shapes() {
        assert!(response_has_subject(&json!({ "id": "blog:BLOG-001" })));
        assert!(response_has_subject(&json!({ "subject": { "id": "blog:BLOG-001" } })));
        assert!(!response_has_subject(&json!(null)));
        assert!(!response_has_subject(&json!({ "ok": true })));
    }
}
