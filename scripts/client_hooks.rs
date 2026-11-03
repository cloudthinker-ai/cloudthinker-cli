
const CYBER_VOCABULARY_PARAM: &str = "cyber_vocabulary";
const CYBER_VOCABULARY: &str = "2";

fn speaks_cyber_vocabulary(path: &str) -> bool {
    path.split_once("/api/v1/")
        .is_some_and(|(_, rest)| matches!(rest.split('/').next(), Some("cyber" | "appsec")))
}

impl ClientHooks<()> for Client {
    async fn pre<E>(
        &self,
        request: &mut reqwest::Request,
        _info: &OperationInfo,
    ) -> std::result::Result<(), Error<E>> {
        if speaks_cyber_vocabulary(request.url().path()) {
            request
                .url_mut()
                .query_pairs_mut()
                .append_pair(CYBER_VOCABULARY_PARAM, CYBER_VOCABULARY);
        }
        Ok(())
    }
}
