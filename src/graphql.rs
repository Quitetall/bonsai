//! Read-only GraphQL projection over the deterministic Bonsai fact store.

use crate::facts::{BqlQuery, FactStore, QueryResult, SqliteFactStore};
use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use std::sync::Arc;

#[derive(SimpleObject)]
struct GraphQueryResult {
    entities: Vec<String>,
    fact_count: usize,
}

impl From<QueryResult> for GraphQueryResult {
    fn from(value: QueryResult) -> Self {
        Self {
            entities: value.entities,
            fact_count: value.supporting_facts.len(),
        }
    }
}

struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn dependencies(
        &self,
        context: &Context<'_>,
        entity: String,
        depth: usize,
        #[graphql(default = "main")] snapshot: String,
    ) -> async_graphql::Result<GraphQueryResult> {
        execute(context, "DEPENDENCIES", entity, depth, snapshot)
    }

    async fn impact(
        &self,
        context: &Context<'_>,
        entity: String,
        depth: usize,
        #[graphql(default = "main")] snapshot: String,
    ) -> async_graphql::Result<GraphQueryResult> {
        execute(context, "IMPACT", entity, depth, snapshot)
    }
}

fn execute(
    context: &Context<'_>,
    operation: &str,
    entity: String,
    depth: usize,
    snapshot: String,
) -> async_graphql::Result<GraphQueryResult> {
    let store = context.data::<Arc<SqliteFactStore>>()?;
    let snapshot = store
        .resolve_snapshot(&snapshot)
        .map_err(|error| async_graphql::Error::new(error.to_string()))?;
    let query = BqlQuery::parse(&format!("{operation} {entity} DEPTH {depth}"))
        .map_err(|error| async_graphql::Error::new(error.to_string()))?;
    store
        .query(&snapshot, &query)
        .map(Into::into)
        .map_err(|error| async_graphql::Error::new(error.to_string()))
}

pub fn execute_query(store: SqliteFactStore, query: &str) -> serde_json::Value {
    let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
        .data(Arc::new(store))
        .finish();
    let response = futures::executor::block_on(schema.execute(query));
    serde_json::to_value(response).expect("GraphQL response is serializable")
}
