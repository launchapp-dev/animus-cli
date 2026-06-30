//! CI contract test for protocol drift between in-tree and standalone
//! `animus-plugin-protocol` crates.
//!
//! ## Why this exists
//!
//! After commit `95c7d948` the in-tree `crates/animus-plugin-protocol/` is the
//! sole source of truth for the wire shapes the plugin host accepts. However
//! plugin authors using the published crate at
//! `launchapp-dev/animus-protocol` could silently send wire shapes the in-tree
//! host doesn't expect any time the in-tree crate adds a field without a
//! corresponding standalone release.
//!
//! This test compares the public surface of both crates and fails on drift.
//!
//! ## How it runs
//!
//! The test reads the path to the standalone crate's `src/lib.rs` from the
//! `ANIMUS_STANDALONE_PROTOCOL_PATH` environment variable. If the variable
//! is unset, the test is treated as a *no-op skip* (it logs a notice and
//! returns successfully) so a fresh `cargo test` on a developer machine
//! doesn't fail just because they haven't cloned the standalone repo.
//!
//! In CI the `.github/workflows/protocol-drift.yml` job clones the
//! standalone repo at the matching tag, exports
//! `ANIMUS_STANDALONE_PROTOCOL_PATH`, and runs `cargo test -p
//! orchestrator-plugin-host --test protocol_drift` — so any drift fails the
//! build.
//!
//! ## What "drift" means
//!
//! The comparison is structural: we parse both files with `syn`, extract the
//! set of public items (structs, enums, constants, modules), and diff their
//! shape descriptors. We flag:
//!
//! - Items that exist in one crate but not the other.
//! - Public fields on the same struct that differ in name or type.
//! - Enum variants that differ.
//! - Const declarations whose type or visible name differs.
//!
//! We deliberately ignore documentation, attribute ordering, and source
//! position so cosmetic changes don't trip the test.
//!
//! ## Caveat
//!
//! Structural comparison cannot catch every form of runtime drift. A field
//! whose default value (`#[serde(default)]`) changes meaning, or a `serde`
//! rename that only the standalone crate applies, would slip past this test.
//! Those classes of drift remain a candidate for a future runtime-level
//! contract test that pipes a `--manifest` invocation through `PluginHost`.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Identifier used by the GH Actions workflow to point at the standalone
/// `animus-plugin-protocol/src/lib.rs`.
const STANDALONE_ENV: &str = "ANIMUS_STANDALONE_PROTOCOL_PATH";

/// Resolve the in-tree crate's `lib.rs` deterministically. `CARGO_MANIFEST_DIR`
/// is provided by cargo at test compile time and points at the
/// orchestrator-plugin-host crate root.
fn in_tree_lib_rs() -> PathBuf {
    in_tree_sibling_crate_lib_rs("animus-plugin-protocol")
}

/// Resolve the `src/lib.rs` of a workspace-sibling crate. `animus-plugin-protocol`
/// lives next to `orchestrator-plugin-host` in `crates/`, so we walk up one
/// level from `CARGO_MANIFEST_DIR` and join the sibling crate's source path.
fn in_tree_sibling_crate_lib_rs(crate_name: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("orchestrator-plugin-host crate has a parent")
        .join(crate_name)
        .join("src")
        .join("lib.rs")
}

/// Shape descriptor for one item in the public surface of a crate.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ItemShape {
    /// `pub struct Name { fields... }` with at least one field exposed.
    Struct {
        /// Map field name → rendered type string. Stable ordering for diff.
        fields: BTreeMap<String, String>,
    },
    /// `pub enum Name { variants... }`.
    Enum {
        /// Map variant name → rendered descriptor (Unit/Tuple/Struct + types).
        variants: BTreeMap<String, String>,
    },
    /// `pub const Name: Ty = ...;`.
    Const {
        /// Rendered type. Initializer is ignored (we compare wire shape, not
        /// the value — constants are pinned by the standalone release tag).
        ty: String,
    },
    /// `pub mod Name { items... }` — recurse one level.
    Module {
        /// Nested public items keyed by item name.
        items: BTreeMap<String, ItemShape>,
    },
}

impl ItemShape {
    /// Short human-readable label used in diff messages.
    fn kind(&self) -> &'static str {
        match self {
            ItemShape::Struct { .. } => "struct",
            ItemShape::Enum { .. } => "enum",
            ItemShape::Const { .. } => "const",
            ItemShape::Module { .. } => "mod",
        }
    }
}

/// Parse a `src/lib.rs` file and extract its public surface as a map
/// keyed by item name.
fn parse_public_surface(path: &PathBuf) -> BTreeMap<String, ItemShape> {
    let source = std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read {} failed: {err}", path.display()));
    let file: syn::File =
        syn::parse_str(&source).unwrap_or_else(|err| panic!("parse {} failed: {err}", path.display()));
    extract_items(&file.items)
}

fn extract_items(items: &[syn::Item]) -> BTreeMap<String, ItemShape> {
    let mut out: BTreeMap<String, ItemShape> = BTreeMap::new();
    for item in items {
        match item {
            syn::Item::Struct(s) if is_public(&s.vis) => {
                let mut fields: BTreeMap<String, String> = BTreeMap::new();
                if let syn::Fields::Named(named) = &s.fields {
                    for field in &named.named {
                        if !is_public(&field.vis) {
                            continue;
                        }
                        let name =
                            field.ident.as_ref().map(|id| id.to_string()).unwrap_or_else(|| "<anon>".to_string());
                        let ty = type_to_string(&field.ty);
                        fields.insert(name, ty);
                    }
                }
                out.insert(s.ident.to_string(), ItemShape::Struct { fields });
            }
            syn::Item::Enum(e) if is_public(&e.vis) => {
                let mut variants: BTreeMap<String, String> = BTreeMap::new();
                for variant in &e.variants {
                    variants.insert(variant.ident.to_string(), variant_shape(&variant.fields));
                }
                out.insert(e.ident.to_string(), ItemShape::Enum { variants });
            }
            syn::Item::Const(c) if is_public(&c.vis) => {
                out.insert(c.ident.to_string(), ItemShape::Const { ty: type_to_string(&c.ty) });
            }
            syn::Item::Mod(m) if is_public(&m.vis) => {
                let Some((_, inner)) = m.content.as_ref() else {
                    // `pub mod foo;` with file-level body — out of scope here.
                    continue;
                };
                let nested = extract_items(inner);
                out.insert(m.ident.to_string(), ItemShape::Module { items: nested });
            }
            _ => {}
        }
    }
    out
}

fn is_public(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

fn type_to_string(ty: &syn::Type) -> String {
    use quote_workaround::to_string as q;
    q(ty)
}

fn variant_shape(fields: &syn::Fields) -> String {
    match fields {
        syn::Fields::Unit => "unit".to_string(),
        syn::Fields::Unnamed(unnamed) => {
            let parts: Vec<String> = unnamed.unnamed.iter().map(|f| type_to_string(&f.ty)).collect();
            format!("tuple({})", parts.join(","))
        }
        syn::Fields::Named(named) => {
            let mut parts: Vec<String> = named
                .named
                .iter()
                .filter_map(|f| f.ident.as_ref().map(|id| format!("{}:{}", id, type_to_string(&f.ty))))
                .collect();
            parts.sort();
            format!("struct({{{}}})", parts.join(","))
        }
    }
}

/// We don't depend on the `quote` crate, so render types via `syn`'s built-in
/// `ToTokens` impl through `proc-macro2`.
mod quote_workaround {
    use proc_macro2::TokenStream;
    use syn::__private::ToTokens;

    pub fn to_string<T: ToTokens>(t: &T) -> String {
        let mut tokens = TokenStream::new();
        t.to_tokens(&mut tokens);
        // Collapse runs of whitespace so `Option < String >` ↔ `Option<String>`
        // compare equal even though `proc-macro2` re-emits spaces between
        // every token.
        let raw = tokens.to_string();
        raw.split_whitespace().collect::<Vec<_>>().join("")
    }
}

/// Single drift finding. We collect all findings before failing so the report
/// covers every divergence in one pass instead of iterating one fix per run.
#[derive(Debug)]
struct DriftFinding {
    /// Dot-delimited path to the divergent item (e.g. `PluginCapabilities.projections`).
    path: String,
    /// Description of the divergence.
    detail: String,
}

fn diff_surfaces(
    in_tree: &BTreeMap<String, ItemShape>,
    standalone: &BTreeMap<String, ItemShape>,
    path_prefix: &str,
    findings: &mut Vec<DriftFinding>,
) {
    for (name, in_shape) in in_tree {
        let path = if path_prefix.is_empty() { name.clone() } else { format!("{path_prefix}.{name}") };
        let Some(std_shape) = standalone.get(name) else {
            findings.push(DriftFinding {
                path: path.clone(),
                detail: format!("only in in-tree (kind={})", in_shape.kind()),
            });
            continue;
        };
        if in_shape.kind() != std_shape.kind() {
            findings.push(DriftFinding {
                path: path.clone(),
                detail: format!("kind mismatch: in-tree={} standalone={}", in_shape.kind(), std_shape.kind()),
            });
            continue;
        }
        match (in_shape, std_shape) {
            (ItemShape::Struct { fields: a }, ItemShape::Struct { fields: b }) => {
                diff_field_maps(&path, a, b, "field", findings);
            }
            (ItemShape::Enum { variants: a }, ItemShape::Enum { variants: b }) => {
                diff_field_maps(&path, a, b, "variant", findings);
            }
            (ItemShape::Const { ty: a }, ItemShape::Const { ty: b }) => {
                if a != b {
                    findings.push(DriftFinding {
                        path: path.clone(),
                        detail: format!("const type drift: in-tree={a} standalone={b}"),
                    });
                }
            }
            (ItemShape::Module { items: a }, ItemShape::Module { items: b }) => {
                diff_surfaces(a, b, &path, findings);
            }
            _ => {}
        }
    }
    for (name, std_shape) in standalone {
        if in_tree.contains_key(name) {
            continue;
        }
        let path = if path_prefix.is_empty() { name.clone() } else { format!("{path_prefix}.{name}") };
        findings.push(DriftFinding { path, detail: format!("only in standalone (kind={})", std_shape.kind()) });
    }
}

fn diff_field_maps(
    item_path: &str,
    a: &BTreeMap<String, String>,
    b: &BTreeMap<String, String>,
    member_kind: &str,
    findings: &mut Vec<DriftFinding>,
) {
    for (name, a_ty) in a {
        match b.get(name) {
            Some(b_ty) if a_ty == b_ty => {}
            Some(b_ty) => findings.push(DriftFinding {
                path: format!("{item_path}.{name}"),
                detail: format!("{member_kind} type drift: in-tree={a_ty} standalone={b_ty}"),
            }),
            None => findings.push(DriftFinding {
                path: format!("{item_path}.{name}"),
                detail: format!("{member_kind} only in in-tree (type={a_ty})"),
            }),
        }
    }
    for (name, b_ty) in b {
        if !a.contains_key(name) {
            findings.push(DriftFinding {
                path: format!("{item_path}.{name}"),
                detail: format!("{member_kind} only in standalone (type={b_ty})"),
            });
        }
    }
}

#[test]
fn protocol_public_surface_does_not_drift_against_standalone() {
    let Ok(standalone_path) = std::env::var(STANDALONE_ENV) else {
        // Local-dev path: silently skip. CI sets this env var explicitly.
        eprintln!(
            "protocol_drift: {STANDALONE_ENV} not set — skipping. CI sets this; \
             to run locally export {STANDALONE_ENV}=path/to/animus-protocol/animus-plugin-protocol/src/lib.rs"
        );
        return;
    };
    let standalone_path = PathBuf::from(standalone_path);
    assert!(standalone_path.exists(), "standalone protocol path does not exist: {}", standalone_path.display());

    let in_tree_path = in_tree_lib_rs();
    assert!(in_tree_path.exists(), "in-tree protocol lib.rs missing: {}", in_tree_path.display());

    let in_tree = parse_public_surface(&in_tree_path);
    let standalone = parse_public_surface(&standalone_path);
    let mut findings: Vec<DriftFinding> = Vec::new();
    diff_surfaces(&in_tree, &standalone, "", &mut findings);

    // Filter out drift we *expect* between the in-tree and standalone crates.
    // Each entry MUST be paired with a tracking note so it isn't quietly carried
    // forever.
    //
    // As of TASK-120 / BU-0 (STANDALONE_TAG bumped to v0.1.27) the two
    // `PluginKind` enums and every other previously in-tree-only addition
    // (env-scrub, trigger surface, log-storage, conversation_store, the typed
    // enum mirrors, the v0.5 PluginKind variants) have been reconciled — the
    // standalone tag now contains them, so all the old in-tree-only entries are
    // dropped. The only remaining divergence is in the OTHER direction:
    // standalone v0.1.27 is AHEAD of the in-tree crate on a small set of
    // additions the kernel has not yet absorbed. They are wire-additive
    // (new optional fields + a new struct) so an in-tree host that ignores them
    // stays compatible; they are tracked here until a future in-tree protocol
    // sync pulls them in.
    let expected_in_tree_only: &[&str] = &[
        // Standalone-ahead (v0.1.27): kind-capability negotiation on the
        // handshake — `InitializeParams.init_extensions` (opaque host→plugin
        // extension map), `InitializeResult.kind_capabilities` (per-kind
        // capability declarations), and the `KindCapability` struct they
        // reference. Not yet mirrored in the in-tree crate. TODO: absorb into
        // `crates/animus-plugin-protocol` in a follow-up in-tree protocol sync,
        // then drop these entries.
        "InitializeParams.init_extensions",
        "InitializeResult.kind_capabilities",
        "KindCapability",
    ];

    let unexpected_findings: Vec<&DriftFinding> =
        findings.iter().filter(|f| !expected_in_tree_only.iter().any(|allowed| f.path == *allowed)).collect();

    if !unexpected_findings.is_empty() {
        let report: Vec<String> =
            unexpected_findings.iter().map(|f| format!("  - {} :: {}", f.path, f.detail)).collect();
        panic!(
            "animus-plugin-protocol drift detected between in-tree and standalone crates:\n{}\n\n\
             If the divergence is intentional, add the item path to expected_in_tree_only \
             with a tracking note (and cut a standalone release to remove it).",
            report.join("\n")
        );
    }
}

// `protocol_drift_subject` was removed when the in-tree
// `crates/animus-subject-protocol/` mirror was deleted (v0.5 P2 fold-in).
// The CLI and daemon now consume `animus-subject-protocol` exclusively
// from the published `launchapp-dev/animus-protocol` git tag, so there is
// no second source of truth left in-tree to drift against.
