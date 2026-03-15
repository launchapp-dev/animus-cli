mod mutation;
mod query;
mod subscription;
pub(crate) mod types;

use async_graphql::{ErrorExtensions, Schema};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse, GraphQLSubscription};
use axum::Extension;
use mutation::MutationRoot;
use orchestrator_web_api::{WebApiError, WebApiService};
use query::QueryRoot;
use subscription::SubscriptionRoot;

pub(crate) fn gql_err(e: WebApiError) -> async_graphql::Error {
    async_graphql::Error::new(&e.message).extend_with(|_, ext| {
        ext.set("code", e.code.clone());
        ext.set("exit_code", e.exit_code);
    })
}

pub type AoSchema = Schema<QueryRoot, MutationRoot, SubscriptionRoot>;

pub fn build_schema(api: WebApiService) -> AoSchema {
    Schema::build(QueryRoot, MutationRoot, SubscriptionRoot).data(api).finish()
}

pub fn ws_subscription(schema: AoSchema) -> GraphQLSubscription<AoSchema> {
    GraphQLSubscription::new(schema)
}

pub fn schema_sdl(schema: &AoSchema) -> String {
    schema.sdl()
}

pub async fn graphql_handler(Extension(schema): Extension<AoSchema>, req: GraphQLRequest) -> GraphQLResponse {
    schema.execute(req.into_inner()).await.into()
}

pub async fn graphql_playground() -> impl axum::response::IntoResponse {
    axum::response::Html(async_graphql::http::playground_source(
        async_graphql::http::GraphQLPlaygroundConfig::new("/graphql").subscription_endpoint("/graphql/ws"),
    ))
}

pub async fn graphql_sdl_handler(Extension(schema): Extension<AoSchema>) -> impl axum::response::IntoResponse {
    schema_sdl(&schema)
}

pub fn export_sdl_to_file(schema: &AoSchema) -> std::io::Result<()> {
    let sdl = schema.sdl();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("embedded").join("schema.graphql");
    std::fs::write(&path, sdl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_schema_sdl() {
        let schema = Schema::build(QueryRoot, MutationRoot, SubscriptionRoot).finish();
        let result = export_sdl_to_file(&schema);
        assert!(result.is_ok(), "Failed to export schema SDL: {:?}", result.err());
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("embedded").join("schema.graphql");
        assert!(path.exists(), "schema.graphql should exist after export");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("type QueryRoot"), "SDL should contain QueryRoot");
        assert!(contents.contains("type MutationRoot"), "SDL should contain MutationRoot");
        assert!(contents.contains("type SubscriptionRoot"), "SDL should contain SubscriptionRoot");
    }
}
