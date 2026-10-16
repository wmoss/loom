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
/// Per-commit ceiling for the message body, which keeps its paragraph breaks.
const MAX_BODY_BYTES: usize = 4_096;
/// Cap on the raw `git log` capture before parsing.
const MAX_LOG_BYTES: usize = 512 * 1024;

/// Six NUL-terminated fields per commit — oid, author name, author email,
/// authoring time, subject, and `%b` (the message body below the subject) —
/// flattened into one stream the parser chunks back into records. NUL is the
/// one byte no field can carry: Git's object headers are NUL-terminated, so a
/// commit message or author field holding one never makes it into the log as
/// a field value.
const LOG_FORMAT: &str = "%H%x00%an%x00%ae%x00%aI%x00%s%x00%b%x00";

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

/// The message body keeps its paragraph structure — newlines survive, other
/// control bytes are repaired — and clips to its bound with a marker.
pub(crate) fn body_text(value: &str, limit: usize) -> Option<String> {
    let trimmed = value.trim_matches(|ch: char| ch == '\n' || ch == '\r');
    if trimmed.is_empty() {
        return None;
    }
    let cleaned: String = trimmed
        .chars()
        .map(|ch| {
            if ch == '\n' || ch == '\t' || !ch.is_control() {
                ch
            } else {
                '\u{fffd}'
            }
        })
        .collect();
    if cleaned.len() <= limit {
        return Some(cleaned);
    }
    let mut end = limit;
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!("{}\u{2026}", &cleaned[..end]))
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

/// Parses the NUL-separated log into commits, newest first. Each commit
/// contributes exactly six NUL-terminated fields, so chunking the flat token
/// stream by six is deterministic — no field the author typed can be mistaken
/// for a separator. At most `limit` records are kept and a surplus marks the
/// listing truncated; a record cut off mid-way by the capture bound is
/// dropped, not half-shown.
pub(crate) fn parse_log(raw: &str, limit: usize) -> (Vec<SessionCommitDto>, bool) {
    let mut tokens: Vec<&str> = raw.split('\0').collect();
    // The capture bound cuts the final token mid-field, while a whole
    // capture ends on the format's terminator and splits into one trailing
    // empty token. Either way exactly one trailing token is not a field.
    tokens.pop();
    let mut commits = Vec::new();
    let mut truncated = false;
    for fields in tokens.chunks(6) {
        // Fewer than six tokens means the capture bound cut this record
        // mid-commit; the remnant is dropped, and the caller's capture flags
        // mark the listing truncated.
        if fields.len() < 6 {
            break;
        }
        if commits.len() == limit {
            truncated = true;
            break;
        }
        let clip = |value: &str| sanitize_text(value, MAX_FIELD_BYTES).0;
        commits.push(SessionCommitDto {
            oid: fields[0].trim().to_string(),
            author_name: clip(fields[1]),
            author_email: clip(fields[2]),
            authored_at: clip(fields[3]),
            subject: clip(fields[4]),
            body: body_text(fields[5], MAX_BODY_BYTES),
        });
    }
    (commits, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One record's six NUL-terminated fields, flattened like the capture.
    fn record_with_body(oid: &str, subject: &str, body: &str) -> String {
        format!(
            "{oid}\0Ada Lovelace\0ada@example.test\02026-10-05T10:00:00+00:00\0{subject}\0{body}\0"
        )
    }

    fn record(oid: &str, subject: &str) -> String {
        record_with_body(oid, subject, "")
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
    fn a_body_keeps_paragraph_breaks_and_omits_when_empty() {
        let raw = format!(
            "{}{}",
            record_with_body("a", "First", "why this exists\n\nand what it costs"),
            record("b", "Second")
        );
        let (commits, _) = parse(&raw);
        assert_eq!(
            commits[0].body.as_deref(),
            Some("why this exists\n\nand what it costs")
        );
        assert_eq!(commits[1].body, None);
    }

    #[test]
    fn an_overlong_body_clips_to_its_bound() {
        let long = "x".repeat(MAX_BODY_BYTES + 10);
        let raw = record_with_body("a", "First", &long);
        let (commits, _) = parse(&raw);
        let body = commits[0].body.clone().unwrap();
        assert_eq!(body.chars().count(), MAX_BODY_BYTES + 1);
        assert!(body.ends_with('\u{2026}'));
    }

    #[test]
    fn a_record_cut_off_mid_capture_is_dropped() {
        let raw = format!("{}abc\0partial", record("a", "First"));
        let (commits, truncated) = parse(&raw);
        assert!(!truncated);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].oid, "a");
    }

    #[test]
    fn overlong_fields_are_clipped() {
        let long = "x".repeat(MAX_FIELD_BYTES + 10);
        let raw = format!("a\0{long}\0e\02026\0s\0body\0");
        let (commits, _) = parse(&raw);
        // The clip keeps the bound's worth of content plus its ellipsis marker.
        assert_eq!(commits[0].author_name.chars().count(), MAX_FIELD_BYTES + 1);
    }

    #[test]
    fn a_separator_byte_in_a_message_body_is_field_content_not_a_split() {
        // An author can type \x1f or \x1e into a commit message; a NUL they
        // cannot — Git object headers are NUL-terminated — which is exactly
        // why the format delimits with NUL. The record must stay whole: the
        // body keeps its place, with its stray control bytes repaired for
        // display rather than misread as record breaks.
        let raw = record_with_body("a", "First", "unit\x1eseparator in\x1f the body");
        let (commits, _) = parse(&raw);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "First");
        assert_eq!(
            commits[0].body.as_deref(),
            Some("unit\u{fffd}separator in\u{fffd} the body")
        );
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
        assert_eq!(commits.commits[0].body, None);
        assert!(!commits.truncated);
        assert!(commits.head_oid.is_some());
    }
}
