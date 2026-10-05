use cloudthinker_client::RecommendationStatus;

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

use super::build_client;

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
    let recommendations = match client.list_recommendations(status, limit).await {
        Ok(recommendations) => recommendations,
        Err(err) => return exit::report(&err),
    };
    let result = if json {
        output::emit_json(&recommendations)
    } else {
        output::print_recommendation_list(&recommendations)
    };
    match result {
        Ok(()) => ExitCode::Ok,
        Err(err) => {
            output::eprintln_error(&err);
            ExitCode::JobFailed
        }
    }
}
