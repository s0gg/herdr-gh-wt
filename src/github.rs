//! Fetching review requests and assigned issues from GitHub through `gh`.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::runner::{Cmd, Runner};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    PullRequest,
    Issue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reviewer {
    User(String),
    Team { org: String, slug: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub kind: Kind,
    /// Git host taken from the URL, e.g. `github.com`.
    pub host: String,
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<String>,
    /// RFC 3339 timestamp.
    pub updated_at: String,
    pub labels: Vec<String>,
    pub body: String,
    pub is_draft: bool,
    /// Head branch of a pull request.
    pub head_ref: Option<String>,
    /// Pending review requests of a pull request.
    pub reviewers: Vec<Reviewer>,
    /// Branches of the same repository linked to an issue on GitHub (the
    /// "Development" section of the issue).
    pub linked_branches: Vec<String>,
    /// Open pull requests from the same repository that will close an issue.
    pub closing_pull_requests: Vec<PullRef>,
}

/// A pull request and its head branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRef {
    pub number: u64,
    pub head_ref: String,
}

impl Item {
    /// `PR #12` or `Issue #34`.
    pub fn reference(&self) -> String {
        match self.kind {
            Kind::PullRequest => format!("PR #{}", self.number),
            Kind::Issue => format!("Issue #{}", self.number),
        }
    }

    /// The repository name without its owner.
    pub fn repo_name(&self) -> &str {
        self.repo.rsplit('/').next().unwrap_or(&self.repo)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dashboard {
    pub viewer: String,
    pub review_requests: Vec<Item>,
    pub assigned_issues: Vec<Item>,
}

/// Search for open pull requests that request a review from you, directly or
/// through one of your teams.
pub const REVIEW_REQUESTS_QUERY: &str =
    "is:open is:pr review-requested:@me archived:false sort:updated-desc";
/// Search for open issues assigned to you.
pub const ASSIGNED_ISSUES_QUERY: &str =
    "is:open is:issue assignee:@me archived:false sort:updated-desc";

pub const GRAPHQL: &str = "\
query($prQuery: String!, $issueQuery: String!, $limit: Int!) {
  viewer { login }
  reviewRequests: search(query: $prQuery, type: ISSUE, first: $limit) {
    nodes {
      ... on PullRequest {
        number title url isDraft updatedAt bodyText headRefName
        author { login }
        repository { nameWithOwner }
        labels(first: 20) { nodes { name } }
        reviewRequests(first: 30) {
          nodes {
            requestedReviewer {
              __typename
              ... on User { login }
              ... on Team { slug organization { login } }
            }
          }
        }
      }
    }
  }
  assignedIssues: search(query: $issueQuery, type: ISSUE, first: $limit) {
    nodes {
      ... on Issue {
        number title url updatedAt bodyText
        author { login }
        repository { nameWithOwner }
        labels(first: 20) { nodes { name } }
        linkedBranches(first: 10) {
          nodes { ref { name repository { nameWithOwner } } }
        }
        closedByPullRequestsReferences(first: 10, includeClosedPrs: false) {
          nodes { number headRefName headRepository { nameWithOwner } }
        }
      }
    }
  }
}";

/// The `gh api graphql` invocation that fetches the dashboard.
pub fn fetch_command(limit: u32) -> Cmd {
    let limit = limit.clamp(1, 100);
    Cmd::new(
        "gh",
        [
            "api".to_string(),
            "graphql".to_string(),
            "-f".to_string(),
            format!("query={GRAPHQL}"),
            "-f".to_string(),
            format!("prQuery={REVIEW_REQUESTS_QUERY}"),
            "-f".to_string(),
            format!("issueQuery={ASSIGNED_ISSUES_QUERY}"),
            "-F".to_string(),
            format!("limit={limit}"),
        ],
    )
}

/// `gh browse` invocation that opens `item` in the web browser.
pub fn browse_command(item: &Item) -> Cmd {
    Cmd::new(
        "gh",
        [
            "browse".to_string(),
            item.number.to_string(),
            "--repo".to_string(),
            format!("{}/{}", item.host, item.repo),
        ],
    )
}

pub fn fetch(runner: &dyn Runner, limit: u32) -> Result<Dashboard> {
    let json = runner.stdout(&fetch_command(limit))?;
    parse_dashboard(&json)
}

pub fn parse_dashboard(json: &str) -> Result<Dashboard> {
    let response: Response =
        serde_json::from_str(json).context("unexpected response from GitHub")?;
    if let Some(errors) = response.errors.filter(|e| !e.is_empty()) {
        let messages: Vec<_> = errors.into_iter().map(|e| e.message).collect();
        anyhow::bail!("GitHub API error: {}", messages.join("; "));
    }
    let data = response.data.context("GitHub returned no data")?;
    Ok(Dashboard {
        viewer: data.viewer.login,
        review_requests: data
            .review_requests
            .nodes
            .into_iter()
            .filter_map(|node| node.into_item(Kind::PullRequest))
            .collect(),
        assigned_issues: data
            .assigned_issues
            .nodes
            .into_iter()
            .filter_map(|node| node.into_item(Kind::Issue))
            .collect(),
    })
}

/// `https://github.com/acme/app/pull/1` -> `github.com`.
fn host_of(url: &str) -> String {
    url.split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

#[derive(Deserialize)]
struct Response {
    data: Option<Data>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    viewer: Login,
    review_requests: Connection<Node>,
    assigned_issues: Connection<Node>,
}

#[derive(Deserialize)]
struct Connection<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryRef {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct LinkedBranch {
    #[serde(rename = "ref")]
    reference: Option<BranchRef>,
}

#[derive(Deserialize)]
struct BranchRef {
    name: String,
    repository: RepositoryRef,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClosingPullRequest {
    number: u64,
    head_ref_name: String,
    head_repository: Option<RepositoryRef>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewRequest {
    requested_reviewer: Option<RequestedReviewer>,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum RequestedReviewer {
    User {
        login: String,
    },
    Team {
        slug: String,
        organization: Login,
    },
    #[serde(other)]
    Other,
}

/// A search result node. Results that match neither fragment come back as
/// `{}`, so every field is optional.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Node {
    number: Option<u64>,
    title: Option<String>,
    url: Option<String>,
    #[serde(default)]
    is_draft: bool,
    updated_at: Option<String>,
    body_text: Option<String>,
    head_ref_name: Option<String>,
    author: Option<Login>,
    repository: Option<RepositoryRef>,
    labels: Option<Connection<Named>>,
    review_requests: Option<Connection<ReviewRequest>>,
    linked_branches: Option<Connection<LinkedBranch>>,
    closed_by_pull_requests_references: Option<Connection<ClosingPullRequest>>,
}

impl Node {
    fn into_item(self, kind: Kind) -> Option<Item> {
        let url = self.url?;
        let repo = self.repository?.name_with_owner;
        let linked_branches = self
            .linked_branches
            .map(|c| c.nodes)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|node| node.reference)
            .filter(|r| r.repository.name_with_owner == repo)
            .map(|r| r.name)
            .collect();
        let closing_pull_requests = self
            .closed_by_pull_requests_references
            .map(|c| c.nodes)
            .unwrap_or_default()
            .into_iter()
            .filter(|pr| {
                pr.head_repository
                    .as_ref()
                    .is_some_and(|r| r.name_with_owner == repo)
            })
            .map(|pr| PullRef {
                number: pr.number,
                head_ref: pr.head_ref_name,
            })
            .collect();
        Some(Item {
            kind,
            host: host_of(&url),
            repo,
            number: self.number?,
            title: self.title.unwrap_or_default(),
            url,
            author: self.author.map(|a| a.login),
            updated_at: self.updated_at.unwrap_or_default(),
            labels: self
                .labels
                .map(|l| l.nodes.into_iter().map(|n| n.name).collect())
                .unwrap_or_default(),
            body: self.body_text.unwrap_or_default(),
            is_draft: self.is_draft,
            head_ref: self.head_ref_name,
            reviewers: self
                .review_requests
                .map(|r| {
                    r.nodes
                        .into_iter()
                        .filter_map(|req| match req.requested_reviewer? {
                            RequestedReviewer::User { login } => Some(Reviewer::User(login)),
                            RequestedReviewer::Team { slug, organization } => {
                                Some(Reviewer::Team {
                                    org: organization.login,
                                    slug,
                                })
                            }
                            RequestedReviewer::Other => None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            linked_branches,
            closing_pull_requests,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Output;
    use crate::runner::fake::FakeRunner;

    const RESPONSE: &str = r#"{
      "data": {
        "viewer": { "login": "octocat" },
        "reviewRequests": { "nodes": [
          {
            "number": 101, "title": "Add dark mode", "url": "https://github.com/acme/app/pull/101",
            "isDraft": false, "updatedAt": "2026-11-20T10:00:00Z", "bodyText": "Adds it.",
            "headRefName": "feature/dark-mode",
            "author": { "login": "alice" },
            "repository": { "nameWithOwner": "acme/app" },
            "labels": { "nodes": [ { "name": "ui" } ] },
            "reviewRequests": { "nodes": [
              { "requestedReviewer": { "__typename": "Team", "slug": "frontend", "organization": { "login": "acme" } } },
              { "requestedReviewer": { "__typename": "User", "login": "octocat" } },
              { "requestedReviewer": { "__typename": "Mannequin" } },
              { "requestedReviewer": null }
            ] }
          },
          {
            "number": 7, "title": "Draft", "url": "https://ghe.example.com/acme/lib/pull/7",
            "isDraft": true, "updatedAt": "2026-11-20T00:00:00Z", "bodyText": "",
            "headRefName": "wip", "author": null,
            "repository": { "nameWithOwner": "acme/lib" },
            "labels": { "nodes": [] },
            "reviewRequests": { "nodes": [] }
          },
          {}
        ] },
        "assignedIssues": { "nodes": [
          {
            "number": 202, "title": "ログイン画面の改善", "url": "https://github.com/acme/app/issues/202",
            "updatedAt": "2026-11-19T09:00:00Z", "bodyText": "Body",
            "author": { "login": "bob" },
            "repository": { "nameWithOwner": "acme/app" },
            "labels": { "nodes": [ { "name": "enhancement" }, { "name": "bug" } ] },
            "linkedBranches": { "nodes": [
              { "ref": { "name": "202-login-screen", "repository": { "nameWithOwner": "acme/app" } } },
              { "ref": { "name": "elsewhere", "repository": { "nameWithOwner": "someone/app" } } },
              { "ref": null }
            ] },
            "closedByPullRequestsReferences": { "nodes": [
              { "number": 303, "headRefName": "login-redesign", "headRepository": { "nameWithOwner": "acme/app" } },
              { "number": 304, "headRefName": "patch-1", "headRepository": { "nameWithOwner": "fork/app" } },
              { "number": 305, "headRefName": "gone", "headRepository": null }
            ] }
          }
        ] }
      }
    }"#;

    #[test]
    fn parses_viewer_pull_requests_and_issues() {
        let dashboard = parse_dashboard(RESPONSE).unwrap();
        assert_eq!(dashboard.viewer, "octocat");
        assert_eq!(
            dashboard.review_requests.len(),
            2,
            "empty nodes (non-matching fragments) are skipped"
        );
        assert_eq!(dashboard.assigned_issues.len(), 1);

        let pr = &dashboard.review_requests[0];
        assert_eq!(pr.kind, Kind::PullRequest);
        assert_eq!(pr.host, "github.com");
        assert_eq!(pr.repo, "acme/app");
        assert_eq!(pr.repo_name(), "app");
        assert_eq!(pr.number, 101);
        assert_eq!(pr.title, "Add dark mode");
        assert_eq!(pr.author.as_deref(), Some("alice"));
        assert_eq!(pr.updated_at, "2026-11-20T10:00:00Z");
        assert_eq!(pr.labels, vec!["ui"]);
        assert_eq!(pr.body, "Adds it.");
        assert_eq!(pr.head_ref.as_deref(), Some("feature/dark-mode"));
        assert_eq!(
            pr.reviewers,
            vec![
                Reviewer::Team {
                    org: "acme".into(),
                    slug: "frontend".into()
                },
                Reviewer::User("octocat".into()),
            ]
        );
        assert_eq!(pr.reference(), "PR #101");

        let draft = &dashboard.review_requests[1];
        assert!(draft.is_draft);
        assert_eq!(draft.host, "ghe.example.com");
        assert_eq!(draft.author, None);

        let issue = &dashboard.assigned_issues[0];
        assert_eq!(issue.kind, Kind::Issue);
        assert_eq!(issue.title, "ログイン画面の改善");
        assert_eq!(issue.labels, vec!["enhancement", "bug"]);
        assert_eq!(issue.head_ref, None);
        assert_eq!(issue.reviewers, vec![]);
        assert_eq!(issue.reference(), "Issue #202");
        assert_eq!(
            issue.linked_branches,
            vec!["202-login-screen"],
            "branches of other repositories are left out"
        );
        assert_eq!(
            issue.closing_pull_requests,
            vec![PullRef {
                number: 303,
                head_ref: "login-redesign".into()
            }],
            "pull requests from forks are left out"
        );
        assert_eq!(pr.linked_branches, Vec::<String>::new());
        assert_eq!(pr.closing_pull_requests, vec![]);
    }

    #[test]
    fn the_query_asks_for_the_branches_of_issues() {
        let issues = &GRAPHQL[GRAPHQL.find("assignedIssues").unwrap()..];
        assert!(issues.contains("linkedBranches"), "{issues}");
        assert!(
            issues.contains("closedByPullRequestsReferences(first: 10, includeClosedPrs: false)"),
            "{issues}"
        );
    }

    #[test]
    fn browse_command_opens_the_item_in_its_repository() {
        let dashboard = parse_dashboard(RESPONSE).unwrap();
        assert_eq!(
            browse_command(&dashboard.review_requests[0]).line(),
            "gh browse 101 --repo github.com/acme/app"
        );
        assert_eq!(
            browse_command(&dashboard.review_requests[1]).line(),
            "gh browse 7 --repo ghe.example.com/acme/lib"
        );
    }

    #[test]
    fn graphql_errors_are_reported() {
        let err = parse_dashboard(r#"{"data":null,"errors":[{"message":"Bad credentials"}]}"#)
            .unwrap_err();
        assert!(format!("{err:#}").contains("Bad credentials"), "{err:#}");
    }

    #[test]
    fn fetch_command_passes_queries_and_limit() {
        let cmd = fetch_command(50);
        assert_eq!(cmd.program, "gh");
        assert_eq!(&cmd.args[..2], ["api", "graphql"]);
        assert!(cmd.args.contains(&format!("query={GRAPHQL}")));
        assert!(
            cmd.args
                .contains(&format!("prQuery={REVIEW_REQUESTS_QUERY}"))
        );
        assert!(
            cmd.args
                .contains(&format!("issueQuery={ASSIGNED_ISSUES_QUERY}"))
        );
        assert!(cmd.args.windows(2).any(|w| w == ["-F", "limit=50"]));
    }

    #[test]
    fn fetch_clamps_the_limit_to_the_search_maximum() {
        assert!(
            fetch_command(500)
                .args
                .windows(2)
                .any(|w| w == ["-F", "limit=100"])
        );
        assert!(
            fetch_command(0)
                .args
                .windows(2)
                .any(|w| w == ["-F", "limit=1"])
        );
    }

    #[test]
    fn fetch_runs_gh_and_parses_the_result() {
        let runner = FakeRunner::new(|_| Ok(Output::ok(RESPONSE)));
        let dashboard = fetch(&runner, 100).unwrap();
        assert_eq!(dashboard.review_requests.len(), 2);
        assert_eq!(runner.calls()[0].program, "gh");
    }

    #[test]
    fn fetch_surfaces_gh_failures() {
        let runner = FakeRunner::new(|_| {
            Ok(Output::failed(
                "gh: To get started with GitHub CLI, please run: gh auth login",
            ))
        });
        let err = fetch(&runner, 100).unwrap_err();
        assert!(format!("{err:#}").contains("gh auth login"), "{err:#}");
    }
}
