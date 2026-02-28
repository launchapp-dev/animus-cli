mod mutation;
mod query;
pub(crate) mod types;

use async_graphql::{EmptySubscription, Schema};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::Extension;
use mutation::MutationRoot;
use orchestrator_web_api::WebApiService;
use query::QueryRoot;

pub type AoSchema = Schema<QueryRoot, MutationRoot, EmptySubscription>;

pub fn build_schema(api: WebApiService) -> AoSchema {
    Schema::build(QueryRoot, MutationRoot, EmptySubscription)
        .data(api)
        .finish()
}

pub async fn graphql_handler(
    Extension(schema): Extension<AoSchema>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    schema.execute(req.into_inner()).await.into()
}

pub async fn graphql_playground() -> impl axum::response::IntoResponse {
    axum::response::Html(async_graphql::http::playground_source(
        async_graphql::http::GraphQLPlaygroundConfig::new("/graphql"),
    ))
}
