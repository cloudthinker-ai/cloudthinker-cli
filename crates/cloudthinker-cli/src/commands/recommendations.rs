use cloudthinker_client::RecommendationStatus;

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

use super::build_client;
use super::render_fetched;

pub async fn run_list(
    base_url: &str,
    workspace: Option<&str>,
    status: Option<RecommendationStatus>,
    limit: u64,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    render_fetched(
        client.list_recommendations(status, limit).await,
        json,
        |recommendations| output::print_recommendation_list(recommendations),
    )
}
