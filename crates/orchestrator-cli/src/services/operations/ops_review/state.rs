use anyhow::Result;
use orchestrator_core::{
    compute_entity_review_status as compute_entity_review_status_core,
    load_reviews as load_reviews_core, parse_review_decision as parse_review_decision_core,
    parse_review_entity_type as parse_review_entity_type_core,
    parse_reviewer_role as parse_reviewer_role_core, save_reviews as save_reviews_core,
    EntityReviewStatus, ReviewDecision, ReviewEntityType, ReviewStore, ReviewerRole,
};

pub(super) type ReviewEntityTypeCli = ReviewEntityType;
pub(super) type ReviewerRoleCli = ReviewerRole;
pub(super) type ReviewDecisionCli = ReviewDecision;
pub(super) type ReviewRecordCli = orchestrator_core::ReviewRecord;
pub(super) type EntityReviewStatusCli = EntityReviewStatus;

pub(super) fn load_reviews(project_root: &str) -> Result<ReviewStore> {
    load_reviews_core(project_root)
}

pub(super) fn save_reviews(project_root: &str, store: &ReviewStore) -> Result<()> {
    save_reviews_core(project_root, store)
}

pub(super) fn parse_review_entity_type(value: &str) -> Result<ReviewEntityTypeCli> {
    parse_review_entity_type_core(value)
}

pub(super) fn parse_reviewer_role(value: &str) -> Result<ReviewerRoleCli> {
    parse_reviewer_role_core(value)
}

pub(super) fn parse_review_decision(value: &str) -> Result<ReviewDecisionCli> {
    parse_review_decision_core(value)
}

pub(super) fn compute_entity_review_status(
    store: &ReviewStore,
    entity_type: ReviewEntityTypeCli,
    entity_id: &str,
) -> EntityReviewStatusCli {
    compute_entity_review_status_core(store, entity_type, entity_id)
}
