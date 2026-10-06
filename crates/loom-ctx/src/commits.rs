//! The session branch's own commits, from the fork point with its base.
//!
//! Base resolution is shared with the changes snapshot — the same fork point a
//! review diffs against — so the two surfaces never disagree about where the
//! branch's own work starts. Git supplies the log under the same hardened
//! invocation; parsing and bounds are deterministic and side-effect free.

use anyhow::Result;
use std::path::Path;
use weaver_api::{
    ChangeBaseDto, ChangeBaseUnavailableReasonDto, SessionCommitDto, SessionCommitsDto,
};

use crate::changes::{
    bootstrap_text, capture_git_status, hardened_git, resolve_base, sanitize_text,
};

/// Listing bound; the response reports `truncated` rather than degrading.
pub const MAX_COMMITS: usize = 200;
/// Per-field ceiling (subject, author), mirroring the changes line bound.
const MAX_FIELD_BYTES: usize = 2_048;
/// Cap on the raw `git log` capture before parsing.
const MAX_LOG_BYTES: usize = 512 * 1024;

/// Field separator `\x1f` between one commit's fields, record separator `\x1e`
/// between commits; git's placeholders never emit either byte.
const LOG_FORMAT: &str = "%H%x1f%an%x1f%ae%x1f%aI%x1f%s%x1e";

/// Read one session worktree's own commits without fetching or changing Git
/// state. An unresolvable base is reported, not guessed — an empty list would
/// be indistinguishable from "nothing to show".
pub async fn load(work_dir: &Path, base_reference: &str) -> Result<SessionCommitsDto> {
    let head_oid = bootstrap_text(work_dir, &["rev-parse", "--verify", "HEAD"]).await?;
    let Some(head_oid) = head_oid else {
        return Ok(unavailable(
            base_reference,
            ChangeBaseUnavailableReasonDto::UnbornHead,
            None,
        ));
    };
    let base = match resolve_base(work_dir, base_reference, &head_oid).await? {
        Ok(base) => base,
        Err(reason) => return Ok(unavailable(base_reference, reason, Some(head_oid))),
    };
    let range = format!("{}..HEAD", base.oid);
    let (capture, success) = capture_git_status(
        hardened_git(work_dir),
        &[
            "log",
            &format!("-{}", MAX_COMMITS + 1),
            &format!("--format={LOG_FORMAT}"),
            &range,
        ],
        MAX_LOG_BYTES,
    )
    .await?;
    let (commits, parse_truncated) =
        parse_log(&String::from_utf8_lossy(&capture.bytes), MAX_COMMITS);
    Ok(SessionCommitsDto {
        base: ChangeBaseDto::Available {
            reference: base.reference,
            oid: base.oid,
        },
        head_oid: Some(head_oid),
        truncated: parse_truncated || capture.truncated || capture.timed_out || !success,
        commits,
    })
}

fn unavailable(
    reference: &str,
    reason: ChangeBaseUnavailableReasonDto,
    head_oid: Option<String>,
) -> SessionCommitsDto {
    SessionCommitsDto {
        base: ChangeBaseDto::Unavailable {
            reference: reference.to_string(),
            reason,
        },
        head_oid,
        commits: Vec::new(),
        truncated: false,
    }
}

/// Parses the separator-encoded log into commits, newest first. At most
/// `limit` records are kept and a surplus marks the listing truncated; a
/// record cut off mid-way by the capture bound is dropped, not half-shown.
pub(crate) fn parse_log(raw: &str, limit: usize) -> (Vec<SessionCommitDto>, bool) {
    let mut commits = Vec::new();
    let mut truncated = false;
    for record in raw.split('\x1e') {
        let fields: Vec<&str> = record.split('\x1f').collect();
        // An empty segment is the split's residue at either end of the
        // capture, not a commit.
        let Some(oid) = fields
            .first()
            .map(|oid| oid.trim())
            .filter(|oid| !oid.is_empty())
        else {
            continue;
        };
        // Fewer fields than the format writes means the capture bound cut
        // this record mid-commit; the remnant is dropped, and the caller's
        // capture flags mark the listing truncated.
        if fields.len() < 5 {
            continue;
        }
        if commits.len() == limit {
            truncated = true;
            break;
        }
        let clip = |value: &str| sanitize_text(value, MAX_FIELD_BYTES).0;
        commits.push(SessionCommitDto {
            oid: oid.to_string(),
            author_name: clip(fields[1]),
            author_email: clip(fields[2]),
            authored_at: clip(fields[3]),
            subject: clip(fields[4]),
        });
    }
    (commits, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEP: char = '\x1f';
    const REC: char = '\x1e';

    fn record(oid: &str, subject: &str) -> String {
        format!("{oid}{SEP}Ada Lovelace{SEP}ada@example.test{SEP}2026-10-05T10:00:00+00:00{SEP}{subject}{REC}")
    }

    fn parse(raw: &str) -> (Vec<SessionCommitDto>, bool) {
        parse_log(raw, MAX_COMMITS)
    }

    #[test]
    fn parses_newest_first_and_separators() {
        let raw = format!("{}{}", record("b", "Second"), record("a", "First"));
        let (commits, truncated) = parse(&raw);
        assert!(!truncated);
        assert_eq!(
            commits.iter().map(|c| c.oid.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert_eq!(commits[0].subject, "Second");
        assert_eq!(commits[0].author_name, "Ada Lovelace");
    }

    #[test]
    fn empty_log_is_an_empty_list() {
        let (commits, truncated) = parse("");
        assert!(commits.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn a_surplus_record_marks_truncation_instead_of_listing() {
        let raw: String = std::iter::repeat_with(|| record("0", "S"))
            .take(4)
            .collect();
        let (commits, truncated) = parse_log(&raw, 3);
        assert!(truncated);
        assert_eq!(commits.len(), 3);
    }

    #[test]
    fn a_record_cut_off_mid_capture_is_dropped() {
        let raw = format!("{}abc\x1fpartial", record("a", "First"));
        let (commits, truncated) = parse(&raw);
        assert!(!truncated);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].oid, "a");
    }

    #[test]
    fn overlong_fields_are_clipped() {
        let long = "x".repeat(MAX_FIELD_BYTES + 10);
        let raw = format!("a{SEP}{long}{SEP}e{SEP}2026{SEP}s{REC}");
        let (commits, _) = parse(&raw);
        // The clip keeps the bound's worth of content plus its ellipsis marker.
        assert_eq!(commits[0].author_name.chars().count(), MAX_FIELD_BYTES + 1);
    }

    #[tokio::test]
    async fn a_real_repo_lists_commits_over_the_fork_point() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(["-C", dir.path().to_str().unwrap()])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap()
        };
        assert!(git(&["init", "-b", "main"]).status.success());
        assert!(git(&["config", "user.name", "E2E"]).status.success());
        assert!(git(&["config", "user.email", "e2e@weaver.test"])
            .status
            .success());
        std::fs::write(dir.path().join("a"), "a\n").unwrap();
        assert!(git(&["add", "-A"]).status.success());
        assert!(git(&["commit", "-m", "base commit"]).status.success());
        assert!(git(&["switch", "-c", "feature"]).status.success());
        std::fs::write(dir.path().join("b"), "b\n").unwrap();
        assert!(git(&["add", "-A"]).status.success());
        assert!(git(&["commit", "-m", "branch commit"]).status.success());

        let commits = load(dir.path(), "main").await.unwrap();
        let ChangeBaseDto::Available { reference, oid: _ } = &commits.base else {
            panic!("base should resolve");
        };
        assert_eq!(reference, "main");
        assert_eq!(commits.commits.len(), 1);
        assert_eq!(commits.commits[0].subject, "branch commit");
        assert!(!commits.truncated);
        assert!(commits.head_oid.is_some());
    }
}
