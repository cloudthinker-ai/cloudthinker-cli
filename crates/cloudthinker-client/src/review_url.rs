//! Parse a pasted GitLab/GitHub merge-request URL into MR coordinates.
//!
//! Detection is by PATH SEGMENT, not host, so a self-hosted GitLab instance
//! still resolves correctly: GitLab URLs carry a literal `-` segment before
//! `merge_requests/<n>`; GitHub URLs carry `pull/<n>`. Anything else — an
//! unparseable URL, a recognized-but-unsupported provider shape (Bitbucket,
//! Azure DevOps, AWS CodeCommit), or a non-numeric/missing MR number — is a
//! usage error (V1 scope, `product-cli-mr-f-review.md`).

use crate::error::{CtError, CtResult};

/// Which provider a parsed MR URL belongs to. V1 only ever produces these two
/// variants — other providers' URL shapes are unrecognized and rejected as
/// `CtError::Usage` before a `MrCoordinates` is ever built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MrProvider {
    Gitlab,
    Github,
}

/// Coordinates resolved from a pasted MR/PR URL: enough to look up the
/// tracked review server-side via `CtClient::lookup_review`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MrCoordinates {
    pub provider: MrProvider,
    pub project_path: String,
    pub mr_iid: i64,
}

/// Parse a GitLab or GitHub merge-request URL into its coordinates.
///
/// - GitLab: `.../<project_path>/-/merge_requests/<n>` (subgroups supported —
///   `project_path` is everything before the literal `-` segment).
/// - GitHub: `.../<owner>/<repo>/pull/<n>`.
///
/// A trailing slash and any query string/fragment are ignored. Any other
/// shape, or a non-numeric/missing MR number, returns `CtError::Usage`.
pub fn parse_mr_url(url: &str) -> CtResult<MrCoordinates> {
    let parsed = url::Url::parse(url).map_err(|_| unparseable(url))?;
    let path = parsed.path().trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if let Some(idx) = segments.iter().position(|s| *s == "-")
        && segments.get(idx + 1) == Some(&"merge_requests")
    {
        return build_coordinates(
            &segments[..idx],
            segments.get(idx + 2).copied(),
            MrProvider::Gitlab,
            url,
        );
    }

    if let Some(idx) = segments.iter().position(|s| *s == "pull") {
        return build_coordinates(
            &segments[..idx],
            segments.get(idx + 1).copied(),
            MrProvider::Github,
            url,
        );
    }

    Err(unparseable(url))
}

/// Shared GitLab/GitHub coordinate builder: `project_segments` is everything
/// before the provider's marker segment, `iid_segment` the MR/PR number.
fn build_coordinates(
    project_segments: &[&str],
    iid_segment: Option<&str>,
    provider: MrProvider,
    url: &str,
) -> CtResult<MrCoordinates> {
    let project_path = project_segments.join("/");
    let mr_iid = iid_segment
        .ok_or_else(|| unparseable(url))?
        .parse::<i64>()
        .map_err(|_| unparseable(url))?;
    if project_path.is_empty() {
        return Err(unparseable(url));
    }
    Ok(MrCoordinates {
        provider,
        project_path,
        mr_iid,
    })
}

fn unparseable(url: &str) -> CtError {
    CtError::Usage(format!(
        "could not parse a GitLab/GitHub merge-request URL: {url}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coords(url: &str) -> MrCoordinates {
        parse_mr_url(url).unwrap_or_else(|e| panic!("expected coordinates for {url}, got {e}"))
    }

    #[test]
    fn gitlab_url_parses() {
        let c = coords("https://gitlab.com/group/project/-/merge_requests/42");
        assert_eq!(c.provider, MrProvider::Gitlab);
        assert_eq!(c.project_path, "group/project");
        assert_eq!(c.mr_iid, 42);
    }

    #[test]
    fn github_url_parses() {
        let c = coords("https://github.com/owner/repo/pull/123");
        assert_eq!(c.provider, MrProvider::Github);
        assert_eq!(c.project_path, "owner/repo");
        assert_eq!(c.mr_iid, 123);
    }

    // CA-RV-EC1: trailing slash, query string, self-hosted host all normalize
    // to the same coordinates.
    #[test]
    fn ca_rv_ec1_trailing_slash_query_and_self_hosted_host_are_ignored() {
        let base = coords("https://gitlab.com/group/project/-/merge_requests/42");
        let trailing = coords("https://gitlab.com/group/project/-/merge_requests/42/");
        let query = coords("https://gitlab.com/group/project/-/merge_requests/42?tab=diffs");
        let self_hosted =
            coords("https://gitlab.mycompany.internal/group/project/-/merge_requests/42");
        assert_eq!(base, trailing);
        assert_eq!(base, query);
        assert_eq!(base.provider, self_hosted.provider);
        assert_eq!(base.project_path, self_hosted.project_path);
        assert_eq!(base.mr_iid, self_hosted.mr_iid);
    }

    // CA-RV-EC2: GitLab subgroup paths keep every segment before `-`.
    #[test]
    fn ca_rv_ec2_gitlab_subgroup_path() {
        let c = coords("https://gitlab.com/g/sub/repo/-/merge_requests/7");
        assert_eq!(c.provider, MrProvider::Gitlab);
        assert_eq!(c.project_path, "g/sub/repo");
        assert_eq!(c.mr_iid, 7);
    }

    // CA-RV-EC3: a non-numeric MR number is a usage error, not a panic.
    #[test]
    fn ca_rv_ec3_non_numeric_iid_is_usage_error() {
        let err = parse_mr_url("https://gitlab.com/group/project/-/merge_requests/abc")
            .expect_err("non-numeric iid must be rejected");
        assert!(matches!(err, CtError::Usage(_)), "got {err:?}");

        let err = parse_mr_url("https://github.com/owner/repo/pull/abc")
            .expect_err("non-numeric iid must be rejected");
        assert!(matches!(err, CtError::Usage(_)), "got {err:?}");
    }

    // CA-RV-SP1: an unparseable/unrecognized URL is a usage error.
    #[test]
    fn ca_rv_sp1_unrecognized_shape_is_usage_error() {
        let err = parse_mr_url("https://example.com/not-a-review-url")
            .expect_err("unrecognized shape must be rejected");
        assert!(matches!(err, CtError::Usage(_)), "got {err:?}");

        let err = parse_mr_url("not a url at all").expect_err("garbage input must be rejected");
        assert!(matches!(err, CtError::Usage(_)), "got {err:?}");
    }

    // Unsupported-provider URL shapes (Bitbucket etc.) don't match either
    // pattern, so they fall through to the same usage error (V1 scope).
    #[test]
    fn bitbucket_shaped_url_is_usage_error() {
        let err = parse_mr_url("https://bitbucket.org/owner/repo/pull-requests/9")
            .expect_err("bitbucket shape is unsupported in V1");
        assert!(matches!(err, CtError::Usage(_)), "got {err:?}");
    }

    // Missing MR number entirely (path ends at the marker segment).
    #[test]
    fn missing_iid_is_usage_error() {
        let err = parse_mr_url("https://gitlab.com/group/project/-/merge_requests")
            .expect_err("missing iid must be rejected");
        assert!(matches!(err, CtError::Usage(_)), "got {err:?}");
    }
}
