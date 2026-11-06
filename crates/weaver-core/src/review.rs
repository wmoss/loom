//! Durable staged reviews over versioned subjects.
//!
//! Drafts are private to their creator and scoped to the session where the
//! review was started. Artifact comments carry their own subject revision and a
//! bounded text anchor with complete relocation context. Every draft mutation
//! advances an optimistic revision. Submission checks the current artifact
//! revision, freezes the exact structured message, records one branch event,
//! and creates one delivery-outbox row in the same immediate transaction.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, Row, Sqlite};
use std::fmt;

use crate::db::{now_iso, Db};
use crate::events::Event;

pub const MAX_COMMENTS: i64 = 100;
pub const MAX_COMMENT_BYTES: usize = 8 * 1024;
/// Per-comment ceiling on session replies. The review-wide bound that keeps
/// threads from growing without limit is the shared byte budget over every
/// reply a review holds, checked in [`reply_comment`].
pub const MAX_REPLIES_PER_COMMENT: i64 = 100;
pub const MAX_ANCHOR_BYTES: usize = 8 * 1024;
pub const MAX_REVIEW_BYTES: i64 = 256 * 1024;
pub const MAX_SUMMARY_BYTES: usize = 8 * 1024;
pub const MAX_QUOTE_BYTES: usize = 4 * 1024;
pub const MAX_CONTEXT_BYTES: usize = 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactTextAnchor {
    pub quote: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub suffix: String,
    #[serde(default)]
    pub block_index: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeLineAnchor {
    pub path_bytes: String,
    pub path_display: String,
    pub side: String,
    pub start_line: u32,
    pub end_line: u32,
    pub hunk_header: String,
    #[serde(default)]
    pub context_before: Vec<String>,
    pub selected: Vec<String>,
    #[serde(default)]
    pub context_after: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ReviewAnchor {
    Text(ArtifactTextAnchor),
    Change(ChangeLineAnchor),
}

impl From<ArtifactTextAnchor> for ReviewAnchor {
    fn from(value: ArtifactTextAnchor) -> Self {
        Self::Text(value)
    }
}

impl ReviewAnchor {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Change(_) => "change",
        }
    }

    fn json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    fn value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[derive(Debug)]
pub struct DraftRevisionConflict {
    pub expected: i64,
    pub actual: i64,
}

impl fmt::Display for DraftRevisionConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "draft changed (expected revision {}, current revision {})",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for DraftRevisionConflict {}

/// The snapshot a changes-review version names: the oid of the one commit it
/// pins (`changes-commit-v1:<oid>:<digest>`), or `None` for the other
/// spellings. A review and all its comments must name the same
/// one — comments on one commit's diff never ride a branch-scope envelope.
/// The one parser for this version spelling; `loom_ctx::changes` delegates
/// its own `commit_for_version` here so the two can never drift.
pub fn pinned_commit(version: &str) -> Option<&str> {
    version
        .strip_prefix("changes-commit-v1:")
        .and_then(|rest| rest.split_once(':'))
        .map(|(oid, _)| oid)
        .filter(|oid| !oid.is_empty())
}

/// True for the unstaged-diff spelling of a changes-review version
/// (`changes-unstaged-v1:<digest>`): the working tree against the index, a
/// live snapshot like the branch change-set rather than an immutable commit.
/// The one parser for this spelling; `loom_ctx::changes` delegates its own
/// `is_unstaged_version` here so the two can never drift.
pub fn is_unstaged_version(version: &str) -> bool {
    version.starts_with("changes-unstaged-v1:")
}

/// The snapshot scope a changes-review version names, as one comparable key:
/// a pinned commit, the unstaged working state, or the branch change-set. A
/// review's envelope and every comment on it must carry the same scope —
/// the key, not the full version, is what moves within a scope when a live
/// snapshot changes underneath.
pub fn changes_scope(version: &str) -> String {
    if let Some(oid) = pinned_commit(version) {
        return format!("commit:{oid}");
    }
    if is_unstaged_version(version) {
        return "unstaged".to_string();
    }
    "branch".to_string()
}

#[derive(Debug, Clone, FromRow)]
pub struct Review {
    pub id: i64,
    pub repo_root: String,
    pub branch_id: String,
    pub session_id: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub subject_key: String,
    pub subject_label: String,
    pub subject_version: String,
    pub status: String,
    pub summary: String,
    /// The note as the session last received it — the summary's delivered
    /// cursor. A summary that differs from it is pending words, riding the
    /// next round like a pending comment does.
    pub delivered_summary: String,
    pub draft_revision: i64,
    pub created_by: String,
    pub delivery_state: String,
    pub delivery_error: Option<String>,
    pub delivery_key: String,
    pub created_at: String,
    pub updated_at: String,
    pub submitted_at: Option<String>,
    /// Follow-up rounds queued so far: 0 means only the initial submission.
    pub reply_round: i64,
    /// The watermark of queued operator replies: the greatest reply id
    /// already included in a round, so the pending ones are exactly those
    /// past it.
    pub reply_upto: i64,
    /// The delivered-comment cursor: the greatest comment id whose body has
    /// reached the session. Comments past it are the round batch's half of
    /// everything not yet delivered.
    pub comment_upto: i64,
    /// Operator replies still waiting for the next round — computed on read,
    /// never stored.
    #[sqlx(default)]
    pub pending_replies: u32,
    /// The note the live, undelivered batch carries — `None` when no batch
    /// is in flight. The follow-up surface's gate compares against this
    /// first, so words already queued for the session stop reading as
    /// pending, exactly as the submit path's no-op check does.
    pub queued_summary: Option<String>,
    #[sqlx(skip)]
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ReviewComment {
    pub id: i64,
    pub review_id: i64,
    pub subject_version: String,
    pub anchor_kind: String,
    pub anchor_json: String,
    pub body: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    #[sqlx(skip)]
    pub replies: Vec<ReviewCommentReply>,
}

/// The reviewed session's own response to one review comment. Replies append
/// after submit and never mutate the comment they answer; resolving stays an
/// operator action. A comment's replies form one flat, oldest-first
/// conversation — both the session and operators append, with no
/// reply-to-reply targeting.
#[derive(Debug, Clone, FromRow)]
pub struct ReviewCommentReply {
    pub id: i64,
    pub comment_id: i64,
    /// Who appended: the reviewed session (whose identity is the review's
    /// own) or an operator, by username.
    pub author_kind: String,
    /// The operator's username; empty for the session.
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// Who is appending to a comment's thread.
#[derive(Debug, Clone, Copy)]
pub enum ReplyAuthor<'a> {
    /// The reviewed session — it may answer its own review's comments.
    Session(&'a str),
    /// An operator, by username — any operator may join any submitted
    /// review's threads, the same reach `set_comment_resolved` grants.
    Operator(&'a str),
}

impl ReplyAuthor<'_> {
    fn kind(&self) -> &'static str {
        match self {
            ReplyAuthor::Session(_) => "session",
            ReplyAuthor::Operator(_) => "operator",
        }
    }

    fn name(&self) -> &str {
        match self {
            ReplyAuthor::Session(_) => "",
            ReplyAuthor::Operator(name) => name,
        }
    }
}

impl ReviewComment {
    pub fn anchor(&self) -> ReviewAnchor {
        match self.anchor_kind.as_str() {
            "change" => serde_json::from_str(&self.anchor_json)
                .map(ReviewAnchor::Change)
                .unwrap_or_else(|_| ReviewAnchor::Change(ChangeLineAnchor::default())),
            _ => serde_json::from_str(&self.anchor_json)
                .map(ReviewAnchor::Text)
                .unwrap_or_else(|_| ReviewAnchor::Text(ArtifactTextAnchor::default())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewReview<'a> {
    pub repo_root: &'a str,
    pub branch_id: &'a str,
    pub session_id: &'a str,
    pub subject_kind: &'a str,
    pub subject_id: &'a str,
    pub subject_key: &'a str,
    pub subject_label: &'a str,
    pub subject_version: &'a str,
    pub created_by: &'a str,
}

#[derive(Debug, Clone)]
pub struct NewComment<'a> {
    pub subject_version: &'a str,
    pub anchor: &'a ReviewAnchor,
    pub body: &'a str,
}

#[derive(Debug, Clone, Default)]
pub struct CommentPatch<'a> {
    pub subject_version: Option<&'a str>,
    pub anchor: Option<&'a ReviewAnchor>,
    pub body: Option<&'a str>,
}

#[derive(Debug, Clone, Default)]
pub struct DraftPatch<'a> {
    pub summary: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct Submission {
    pub review: Review,
    pub event: Option<Event>,
}

#[derive(Debug, Clone, FromRow)]
pub struct OutboxItem {
    pub review_id: i64,
    pub delivery_key: String,
    pub state: String,
    pub attempts: i64,
    pub next_attempt_at: String,
    pub last_error: Option<String>,
    pub lease_token: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct DeliveryLease {
    pub token: String,
    /// The round's reply window: floor and cutoff snapshotted at submit.
    /// (0, 0) is the initial submission, which carries comments, not
    /// replies.
    pub reply_from: i64,
    pub reply_upto: i64,
    /// The greatest comment id the round's payload covers.
    pub comment_upto: i64,
    /// The round's frozen payload; the initial submission carries none and
    /// builds from the durable review at delivery time.
    pub payload: Option<String>,
}

const SELECT_REVIEW: &str = "SELECT id, repo_root, branch_id, session_id, subject_kind, \
    subject_id, subject_key, subject_label, subject_version, status, summary, \
    delivered_summary, draft_revision, created_by, \
    delivery_state, delivery_error, delivery_key, created_at, \
    updated_at, submitted_at, reply_round, reply_upto, comment_upto, \
    (SELECT o.summary FROM review_delivery_outbox o \
      WHERE o.review_id = reviews.id AND o.state != 'delivered') AS queued_summary \
    FROM reviews";

const SELECT_COMMENT: &str = "SELECT id, review_id, subject_version, anchor_kind, anchor_json, \
    body, status, created_at, updated_at FROM review_comments";

const SELECT_REPLY: &str = "SELECT id, comment_id, author_kind, author, body, created_at \
    FROM review_comment_replies";

fn validate_anchor(anchor: &ReviewAnchor) -> Result<()> {
    match anchor {
        ReviewAnchor::Text(anchor) => {
            if anchor.quote.trim().is_empty() {
                bail!("a non-empty text quote is required");
            }
            if anchor.quote.len() > MAX_QUOTE_BYTES {
                bail!("anchor quote exceeds the 4 KiB limit");
            }
            if anchor.prefix.len() > MAX_CONTEXT_BYTES || anchor.suffix.len() > MAX_CONTEXT_BYTES {
                bail!("anchor prefix and suffix are limited to 1 KiB each");
            }
            if anchor.block_index.is_some_and(|index| index < 0) {
                bail!("anchor block_index must be non-negative");
            }
        }
        ReviewAnchor::Change(anchor) => {
            if anchor.path_bytes.is_empty() {
                bail!("a change path identity is required");
            }
            if !matches!(anchor.side.as_str(), "old" | "new") {
                bail!("change anchor side must be old or new");
            }
            if anchor.start_line == 0 || anchor.end_line < anchor.start_line {
                bail!("change anchor range is invalid");
            }
            if anchor.selected.is_empty() {
                bail!("change anchor selected context is required");
            }
            for line in anchor
                .context_before
                .iter()
                .chain(&anchor.selected)
                .chain(&anchor.context_after)
            {
                if line.len() > MAX_CONTEXT_BYTES {
                    bail!("change anchor context lines are limited to 1 KiB");
                }
            }
        }
    }
    if anchor.json()?.len() > MAX_ANCHOR_BYTES {
        bail!("comment anchor exceeds the 8 KiB limit");
    }
    Ok(())
}

fn validate_comment(body: &str, anchor: &ReviewAnchor) -> Result<()> {
    let body = body.trim();
    if body.is_empty() {
        bail!("comment body is required");
    }
    if body.len() > MAX_COMMENT_BYTES {
        bail!("comment body exceeds the 8 KiB limit");
    }
    validate_anchor(anchor)
}

fn validate_summary(summary: &str) -> Result<()> {
    if summary.len() > MAX_SUMMARY_BYTES {
        bail!("review summary exceeds the 8 KiB limit");
    }
    Ok(())
}

async fn comments_for<'e, E>(executor: E, review_id: i64) -> Result<Vec<ReviewComment>>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let comments: Vec<ReviewComment> = sqlx::query_as::<_, ReviewComment>(&format!(
        "{SELECT_COMMENT} WHERE review_id = ? ORDER BY id"
    ))
    .bind(review_id)
    .fetch_all(executor)
    .await?;
    Ok(comments)
}

/// Loads every reply to the given comments in one query and folds each onto
/// its comment, oldest first.
async fn attach_replies<'e, E>(executor: E, comments: &mut [ReviewComment]) -> Result<()>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    if comments.is_empty() {
        return Ok(());
    }
    let ids = comments
        .iter()
        .map(|comment| comment.id)
        .collect::<Vec<_>>();
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql =
        format!("{SELECT_REPLY} WHERE comment_id IN ({placeholders}) ORDER BY comment_id, id");
    let mut query = sqlx::query_as::<_, ReviewCommentReply>(&sql);
    for id in &ids {
        query = query.bind(id);
    }
    let replies = query.fetch_all(executor).await?;
    let mut by_comment = std::collections::BTreeMap::<i64, Vec<ReviewCommentReply>>::new();
    for reply in replies {
        by_comment.entry(reply.comment_id).or_default().push(reply);
    }
    for comment in comments.iter_mut() {
        comment.replies = by_comment.remove(&comment.id).unwrap_or_default();
    }
    Ok(())
}

async fn attach_comments(db: &Db, mut review: Review) -> Result<Review> {
    let mut comments = comments_for(db, review.id).await?;
    attach_replies(db, &mut comments).await?;
    review.comments = comments;
    review.pending_replies = pending_reply_count(db, review.id, review.reply_upto).await?;
    Ok(review)
}

async fn attach_comments_tx(
    tx: &mut crate::db::DbTransaction<'_>,
    mut review: Review,
) -> Result<Review> {
    let mut comments = comments_for(&mut **tx, review.id).await?;
    attach_replies(&mut **tx, &mut comments).await?;
    review.comments = comments;
    review.pending_replies = pending_reply_count(&mut **tx, review.id, review.reply_upto).await?;
    Ok(review)
}

/// Operator replies past the queued watermark: the reviewer's words waiting
/// for the next round to carry them back to the reviewed session.
async fn pending_reply_count<'e, E>(executor: E, review_id: i64, upto: i64) -> Result<u32>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM review_comment_replies r
         JOIN review_comments c ON c.id = r.comment_id
         WHERE c.review_id = ? AND r.author_kind = 'operator' AND r.id > ?",
    )
    .bind(review_id)
    .bind(upto)
    .fetch_one(executor)
    .await?;
    Ok(count.max(0) as u32)
}

/// The greatest **operator** reply id on a review's threads: the cutoff a
/// round snapshots so retries derive the same set. Session replies never
/// extend it — they are the session's own words, and a round that carried
/// none of the reviewer's would deliver an empty "the reviewer replied"
/// message into the conversation it answers.
async fn max_operator_reply_id<'e, E>(executor: E, review_id: i64) -> Result<i64>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(r.id) FROM review_comment_replies r
         JOIN review_comments c ON c.id = r.comment_id
         WHERE c.review_id = ? AND r.author_kind = 'operator'",
    )
    .bind(review_id)
    .fetch_optional(executor)
    .await?
    .flatten();
    Ok(id.unwrap_or(0))
}

pub async fn get(db: &Db, id: i64) -> Result<Option<Review>> {
    let review = sqlx::query_as::<_, Review>(&format!("{SELECT_REVIEW} WHERE id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await?;
    match review {
        Some(review) => Ok(Some(attach_comments(db, review).await?)),
        None => Ok(None),
    }
}

pub async fn get_visible(db: &Db, id: i64, viewer: &str) -> Result<Option<Review>> {
    let review = sqlx::query_as::<_, Review>(&format!(
        "{SELECT_REVIEW} WHERE id = ? AND (status != 'draft' OR created_by = ?)"
    ))
    .bind(id)
    .bind(viewer)
    .fetch_optional(db)
    .await?;
    match review {
        Some(review) => Ok(Some(attach_comments(db, review).await?)),
        None => Ok(None),
    }
}

pub async fn list_visible(
    db: &Db,
    branch_id: &str,
    session_id: &str,
    subject_kind: &str,
    subject_id: &str,
    viewer: &str,
) -> Result<Vec<Review>> {
    let rows = sqlx::query_as::<_, Review>(&format!(
        "{SELECT_REVIEW}
         WHERE branch_id = ? AND session_id = ? AND subject_kind = ? AND subject_id = ?
           AND (status != 'draft' OR created_by = ?)
         ORDER BY (status = 'draft') DESC, id"
    ))
    .bind(branch_id)
    .bind(session_id)
    .bind(subject_kind)
    .bind(subject_id)
    .bind(viewer)
    .fetch_all(db)
    .await?;
    let mut reviews = Vec::with_capacity(rows.len());
    for review in rows {
        reviews.push(attach_comments(db, review).await?);
    }
    Ok(reviews)
}

/// Return the creator's open draft for this exact session/subject, or
/// create it. The partial unique index is the concurrency guard. (A
/// submitted review stays open to its reviewer at the comment level — new
/// comments attach to it directly — but it is not resolved here: composing a
/// fresh draft is the CLI's round-by-round workflow, while the review UI
/// continues the submitted envelope.)
pub async fn get_or_create(db: &Db, new: &NewReview<'_>) -> Result<Review> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO reviews
            (repo_root, branch_id, session_id, subject_kind, subject_id, subject_key, subject_label,
             subject_version, created_by, delivery_key, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'review:' || lower(hex(randomblob(16))), ?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(new.repo_root)
    .bind(new.branch_id)
    .bind(new.session_id)
    .bind(new.subject_kind)
    .bind(new.subject_id)
    .bind(new.subject_key)
    .bind(new.subject_label)
    .bind(new.subject_version)
    .bind(new.created_by)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;

    let review = sqlx::query_as::<_, Review>(&format!(
        "{SELECT_REVIEW}
         WHERE created_by = ? AND session_id = ? AND subject_kind = ? AND subject_id = ?
           AND status = 'draft'"
    ))
    .bind(new.created_by)
    .bind(new.session_id)
    .bind(new.subject_kind)
    .bind(new.subject_id)
    .fetch_one(db)
    .await?;
    attach_comments(db, review).await
}

async fn require_creator_draft(
    tx: &mut crate::db::DbTransaction<'_>,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
) -> Result<Review> {
    let review = sqlx::query_as::<_, Review>(&format!(
        "{SELECT_REVIEW} WHERE id = ? AND created_by = ? AND status = 'draft'"
    ))
    .bind(review_id)
    .bind(creator)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| anyhow!("draft review not found"))?;
    if review.draft_revision != expected_revision {
        return Err(anyhow!(DraftRevisionConflict {
            expected: expected_revision,
            actual: review.draft_revision,
        }));
    }
    Ok(review)
}

/// The review a comment may be added to, or a pending comment edited on:
/// the creator's draft, or their submitted review — a submitted review
/// stays open to its reviewer, and a comment the session has not received
/// yet is as editable as a draft comment. A delivered comment is immutable
/// history: it can be replied to or resolved, never rewritten.
async fn require_comment_surface(
    tx: &mut crate::db::DbTransaction<'_>,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
    // When set, the named comment must itself be pending to edit or delete.
    pending_comment_id: Option<i64>,
) -> Result<Review> {
    let review = sqlx::query_as::<_, Review>(&format!(
        "{SELECT_REVIEW} WHERE id = ? AND created_by = ? AND status IN ('draft', 'submitted')"
    ))
    .bind(review_id)
    .bind(creator)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| anyhow!("review not found"))?;
    if review.draft_revision != expected_revision {
        return Err(anyhow!(DraftRevisionConflict {
            expected: expected_revision,
            actual: review.draft_revision,
        }));
    }
    if review.status == "submitted" {
        if let Some(comment_id) = pending_comment_id {
            let status: Option<String> = sqlx::query_scalar(
                "SELECT status FROM review_comments WHERE id = ? AND review_id = ?",
            )
            .bind(comment_id)
            .bind(review_id)
            .fetch_optional(&mut **tx)
            .await?
            .flatten();
            if status.as_deref() != Some("pending") {
                bail!("this comment was already delivered; reply to it or resolve it instead");
            }
        }
    }
    Ok(review)
}

/// Bump the optimistic revision on an open comment surface — a draft, or a
/// submitted review taking on pending comments — and return the fresh view.
async fn finish_surface_mutation(
    tx: &mut crate::db::DbTransaction<'_>,
    review_id: i64,
    expected_revision: i64,
    now: &str,
) -> Result<Review> {
    let updated = sqlx::query(
        "UPDATE reviews
         SET draft_revision = draft_revision + 1, updated_at = ?
         WHERE id = ? AND status IN ('draft', 'submitted') AND draft_revision = ?",
    )
    .bind(now)
    .bind(review_id)
    .bind(expected_revision)
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        bail!("review changed while applying the mutation");
    }
    let review = sqlx::query_as::<_, Review>(&format!("{SELECT_REVIEW} WHERE id = ?"))
        .bind(review_id)
        .fetch_one(&mut **tx)
        .await?;
    attach_comments_tx(tx, review).await
}

pub async fn add_comment(
    db: &Db,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
    new: &NewComment<'_>,
) -> Result<Review> {
    validate_comment(new.body, new.anchor)?;
    let body = new.body.trim();
    let anchor_json = new.anchor.json()?;
    let mut tx = crate::db::begin_immediate(db).await?;
    let draft =
        require_comment_surface(&mut tx, review_id, creator, expected_revision, None).await?;
    // A changes review's comments all target one snapshot spelling — the
    // branch change-set, one commit's own diff, or the unstaged working
    // state. A comment naming another spelling would ride an envelope it
    // disagrees with through submit and delivery.
    if draft.subject_kind == "changes"
        && changes_scope(new.subject_version) != changes_scope(&draft.subject_version)
    {
        bail!(
            "this draft is scoped to another snapshot; submit or discard it before commenting on this one"
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM review_comments WHERE review_id = ?")
        .bind(review_id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_COMMENTS {
        bail!("a review may contain at most 100 comments");
    }
    let total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(
             length(CAST(body AS BLOB)) + length(CAST(anchor_json AS BLOB))
         ), 0)
         FROM review_comments WHERE review_id = ?",
    )
    .bind(review_id)
    .fetch_one(&mut *tx)
    .await?;
    if total + body.len() as i64 + anchor_json.len() as i64 > MAX_REVIEW_BYTES {
        bail!("review comments and anchors exceed the 256 KiB total limit");
    }
    // Comments are born pending on either surface: on a draft they await
    // the first submission, on a submitted review they await the follow-up
    // round, and the round flips them to delivered alongside the replies.
    let now = now_iso();
    sqlx::query(
        "INSERT INTO review_comments
            (review_id, subject_version, anchor_kind, anchor_json, body, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(review_id)
    .bind(new.subject_version)
    .bind(new.anchor.kind())
    .bind(anchor_json)
    .bind(body)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    let review = finish_surface_mutation(&mut tx, review_id, expected_revision, &now).await?;
    tx.commit().await?;
    Ok(review)
}

pub async fn patch_comment(
    db: &Db,
    review_id: i64,
    comment_id: i64,
    creator: &str,
    expected_revision: i64,
    patch: &CommentPatch<'_>,
) -> Result<Review> {
    if patch.body.is_none() && patch.subject_version.is_none() && patch.anchor.is_none() {
        bail!("comment update is empty");
    }
    if patch.subject_version.is_some() && patch.anchor.is_none() {
        bail!("a replacement anchor is required when changing comment revision");
    }
    if let Some(body) = patch.body {
        if body.trim().is_empty() {
            bail!("comment body is required");
        }
        if body.len() > MAX_COMMENT_BYTES {
            bail!("comment body exceeds the 8 KiB limit");
        }
    }
    let mut tx = crate::db::begin_immediate(db).await?;
    let draft = require_comment_surface(
        &mut tx,
        review_id,
        creator,
        expected_revision,
        Some(comment_id),
    )
    .await?;
    let current = sqlx::query_as::<_, ReviewComment>(&format!(
        "{SELECT_COMMENT} WHERE id = ? AND review_id = ?"
    ))
    .bind(comment_id)
    .bind(review_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow!("review comment not found"))?;

    // Re-anchoring may move a comment to a newer version of its snapshot,
    // never to the other spelling — the same scope the comment was born in.
    if let Some(subject_version) = patch.subject_version {
        if draft.subject_kind == "changes"
            && changes_scope(subject_version) != changes_scope(&draft.subject_version)
        {
            bail!("a comment re-anchors within its own snapshot, not onto another one");
        }
    }

    let subject_version = patch.subject_version.unwrap_or(&current.subject_version);
    let anchor_json = patch
        .anchor
        .map(ReviewAnchor::json)
        .transpose()?
        .unwrap_or(current.anchor_json);
    let body = patch.body.unwrap_or(&current.body).trim();
    let anchor = match current.anchor_kind.as_str() {
        "change" => ReviewAnchor::Change(
            serde_json::from_str(&anchor_json).context("decoding stored change anchor")?,
        ),
        _ => ReviewAnchor::Text(
            serde_json::from_str(&anchor_json).context("decoding stored text anchor")?,
        ),
    };
    validate_comment(body, &anchor)?;
    let total_without: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(
             length(CAST(body AS BLOB)) + length(CAST(anchor_json AS BLOB))
         ), 0)
         FROM review_comments WHERE review_id = ? AND id != ?",
    )
    .bind(review_id)
    .bind(comment_id)
    .fetch_one(&mut *tx)
    .await?;
    if total_without + body.len() as i64 + anchor_json.len() as i64 > MAX_REVIEW_BYTES {
        bail!("review comments and anchors exceed the 256 KiB total limit");
    }
    let now = now_iso();
    sqlx::query(
        "UPDATE review_comments
         SET subject_version = ?, anchor_kind = ?, anchor_json = ?, body = ?, updated_at = ?
         WHERE id = ? AND review_id = ?",
    )
    .bind(subject_version)
    .bind(
        patch
            .anchor
            .map(ReviewAnchor::kind)
            .unwrap_or(&current.anchor_kind),
    )
    .bind(anchor_json)
    .bind(body)
    .bind(&now)
    .bind(comment_id)
    .bind(review_id)
    .execute(&mut *tx)
    .await?;

    let now = now_iso();
    let review = finish_surface_mutation(&mut tx, review_id, expected_revision, &now).await?;
    tx.commit().await?;
    Ok(review)
}

pub async fn delete_comment(
    db: &Db,
    review_id: i64,
    comment_id: i64,
    creator: &str,
    expected_revision: i64,
) -> Result<Review> {
    let mut tx = crate::db::begin_immediate(db).await?;
    require_comment_surface(
        &mut tx,
        review_id,
        creator,
        expected_revision,
        Some(comment_id),
    )
    .await?;
    let removed = sqlx::query("DELETE FROM review_comments WHERE id = ? AND review_id = ?")
        .bind(comment_id)
        .bind(review_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        == 1;
    if !removed {
        bail!("review comment not found");
    }
    let now = now_iso();
    let review = finish_surface_mutation(&mut tx, review_id, expected_revision, &now).await?;
    tx.commit().await?;
    Ok(review)
}

pub async fn update_draft(
    db: &Db,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
    patch: &DraftPatch<'_>,
) -> Result<Review> {
    if let Some(summary) = patch.summary {
        validate_summary(summary)?;
    }
    if patch.summary.is_none() {
        bail!("draft update is empty");
    }
    let mut tx = crate::db::begin_immediate(db).await?;
    require_comment_surface(&mut tx, review_id, creator, expected_revision, None).await?;
    if let Some(summary) = patch.summary {
        sqlx::query("UPDATE reviews SET summary = ? WHERE id = ?")
            .bind(summary.trim())
            .bind(review_id)
            .execute(&mut *tx)
            .await?;
    }
    let now = now_iso();
    let review = finish_surface_mutation(&mut tx, review_id, expected_revision, &now).await?;
    tx.commit().await?;
    Ok(review)
}

/// Resolution is the one mutable bit of submitted thread state. Feedback
/// content and anchors remain frozen.
pub async fn set_comment_resolved(
    db: &Db,
    review_id: i64,
    comment_id: i64,
    resolved: bool,
) -> Result<ReviewComment> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let review_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM reviews
           WHERE id = ? AND status = 'submitted'
         )",
    )
    .bind(review_id)
    .fetch_one(&mut *tx)
    .await?;
    if !review_exists {
        bail!("submitted review not found");
    }
    let now = now_iso();
    let mut row = sqlx::query_as::<_, ReviewComment>(
        "UPDATE review_comments SET status = ?, updated_at = ?
         WHERE id = ? AND review_id = ? AND status IN ('submitted', 'resolved')
         RETURNING id, review_id, subject_version, anchor_kind, anchor_json, body, status,
                   created_at, updated_at",
    )
    .bind(if resolved { "resolved" } else { "submitted" })
    .bind(&now)
    .bind(comment_id)
    .bind(review_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow!("submitted review comment not found"))?;
    // The comment's replies ride along so a resolve does not read as their
    // disappearance in the response.
    attach_replies(&mut *tx, std::slice::from_mut(&mut row)).await?;
    tx.commit().await?;
    Ok(row)
}

/// Append one entry to a comment's reply thread on a submitted review.
/// Replies never mutate the comment they answer and never touch the draft
/// revision — that guard tracks operator edits, and a thread append has
/// nothing to race. A session author must name the review's own session: it
/// answers feedback on its work, not another session's.
pub async fn reply_comment(
    db: &Db,
    comment_id: i64,
    author: ReplyAuthor<'_>,
    body: &str,
) -> Result<ReviewComment> {
    let body = body.trim();
    if body.is_empty() {
        bail!("reply body must not be empty");
    }
    if body.len() > MAX_COMMENT_BYTES {
        bail!("reply exceeds the 8 KiB comment limit");
    }
    let mut tx = crate::db::begin_immediate(db).await?;
    let review_id: Option<i64> =
        sqlx::query_scalar("SELECT review_id FROM review_comments WHERE id = ?")
            .bind(comment_id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
    let Some(review_id) = review_id else {
        bail!("review comment not found");
    };
    let (status, review_session): (String, String) =
        sqlx::query_as("SELECT status, session_id FROM reviews WHERE id = ?")
            .bind(review_id)
            .fetch_one(&mut *tx)
            .await?;
    if status != "submitted" {
        bail!("replies attach to submitted reviews only; this one is {status}");
    }
    if let ReplyAuthor::Session(session_id) = author {
        if review_session != session_id {
            bail!("only the reviewed session may reply to this review");
        }
    }
    let replies: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM review_comment_replies WHERE comment_id = ?")
            .bind(comment_id)
            .fetch_one(&mut *tx)
            .await?;
    if replies >= MAX_REPLIES_PER_COMMENT {
        bail!("this comment already holds its reply cap");
    }
    // The review-wide reply budget: every thread on every comment shares the
    // same bound the review's own comments obey, so a looping conversation
    // degrades instead of growing without limit.
    let reply_total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(length(CAST(body AS BLOB))), 0)
         FROM review_comment_replies
         WHERE comment_id IN (SELECT id FROM review_comments WHERE review_id = ?)",
    )
    .bind(review_id)
    .fetch_one(&mut *tx)
    .await?;
    if reply_total + body.len() as i64 > MAX_REVIEW_BYTES {
        bail!("this review's reply threads exceed the 256 KiB total limit");
    }
    sqlx::query(
        "INSERT INTO review_comment_replies (comment_id, author_kind, author, body) VALUES (?, ?, ?, ?)",
    )
    .bind(comment_id)
    .bind(author.kind())
    .bind(author.name())
    .bind(body)
    .execute(&mut *tx)
    .await?;
    // A reply is a conversation continuing — it reopens a thread someone
    // closed, whoever answers. The resolve bit stays with `resolve`; it just
    // yields to new words.
    sqlx::query(
        "UPDATE review_comments SET status = 'submitted', updated_at = ?
         WHERE id = ? AND status = 'resolved'",
    )
    .bind(now_iso())
    .bind(comment_id)
    .execute(&mut *tx)
    .await?;
    let mut comment = sqlx::query_as::<_, ReviewComment>(&format!("{SELECT_COMMENT} WHERE id = ?"))
        .bind(comment_id)
        .fetch_one(&mut *tx)
        .await?;
    attach_replies(&mut *tx, std::slice::from_mut(&mut comment)).await?;
    tx.commit().await?;
    Ok(comment)
}

/// One author's own words, checked against the reply row: the speaker who
/// wrote a reply may rewrite it, and nobody else may — not another operator,
/// not the reviewed session.
async fn require_reply_authorship(
    tx: &mut crate::db::DbTransaction<'_>,
    reply_id: i64,
    author: ReplyAuthor<'_>,
) -> Result<(i64, i64)> {
    let (comment_id, review_id): (i64, i64) = sqlx::query_as(
        "SELECT comment_id, (SELECT review_id FROM review_comments WHERE id = review_comment_replies.comment_id)
         FROM review_comment_replies WHERE id = ?",
    )
    .bind(reply_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| anyhow!("review reply not found"))?;
    let (status, review_session, reply_upto): (String, String, i64) =
        sqlx::query_as("SELECT status, session_id, reply_upto FROM reviews WHERE id = ?")
            .bind(review_id)
            .fetch_one(&mut **tx)
            .await?;
    if status != "submitted" {
        bail!("reply threads attach to submitted reviews only; this one is {status}");
    }
    let (author_kind, author_name): (String, String) =
        sqlx::query_as("SELECT author_kind, author FROM review_comment_replies WHERE id = ?")
            .bind(reply_id)
            .fetch_one(&mut **tx)
            .await?;
    match author {
        ReplyAuthor::Operator(name) if author_kind == "operator" && author_name == name => {}
        ReplyAuthor::Session(session_id)
            if author_kind == "session" && review_session == session_id => {}
        _ => bail!("only the reply's own author may rewrite it"),
    }
    // Delivered words are history: the reviewed session has already read an
    // operator reply at or below the cursor, so it freezes exactly like
    // submitted comment content. A session's own replies are not
    // delivery-tracked — the reviewer reads them live — so they stay the
    // session's to revise.
    if author_kind == "operator" && reply_id <= reply_upto {
        bail!("this reply already reached the session; delivered words are history");
    }
    Ok((comment_id, review_id))
}

/// Rewrite one reply's body in place. Only the reply's own author, and only
/// words the reviewed session has not received yet. The comment a reply
/// answers never changes.
pub async fn update_reply(
    db: &Db,
    reply_id: i64,
    author: ReplyAuthor<'_>,
    body: &str,
) -> Result<ReviewComment> {
    let body = body.trim();
    if body.is_empty() {
        bail!("reply body must not be empty");
    }
    if body.len() > MAX_COMMENT_BYTES {
        bail!("reply exceeds the 8 KiB comment limit");
    }
    let mut tx = crate::db::begin_immediate(db).await?;
    let (comment_id, _) = require_reply_authorship(&mut tx, reply_id, author).await?;
    sqlx::query("UPDATE review_comment_replies SET body = ? WHERE id = ?")
        .bind(body)
        .bind(reply_id)
        .execute(&mut *tx)
        .await?;
    let mut comment = sqlx::query_as::<_, ReviewComment>(&format!("{SELECT_COMMENT} WHERE id = ?"))
        .bind(comment_id)
        .fetch_one(&mut *tx)
        .await?;
    attach_replies(&mut *tx, std::slice::from_mut(&mut comment)).await?;
    tx.commit().await?;
    Ok(comment)
}

/// Remove one reply from its thread. The same reach `update_reply` grants: the
/// reply's own author, before the reviewed session has received it.
pub async fn delete_reply(
    db: &Db,
    reply_id: i64,
    author: ReplyAuthor<'_>,
) -> Result<ReviewComment> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let (comment_id, _) = require_reply_authorship(&mut tx, reply_id, author).await?;
    sqlx::query("DELETE FROM review_comment_replies WHERE id = ?")
        .bind(reply_id)
        .execute(&mut *tx)
        .await?;
    let mut comment = sqlx::query_as::<_, ReviewComment>(&format!("{SELECT_COMMENT} WHERE id = ?"))
        .bind(comment_id)
        .fetch_one(&mut *tx)
        .await?;
    attach_replies(&mut *tx, std::slice::from_mut(&mut comment)).await?;
    tx.commit().await?;
    Ok(comment)
}

pub async fn discard(
    db: &Db,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
) -> Result<bool> {
    let mut tx = crate::db::begin_immediate(db).await?;
    require_creator_draft(&mut tx, review_id, creator, expected_revision).await?;
    sqlx::query("DELETE FROM review_comments WHERE review_id = ?")
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    let removed = sqlx::query("DELETE FROM reviews WHERE id = ? AND status = 'draft'")
        .bind(review_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        == 1;
    tx.commit().await?;
    Ok(removed)
}

pub async fn submit(
    db: &Db,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
) -> Result<Submission> {
    submit_inner(db, review_id, creator, expected_revision).await
}

async fn submit_inner(
    db: &Db,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
) -> Result<Submission> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let mut review =
        sqlx::query_as::<_, Review>(&format!("{SELECT_REVIEW} WHERE id = ? AND created_by = ?"))
            .bind(review_id)
            .bind(creator)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow!("review not found"))?;
    let mut comments = comments_for(&mut *tx, review_id).await?;

    if review.draft_revision != expected_revision {
        return Err(anyhow!(DraftRevisionConflict {
            expected: expected_revision,
            actual: review.draft_revision,
        }));
    }
    // A submitted review stays open to its reviewer. New comments queue on
    // it exactly like draft comments queue on a draft, thread replies queue
    // past the delivered cursor, and submitting again sends them together as
    // one follow-up round — the same delivery machinery the initial comments
    // rode. A resubmit with nothing new is the read it always was.
    if review.status == "submitted" {
        let reply_cutoff = max_operator_reply_id(&mut *tx, review_id).await?;
        let pending_comments = comments.iter().any(|comment| comment.status == "pending");
        let batch: Option<OutboxItem> = sqlx::query_as::<_, OutboxItem>(
            "SELECT review_id, delivery_key, state, attempts, next_attempt_at, last_error, \
             lease_token, summary FROM review_delivery_outbox WHERE review_id = ?",
        )
        .bind(review_id)
        .fetch_optional(&mut *tx)
        .await?;
        // A note the live batch already carries is not pending words — it
        // rides that batch. Once delivery lands, the batch snapshot becomes
        // the review's delivered_summary and the comparison stays truthful.
        let pending_note = match &batch {
            Some(item) if item.state != "delivered" => review.summary != item.summary,
            _ => review.summary != review.delivered_summary,
        };
        if pending_comments || pending_note || reply_cutoff > review.reply_upto {
            let event =
                queue_follow_up_round(&mut tx, &mut review, &mut comments, reply_cutoff).await?;
            tx.commit().await?;
            let review = get(db, review_id)
                .await?
                .ok_or_else(|| anyhow!("submitted review vanished"))?;
            return Ok(Submission {
                review,
                event: Some(event),
            });
        }
        // Replies ride along like every other read path.
        attach_replies(&mut *tx, &mut comments).await?;
        review.comments = comments;
        tx.commit().await?;
        return Ok(Submission {
            review,
            event: None,
        });
    }
    if review.status != "draft" {
        bail!("review is not editable");
    }
    if comments.is_empty() && review.summary.trim().is_empty() {
        bail!("add a comment or overall note before submitting");
    }
    // The captured revision is the review's provenance, not a gate: staleness
    // is a property of what a comment anchors at, decided where the anchor
    // renders, and the delivered message quotes each comment's own revision.
    let now = now_iso();
    sqlx::query(
        "UPDATE reviews
         SET status = 'submitted',
             delivery_state = 'queued', delivery_error = NULL, submitted_at = ?, updated_at = ?
         WHERE id = ? AND status = 'draft' AND draft_revision = ?",
    )
    .bind(&now)
    .bind(&now)
    .bind(review_id)
    .bind(expected_revision)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE review_comments SET status = 'submitted' WHERE review_id = ?")
        .bind(review_id)
        .execute(&mut *tx)
        .await?;

    let comment_payload: Vec<Value> = comments
        .iter()
        .map(|comment| {
            json!({
                "id": comment.id,
                "revision": comment.subject_version,
                "anchor_kind": comment.anchor_kind,
                "anchor": comment.anchor().value(),
                "body": comment.body,
            })
        })
        .collect();
    review.comments = comments.clone();
    let message = structured_message(&review);
    let event_data = json!({
        "review_id": review_id,
        "delivery_key": review.delivery_key,
        "created_by": creator,
        "session_id": review.session_id,
        "subject": {
            "kind": review.subject_kind,
            "id": review.subject_id,
            "key": review.subject_key,
            "label": review.subject_label,
            "revision": review.subject_version,
        },
        "summary": review.summary,
        "comments": comment_payload,
        "message": message,
    });
    let event_row = sqlx::query(
        "INSERT INTO events (branch_id, kind, data)
         VALUES (?, 'review_submitted', ?) RETURNING id, created_at",
    )
    .bind(&review.branch_id)
    .bind(event_data.to_string())
    .fetch_one(&mut *tx)
    .await?;
    let event = Event {
        id: event_row.get("id"),
        branch_id: review.branch_id.clone(),
        kind: "review_submitted".to_string(),
        data: event_data,
        created_at: event_row.get("created_at"),
    };
    let comment_cutoff = comments.iter().map(|comment| comment.id).max().unwrap_or(0);
    sqlx::query(
        "INSERT INTO review_delivery_outbox
            (review_id, delivery_key, state, attempts, next_attempt_at, comment_upto, summary,
             payload)
         VALUES (?, ?, 'queued', 0, ?, ?, ?, ?)
         ON CONFLICT(review_id) DO NOTHING",
    )
    .bind(review_id)
    .bind(&review.delivery_key)
    .bind(&now)
    .bind(comment_cutoff)
    .bind(&review.summary)
    // The payload freezes exactly as the event's message: delivery re-sends
    // byte-identical text, and the words the session receives are the words
    // the delivered cursors record.
    .bind(&message)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let review = get(db, review_id)
        .await?
        .ok_or_else(|| anyhow!("submitted review vanished"))?;
    Ok(Submission {
        review,
        event: Some(event),
    })
}

/// Queue one follow-up round on a submitted review: everything not yet
/// delivered — new comments and the reviewer's thread replies alike — goes
/// back to the session through the same outbox and inbox the initial
/// submission rode, under a fresh round key.
///
/// The batch is the single live one for the review: the outbox row is
/// overwritten in place, its payload frozen verbatim at submit time, so a
/// retry re-sends byte-identical text and a superseded round's key never
/// inserts into the once-only inbox. The delivered cursors
/// (`comment_upto`/`reply_upto`) do **not** move here — they advance when
/// delivery succeeds, which is what makes a failed or in-flight round
/// self-healing: the next round's batch recomputes everything past the
/// delivered point instead of stranding it.
async fn queue_follow_up_round(
    tx: &mut crate::db::DbTransaction<'_>,
    review: &mut Review,
    comments: &mut [ReviewComment],
    reply_cutoff: i64,
) -> Result<Event> {
    let comment_from = review.comment_upto;
    let reply_from = review.reply_upto;
    let round = review.reply_round + 1;
    let now = now_iso();
    // The original key never carried a '#': every round appends its own.
    let base = review.delivery_key.split('#').next().unwrap_or_default();
    let key = format!("{base}#{round}");
    // Pending comments join the delivered set — the round owns them now.
    sqlx::query("UPDATE review_comments SET status = 'submitted' WHERE review_id = ? AND status = 'pending'")
        .bind(review.id)
        .execute(&mut **tx)
        .await?;
    attach_replies(&mut **tx, comments).await?;
    review.reply_round = round;
    review.delivery_key = key.clone();
    review.comments = comments.to_owned();
    let comment_cutoff = comments
        .iter()
        .map(|comment| comment.id)
        .max()
        .unwrap_or(comment_from);
    let message = follow_up_message(review, comment_from, reply_from, reply_cutoff);
    let updated = sqlx::query(
        "UPDATE reviews
         SET reply_round = ?, delivery_key = ?,
             delivery_state = 'queued', delivery_error = NULL, updated_at = ?
         WHERE id = ? AND status = 'submitted' AND draft_revision = ?",
    )
    .bind(round)
    .bind(&key)
    .bind(&now)
    .bind(review.id)
    .bind(review.draft_revision)
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        bail!("review is not editable");
    }
    sqlx::query(
        "INSERT INTO review_delivery_outbox
            (review_id, delivery_key, state, attempts, next_attempt_at,
             reply_from, reply_upto, comment_upto, summary, payload)
         VALUES (?, ?, 'queued', 0, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(review_id) DO UPDATE SET
            delivery_key = excluded.delivery_key, state = 'queued', attempts = 0,
            next_attempt_at = excluded.next_attempt_at, last_error = NULL,
            lease_token = NULL, reply_from = excluded.reply_from,
            reply_upto = excluded.reply_upto, comment_upto = excluded.comment_upto,
            summary = excluded.summary, payload = excluded.payload",
    )
    .bind(review.id)
    .bind(&key)
    .bind(&now)
    .bind(reply_from)
    .bind(reply_cutoff)
    .bind(comment_cutoff)
    .bind(&review.summary)
    .bind(&message)
    .execute(&mut **tx)
    .await?;
    let comment_payload: Vec<Value> = comments
        .iter()
        .map(|comment| {
            json!({
                "id": comment.id,
                "revision": comment.subject_version,
                "anchor_kind": comment.anchor_kind,
                "anchor": comment.anchor().value(),
                "body": comment.body,
            })
        })
        .collect();
    let event_data = json!({
        "review_id": review.id,
        "delivery_key": key,
        "created_by": review.created_by,
        "session_id": review.session_id,
        "subject": {
            "kind": review.subject_kind,
            "id": review.subject_id,
            "key": review.subject_key,
            "label": review.subject_label,
            "revision": review.subject_version,
            "current_revision": review.subject_version,
        },
        "round": round,
        "summary": review.summary,
        "comments": comment_payload,
        "message": message,
    });
    let event_row = sqlx::query(
        "INSERT INTO events (branch_id, kind, data)
         VALUES (?, 'review_submitted', ?) RETURNING id, created_at",
    )
    .bind(&review.branch_id)
    .bind(event_data.to_string())
    .fetch_one(&mut **tx)
    .await?;
    Ok(Event {
        id: event_row.get("id"),
        branch_id: review.branch_id.clone(),
        kind: "review_submitted".to_string(),
        data: event_data,
        created_at: event_row.get("created_at"),
    })
}

/// The message a follow-up round carries: the reviewer's new words, both
/// kinds alike — new comments as first-class numbered entries with one-line
/// locations, and their replies in the threads they answer, verbatim. The
/// anchors stay in the durable review, reachable through `review_list`.
pub fn follow_up_message(
    review: &Review,
    comment_from: i64,
    reply_from: i64,
    reply_cutoff: i64,
) -> String {
    let subject = if review.subject_kind == "changes" {
        format!(
            "The reviewer sent follow-up feedback on changes version `{}`.",
            review.subject_version
        )
    } else {
        format!(
            "The reviewer sent follow-up feedback on artifact `{}`, revision {}.",
            review.subject_label, review.subject_version
        )
    };
    let mut message = subject;
    if review.summary != review.delivered_summary {
        // A cleared note is still news: the round says so, and its delivery
        // advances the delivered cursor past the words it replaced. An
        // empty round would strand the cursor and re-queue forever.
        if review.summary.is_empty() {
            message.push_str("\n\nThe reviewer cleared the overall note.");
        } else {
            message.push_str(&format!("\n\nOverall:\n{}", review.summary));
        }
    }
    let fresh_comments: Vec<&ReviewComment> = review
        .comments
        .iter()
        .filter(|comment| comment.id > comment_from)
        .collect();
    for (index, comment) in fresh_comments.iter().enumerate() {
        message.push_str(&format!(
            "\n\n{}. {} (revision {}, comment id {}):\n{}",
            index + 1,
            comment_location(comment),
            comment.subject_version,
            comment.id,
            comment.body
        ));
    }
    for comment in review.comments.iter() {
        let fresh: Vec<&ReviewCommentReply> = comment
            .replies
            .iter()
            .filter(|reply| {
                reply.author_kind == "operator" && reply.id > reply_from && reply.id <= reply_cutoff
            })
            .collect();
        if fresh.is_empty() {
            continue;
        }
        message.push_str(&format!(
            "\n\n{} (comment id {}):\n",
            comment_location(comment),
            comment.id
        ));
        for reply in fresh {
            message.push_str(&format!("The reviewer replied:\n{}\n", reply.body));
        }
    }
    message.push_str(&format!(
        "\n\n[review_id: {}; delivery_key: {}]",
        review.id, review.delivery_key
    ));
    message.push_str(
        "\n\nTake action on any comment that calls for a change, and continue the threads \
         with the `review_comment_reply` tool, passing each comment id and your response \
         or what you did. Each comment's full anchor and surrounding context is served \
         by the `review_list` tool. Resolving a comment is the reviewer's action, \
         not yours.",
    );
    message
}

/// The message a submission would carry for the review as it stands — the
/// tray's preview. A draft previews its initial payload; an envelope with
/// words still on the reviewer's side previews the next follow-up round; a
/// quiescent envelope previews its standing message. The pending-note rule
/// matches the submit path's: words a live batch already carries are not
/// pending.
pub fn preview_message(review: &Review) -> String {
    if review.status != "submitted" {
        return structured_message(review);
    }
    let pending_comments = review
        .comments
        .iter()
        .any(|comment| comment.status == "pending");
    let pending_note = match &review.queued_summary {
        Some(queued) => review.summary != *queued,
        None => review.summary != review.delivered_summary,
    };
    if pending_comments || pending_note || review.pending_replies > 0 {
        // Every operator reply past the delivered cursor is pending, so an
        // open window picks exactly those up.
        return follow_up_message(review, review.comment_upto, review.reply_upto, i64::MAX);
    }
    structured_message(review)
}

pub async fn ready_outbox(db: &Db, limit: i64) -> Result<Vec<OutboxItem>> {
    Ok(sqlx::query_as::<_, OutboxItem>(
        "SELECT review_id, delivery_key, state, attempts, next_attempt_at, last_error,
                lease_token, summary
         FROM review_delivery_outbox
         WHERE state IN ('queued', 'retrying', 'delivering') AND next_attempt_at <= ?
         ORDER BY next_attempt_at, review_id LIMIT ?",
    )
    .bind(now_iso())
    .bind(limit)
    .fetch_all(db)
    .await?)
}

/// Claim one due delivery for a short lease. The lease prevents the request
/// path and background sweep from sending the same terminal payload
/// concurrently, while allowing recovery if the process exits mid-delivery.
pub async fn claim_delivery(db: &Db, review_id: i64) -> Result<Option<DeliveryLease>> {
    let now = now_iso();
    let lease_until = chrono::Utc::now()
        .checked_add_signed(chrono::TimeDelta::minutes(1))
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(now_iso);
    let mut tx = crate::db::begin_immediate(db).await?;
    let claimed = sqlx::query_as::<_, DeliveryLease>(
        "UPDATE review_delivery_outbox
         SET state = 'delivering', next_attempt_at = ?,
             lease_token = lower(hex(randomblob(16)))
         WHERE review_id = ? AND next_attempt_at <= ?
           AND state IN ('queued', 'retrying', 'delivering')
         RETURNING lease_token AS token, reply_from, reply_upto, comment_upto, payload",
    )
    .bind(lease_until)
    .bind(review_id)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    if claimed.is_some() {
        sqlx::query(
            "UPDATE reviews
             SET delivery_state = 'delivering', delivery_error = NULL
             WHERE id = ?",
        )
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(claimed)
}

/// Return an expired ownerless lease to the honest queued state without
/// counting an attempt. This is used when no live conversation transport
/// exists after the lease deadline.
pub async fn release_expired_delivery(db: &Db, review_id: i64) -> Result<bool> {
    let now = now_iso();
    let mut tx = crate::db::begin_immediate(db).await?;
    let released = sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'queued', next_attempt_at = ?, lease_token = NULL
         WHERE review_id = ? AND state = 'delivering' AND next_attempt_at <= ?",
    )
    .bind(&now)
    .bind(review_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    if released.rows_affected() == 1 {
        sqlx::query(
            "UPDATE reviews
             SET delivery_state = 'queued', delivery_error = NULL
             WHERE id = ? AND delivery_state = 'delivering'",
        )
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(released.rows_affected() == 1)
}

pub async fn mark_retry(
    db: &Db,
    review_id: i64,
    lease_token: &str,
    error: &str,
) -> Result<Option<String>> {
    let next = chrono::Utc::now()
        .checked_add_signed(chrono::TimeDelta::minutes(1))
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(now_iso);
    let mut tx = crate::db::begin_immediate(db).await?;
    let attempts = sqlx::query_scalar::<_, i64>(
        "SELECT attempts FROM review_delivery_outbox
         WHERE review_id = ? AND state = 'delivering' AND lease_token = ?",
    )
    .bind(review_id)
    .bind(lease_token)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(attempts) = attempts.map(|attempts| attempts + 1) else {
        tx.commit().await?;
        return Ok(None);
    };
    let state = if attempts >= 3 { "failed" } else { "retrying" };
    sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = ?, attempts = ?, next_attempt_at = ?, last_error = ?, lease_token = NULL
         WHERE review_id = ? AND state = 'delivering' AND lease_token = ?",
    )
    .bind(state)
    .bind(attempts)
    .bind(next)
    .bind(error)
    .bind(review_id)
    .bind(lease_token)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE reviews SET delivery_state = ?, delivery_error = ?
         WHERE id = ? AND delivery_state = 'delivering'",
    )
    .bind(state)
    .bind(error)
    .bind(review_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(state.to_string()))
}

pub async fn mark_delivered(db: &Db, review_id: i64, lease_token: &str) -> Result<bool> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let updated = sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'delivered', attempts = attempts + 1, last_error = NULL, lease_token = NULL
         WHERE review_id = ? AND state = 'delivering' AND lease_token = ?",
    )
    .bind(review_id)
    .bind(lease_token)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 1 {
        // The delivered cursors move here, never at submit: these words have
        // now reached the session, so the next round's batch is exactly what
        // arrives after them.
        let (comment_upto, reply_upto, delivered_summary): (i64, i64, String) = sqlx::query_as(
            "SELECT comment_upto, reply_upto, summary FROM review_delivery_outbox WHERE review_id = ?",
        )
        .bind(review_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE reviews SET delivery_state = 'delivered', delivery_error = NULL,
                 comment_upto = ?, reply_upto = ?, delivered_summary = ?
             WHERE id = ? AND delivery_state = 'delivering'",
        )
        .bind(comment_upto)
        .bind(reply_upto)
        .bind(delivered_summary)
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(updated.rows_affected() == 1)
}

pub async fn retry_delivery(db: &Db, review_id: i64) -> Result<()> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1
           FROM reviews r
           JOIN review_delivery_outbox o ON o.review_id = r.id
           WHERE r.id = ? AND r.status = 'submitted'
             AND r.delivery_state = 'failed' AND o.state = 'failed'
         )",
    )
    .bind(review_id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        bail!("failed review delivery not found");
    }
    sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'queued', next_attempt_at = ?, last_error = NULL, lease_token = NULL
         WHERE review_id = ?",
    )
    .bind(now_iso())
    .bind(review_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE reviews SET delivery_state = 'queued', delivery_error = NULL WHERE id = ?")
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// The prompt pasted into the reviewed session's conversation.
///
/// Deliberately compact: each comment contributes only its one-line location
/// — with its id, so the reply tool is callable without a lookup — not its
/// full anchor. The durable review — complete anchors and context excerpts
/// included — stays reachable through `reviews.list`, which the reviewed
/// session's own credential may call. The closing line drives the loop:
/// act on what is actionable, then answer everything. Resolving stays with
/// the reviewer.
pub fn structured_message(review: &Review) -> String {
    let subject = if review.subject_kind == "changes" {
        format!(
            "The user submitted feedback on changes version `{}`.",
            review.subject_version
        )
    } else {
        format!(
            "The user submitted feedback on artifact `{}`, revision {}.",
            review.subject_label, review.subject_version
        )
    };
    let mut message = subject;
    if !review.summary.trim().is_empty() {
        message.push_str("\n\nOverall:\n");
        message.push_str(review.summary.trim());
    }
    for (index, comment) in review.comments.iter().enumerate() {
        message.push_str(&format!(
            "\n\n{}. {} (revision {}, comment id {}):\n",
            index + 1,
            comment_location(comment),
            comment.subject_version,
            comment.id
        ));
        message.push_str(&comment.body);
    }
    message.push_str(&format!(
        "\n\n[review_id: {}; delivery_key: {}]",
        review.id, review.delivery_key
    ));
    if !review.comments.is_empty() {
        message.push_str(
            "\n\nTake action on any comment that calls for a change, then \
             respond to every comment with the `review_comment_reply` tool, \
             passing its comment id and what you did or why you left it. Each \
             comment's full anchor and surrounding context is served by the \
             `review_list` tool. Resolving a comment is the reviewer's \
             action, not yours.",
        );
    }
    message
}

/// A comment's one-line location: just enough for the reviewed agent to find
/// the spot in its own checkout. The anchor's context excerpts stay in the
/// durable review and out of the pasted prompt.
fn comment_location(comment: &ReviewComment) -> String {
    match comment.anchor() {
        ReviewAnchor::Text(anchor) => {
            let block = anchor
                .block_index
                .map(|index| format!(", block {index}"))
                .unwrap_or_default();
            format!("\"{}\"{block}", anchor.quote)
        }
        ReviewAnchor::Change(anchor) => format!(
            "{} · {} lines {}-{}",
            anchor.path_display, anchor.side, anchor.start_line, anchor.end_line
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{self, NewRevision};
    use crate::branch;
    use std::sync::Arc;
    use tokio::sync::Barrier;

    async fn seeded() -> (Db, crate::artifact::Artifact) {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch::upsert(&db, "/repo", "weaver/review", "main")
            .await
            .unwrap();
        let artifact = artifact::write(
            &db,
            &NewRevision {
                repo_root: "/repo",
                branch_id: Some(&branch.id),
                name: "design",
                kind: "markdown",
                title: "Design",
                content: "Alpha beta gamma",
                author: "agent",
            },
        )
        .await
        .unwrap();
        (db, artifact)
    }

    async fn draft(db: &Db, artifact: &crate::artifact::Artifact, user: &str) -> Review {
        get_or_create(
            db,
            &NewReview {
                repo_root: &artifact.repo_root,
                branch_id: artifact.branch_id.as_deref().unwrap(),
                session_id: "session-1",
                subject_kind: "artifact",
                subject_id: &artifact.id.to_string(),
                subject_key: &artifact.name,
                subject_label: &artifact.name,
                subject_version: "1",
                created_by: user,
            },
        )
        .await
        .unwrap()
    }

    fn anchor(quote: &str) -> ReviewAnchor {
        ArtifactTextAnchor {
            quote: quote.to_string(),
            prefix: "before".to_string(),
            suffix: "after".to_string(),
            block_index: Some(0),
        }
        .into()
    }

    async fn add(db: &Db, draft: &Review, body: &str) -> Review {
        let anchor = anchor("beta");
        add_comment(
            db,
            draft.id,
            "alice",
            draft.draft_revision,
            &NewComment {
                subject_version: "1",
                anchor: &anchor,
                body,
            },
        )
        .await
        .unwrap()
    }

    fn review_fixture() -> Review {
        Review {
            id: 7,
            repo_root: "/repo".to_string(),
            branch_id: "branch".to_string(),
            session_id: "session-1".to_string(),
            subject_kind: "artifact".to_string(),
            subject_id: "1".to_string(),
            subject_key: "design".to_string(),
            subject_label: "design".to_string(),
            subject_version: "1".to_string(),
            status: "submitted".to_string(),
            summary: String::new(),
            delivered_summary: String::new(),
            draft_revision: 1,
            created_by: "alice".to_string(),
            delivery_state: "delivered".to_string(),
            delivery_error: None,
            delivery_key: "key-7".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            submitted_at: None,
            reply_round: 0,
            reply_upto: 0,
            comment_upto: 0,
            pending_replies: 0,
            queued_summary: None,
            comments: Vec::new(),
        }
    }

    fn comment<'a>(body: &'a str, anchor: &'a ReviewAnchor) -> NewComment<'a> {
        NewComment {
            subject_version: "1",
            anchor,
            body,
        }
    }

    #[tokio::test]
    async fn drafts_are_durable_and_creator_isolated() {
        let (db, artifact) = seeded().await;
        let first = draft(&db, &artifact, "alice").await;
        let updated = add(&db, &first, "change this").await;
        let updated = update_draft(
            &db,
            updated.id,
            "alice",
            updated.draft_revision,
            &DraftPatch {
                summary: Some("Durable overall note"),
            },
        )
        .await
        .unwrap();
        let reloaded = draft(&db, &artifact, "alice").await;
        assert_eq!(reloaded.id, first.id);
        assert_eq!(reloaded.comments.len(), 1);
        assert_eq!(reloaded.summary, "Durable overall note");
        assert_eq!(reloaded.draft_revision, updated.draft_revision);
        assert!(
            get_visible(&db, first.id, "bob").await.unwrap().is_none(),
            "another operator cannot read a draft"
        );
        assert_eq!(
            list_visible(
                &db,
                artifact.branch_id.as_deref().unwrap(),
                "session-1",
                "artifact",
                &artifact.id.to_string(),
                "bob",
            )
            .await
            .unwrap()
            .len(),
            0
        );
    }

    #[tokio::test]
    async fn recreated_artifact_does_not_recover_reviews_from_old_identity() {
        let (db, artifact) = seeded().await;
        let old_draft = draft(&db, &artifact, "alice").await;
        artifact::delete(&db, artifact.id).await.unwrap();
        let replacement = artifact::write(
            &db,
            &NewRevision {
                repo_root: "/repo",
                branch_id: artifact.branch_id.as_deref(),
                name: "design",
                kind: "markdown",
                title: "Replacement",
                content: "Unrelated replacement content",
                author: "agent",
            },
        )
        .await
        .unwrap();
        assert_ne!(replacement.id, artifact.id);

        let replacement_draft = draft(&db, &replacement, "alice").await;
        assert_ne!(replacement_draft.id, old_draft.id);
        let visible = list_visible(
            &db,
            replacement.branch_id.as_deref().unwrap(),
            "session-1",
            "artifact",
            &replacement.id.to_string(),
            "alice",
        )
        .await
        .unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, replacement_draft.id);
        assert_eq!(visible[0].subject_id, replacement.id.to_string());
    }

    #[tokio::test]
    async fn replies_append_after_submit_and_reject_everyone_else() {
        let (db, artifact) = seeded().await;
        let draft = draft(&db, &artifact, "alice").await;
        let draft = add(&db, &draft, "change this").await;
        let comment_id = draft.comments[0].id;

        // A draft is undelivered; there is nothing to answer yet.
        assert!(
            reply_comment(&db, comment_id, ReplyAuthor::Session("session-1"), "on it")
                .await
                .is_err()
        );

        let submitted = submit(&db, draft.id, "alice", draft.draft_revision)
            .await
            .unwrap();

        // Only the reviewed session may answer as the session.
        assert!(reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Session("someone-else"),
            "not mine"
        )
        .await
        .is_err());
        let comment = reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Session("session-1"),
            "done in 4f2c",
        )
        .await
        .unwrap();
        assert_eq!(comment.replies.len(), 1);
        assert_eq!(comment.replies[0].author_kind, "session");
        assert_eq!(comment.replies[0].author, "");

        // An operator joins the same thread — any operator, any submitted
        // review, the reach `set_comment_resolved` grants.
        let comment = reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Operator("alice"),
            "thank you — closing after checks",
        )
        .await
        .unwrap();
        assert_eq!(comment.replies.len(), 2);
        assert_eq!(comment.replies[1].author_kind, "operator");
        assert_eq!(comment.replies[1].author, "alice");

        // The conversation reads oldest-first on the durable review.
        let comment = reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Session("session-1"),
            "and a follow-up",
        )
        .await
        .unwrap();
        assert_eq!(comment.replies.len(), 3);
        assert_eq!(comment.replies[0].body, "done in 4f2c");
        assert_eq!(comment.replies[2].body, "and a follow-up");
        let listed = get(&db, submitted.review.id).await.unwrap().unwrap();
        assert_eq!(listed.comments[0].replies.len(), 3);

        // An empty reply never lands.
        assert!(
            reply_comment(&db, comment_id, ReplyAuthor::Session("session-1"), "   ")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_reviews_reply_threads_share_one_byte_budget() {
        let (db, artifact) = seeded().await;
        let draft = draft(&db, &artifact, "alice").await;
        let draft = add(&db, &draft, "change this").await;
        let comment_id = draft.comments[0].id;
        let _ = submit(&db, draft.id, "alice", draft.draft_revision)
            .await
            .unwrap();

        // Fill the review-wide reply budget: 8 KiB bodies until the 256 KiB
        // bound is crossed. Every append lands inside the same transaction
        // that checks the bound, so the last one over is the one that fails.
        let body = "x".repeat(MAX_COMMENT_BYTES);
        let mut appended = 0;
        loop {
            let result =
                reply_comment(&db, comment_id, ReplyAuthor::Operator("alice"), &body).await;
            appended += 1;
            if result.is_err() {
                break;
            }
        }
        assert!(appended > 1, "the budget spans more than one reply");
        assert!(appended <= MAX_REVIEW_BYTES / MAX_COMMENT_BYTES as i64 + 2);
        // A small reply still fits nowhere once the budget is spent.
        assert!(
            reply_comment(&db, comment_id, ReplyAuthor::Operator("alice"), "tiny")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_changes_draft_rejects_comments_from_another_snapshot_spelling() {
        let (db, artifact) = seeded().await;
        // A commit-pinned changes draft.
        let pinned_version = "changes-commit-v1:0123abcd:fededigest";
        let review = get_or_create(
            &db,
            &NewReview {
                repo_root: &artifact.repo_root,
                branch_id: artifact.branch_id.as_deref().unwrap(),
                session_id: "session-1",
                subject_kind: "changes",
                subject_id: "branch",
                subject_key: "changes",
                subject_label: "changes",
                subject_version: pinned_version,
                created_by: "alice",
            },
        )
        .await
        .unwrap();

        // A comment naming the branch spelling cannot ride the pinned
        // envelope, and neither can one pinning a different commit or the
        // unstaged working state.
        for version in [
            "changes-v1:digest",
            "changes-commit-v1:other:digest",
            "changes-unstaged-v1:digest",
        ] {
            let err = add_comment(
                &db,
                review.id,
                "alice",
                review.draft_revision,
                &NewComment {
                    subject_version: version,
                    anchor: &anchor("beta"),
                    body: "wrong scope",
                },
            )
            .await
            .unwrap_err();
            assert!(err.to_string().contains("another snapshot"), "{err}");
        }

        // The pinned spelling itself is accepted.
        let review = add_comment(
            &db,
            review.id,
            "alice",
            review.draft_revision,
            &NewComment {
                subject_version: pinned_version,
                anchor: &anchor("beta"),
                body: "right scope",
            },
        )
        .await
        .unwrap();
        // And a re-anchor cannot carry its comment across spellings either.
        let err = patch_comment(
            &db,
            review.id,
            review.comments[0].id,
            "alice",
            review.draft_revision,
            &CommentPatch {
                subject_version: Some("changes-v1:digest"),
                anchor: Some(&anchor("gamma")),
                body: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("its own snapshot"), "{err}");
    }

    #[test]
    fn the_scope_key_separates_all_three_snapshot_spellings() {
        use super::changes_scope;
        assert_eq!(
            changes_scope("changes-commit-v1:0123abcd:fededigest"),
            "commit:0123abcd"
        );
        assert_eq!(changes_scope("changes-unstaged-v1:fededigest"), "unstaged");
        assert_eq!(changes_scope("changes-v1:fededigest"), "branch");
        // The unstaged key is per-spelling, not per-version: a live
        // snapshot's digest may move within its scope freely.
        assert_eq!(
            changes_scope("changes-unstaged-v1:otherdigest"),
            changes_scope("changes-unstaged-v1:fededigest")
        );
    }

    #[tokio::test]
    async fn an_unstaged_scoped_draft_rejects_comments_from_the_other_spellings() {
        let (db, artifact) = seeded().await;
        let unstaged_version = "changes-unstaged-v1:fededigest";
        let review = get_or_create(
            &db,
            &NewReview {
                repo_root: &artifact.repo_root,
                branch_id: artifact.branch_id.as_deref().unwrap(),
                session_id: "session-1",
                subject_kind: "changes",
                subject_id: "branch",
                subject_key: "changes",
                subject_label: "changes",
                subject_version: unstaged_version,
                created_by: "alice",
            },
        )
        .await
        .unwrap();
        for version in ["changes-v1:digest", "changes-commit-v1:0123abcd:digest"] {
            let err = add_comment(
                &db,
                review.id,
                "alice",
                review.draft_revision,
                &NewComment {
                    subject_version: version,
                    anchor: &anchor("beta"),
                    body: "wrong scope",
                },
            )
            .await
            .unwrap_err();
            assert!(err.to_string().contains("another snapshot"), "{err}");
        }
        // Its own spelling — digest moved by a live edit or not — is home.
        let review = add_comment(
            &db,
            review.id,
            "alice",
            review.draft_revision,
            &NewComment {
                subject_version: "changes-unstaged-v1:otherdigest",
                anchor: &anchor("beta"),
                body: "right scope",
            },
        )
        .await
        .unwrap();
        assert_eq!(review.comments.len(), 1);
    }

    #[tokio::test]
    async fn submit_is_atomic_idempotent_without_a_stale_gate() {
        let (db, artifact) = seeded().await;
        let first_draft = draft(&db, &artifact, "alice").await;
        let first_draft = add(&db, &first_draft, "change this").await;
        artifact::write(
            &db,
            &NewRevision {
                repo_root: "/repo",
                branch_id: artifact.branch_id.as_deref(),
                name: "design",
                kind: "markdown",
                title: "Design",
                content: "Alpha beta gamma delta",
                author: "agent",
            },
        )
        .await
        .unwrap();

        // The captured revision is provenance, not a gate: a draft started
        // against an older snapshot submits plainly, and staleness is decided
        // where each comment's anchor renders, not here.
        let first_draft = update_draft(
            &db,
            first_draft.id,
            "alice",
            first_draft.draft_revision,
            &DraftPatch {
                summary: Some("Overall note"),
            },
        )
        .await
        .unwrap();
        let submitted = submit(&db, first_draft.id, "alice", first_draft.draft_revision)
            .await
            .unwrap();
        assert!(submitted.event.is_some());
        assert_eq!(submitted.review.status, "submitted");

        let stale_retry = submit(&db, first_draft.id, "alice", 0).await.unwrap_err();
        assert!(stale_retry
            .downcast_ref::<DraftRevisionConflict>()
            .is_some());
        let retried = submit(&db, first_draft.id, "alice", first_draft.draft_revision)
            .await
            .unwrap();
        assert!(retried.event.is_none());
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM events WHERE branch_id = ? AND kind = 'review_submitted'",
        )
        .bind(artifact.branch_id.as_deref().unwrap())
        .fetch_one(&db)
        .await
        .unwrap();
        let outbox: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM review_delivery_outbox WHERE review_id = ?")
                .bind(first_draft.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(events, 1);
        assert_eq!(outbox, 1);

        let resolved =
            set_comment_resolved(&db, first_draft.id, submitted.review.comments[0].id, true)
                .await
                .unwrap();
        assert_eq!(resolved.status, "resolved");
        let reopened = set_comment_resolved(&db, first_draft.id, resolved.id, false)
            .await
            .unwrap();
        assert_eq!(reopened.status, "submitted");

        let next = draft(&db, &artifact, "alice").await;
        assert_ne!(next.id, first_draft.id);
        assert_ne!(next.delivery_key, first_draft.delivery_key);
    }

    #[tokio::test]
    async fn the_delivery_drain_maps_every_outbox_column() {
        let (db, artifact) = seeded().await;
        let first = add(&db, &draft(&db, &artifact, "alice").await, "change this").await;
        let first = update_draft(
            &db,
            first.id,
            "alice",
            first.draft_revision,
            &DraftPatch {
                summary: Some("A note the drain must carry"),
            },
        )
        .await
        .unwrap();
        let submitted = submit(&db, first.id, "alice", first.draft_revision)
            .await
            .unwrap();
        assert_eq!(submitted.review.delivery_state, "queued");

        // The background sweep maps rows through OutboxItem: a column drift
        // between the struct and this query fails every due row at once, and
        // the integration tests that would catch it are the two that cannot
        // run on this box — so the mapping is pinned here.
        let due = ready_outbox(&db, 10).await.unwrap();
        let row = due
            .iter()
            .find(|item| item.review_id == first.id)
            .expect("the queued round is due for the drain");
        assert_eq!(row.state, "queued");
        assert_eq!(row.summary, "A note the drain must carry");
        assert!(!row.next_attempt_at.is_empty());
    }

    #[tokio::test]
    async fn delivery_claim_has_a_recoverable_single_owner_lease() {
        let (db, artifact) = seeded().await;
        let draft = draft(&db, &artifact, "alice").await;
        let draft = add(&db, &draft, "change this").await;
        submit(&db, draft.id, "alice", draft.draft_revision)
            .await
            .unwrap();

        let first = claim_delivery(&db, draft.id).await.unwrap().unwrap();
        assert!(claim_delivery(&db, draft.id).await.unwrap().is_none());
        let state: String = sqlx::query_scalar("SELECT delivery_state FROM reviews WHERE id = ?")
            .bind(draft.id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(state, "delivering");

        sqlx::query(
            "UPDATE review_delivery_outbox
             SET next_attempt_at = '1970-01-01T00:00:00.000Z'
             WHERE review_id = ?",
        )
        .bind(draft.id)
        .execute(&db)
        .await
        .unwrap();
        let second = claim_delivery(&db, draft.id).await.unwrap().unwrap();
        assert_ne!(second.token, first.token);
        assert_eq!(
            mark_retry(&db, draft.id, &first.token, "stale worker")
                .await
                .unwrap(),
            None
        );
        assert!(mark_delivered(&db, draft.id, &second.token).await.unwrap());
        assert!(!mark_delivered(&db, draft.id, &first.token).await.unwrap());
        let state: String = sqlx::query_scalar("SELECT delivery_state FROM reviews WHERE id = ?")
            .bind(draft.id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(state, "delivered");
    }

    #[tokio::test]
    async fn expired_ownerless_delivery_returns_to_queue_without_an_attempt() {
        let (db, artifact) = seeded().await;
        let draft = add(&db, &draft(&db, &artifact, "alice").await, "queue this").await;
        submit(&db, draft.id, "alice", draft.draft_revision)
            .await
            .unwrap();
        claim_delivery(&db, draft.id).await.unwrap().unwrap();
        assert!(!release_expired_delivery(&db, draft.id).await.unwrap());
        sqlx::query(
            "UPDATE review_delivery_outbox
             SET next_attempt_at = '1970-01-01T00:00:00.000Z'
             WHERE review_id = ?",
        )
        .bind(draft.id)
        .execute(&db)
        .await
        .unwrap();
        assert!(release_expired_delivery(&db, draft.id).await.unwrap());
        let outbox: (String, i64, Option<String>) = sqlx::query_as(
            "SELECT state, attempts, lease_token
             FROM review_delivery_outbox WHERE review_id = ?",
        )
        .bind(draft.id)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(outbox, ("queued".to_string(), 0, None));
        let state: String = sqlx::query_scalar("SELECT delivery_state FROM reviews WHERE id = ?")
            .bind(draft.id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(state, "queued");
    }

    #[tokio::test]
    async fn anchors_are_bounded_individually_and_in_the_review_total() {
        let (db, artifact) = seeded().await;
        let mut draft = draft(&db, &artifact, "alice").await;
        let oversized: ReviewAnchor = ArtifactTextAnchor {
            quote: "x".repeat(MAX_QUOTE_BYTES + 1),
            prefix: String::new(),
            suffix: String::new(),
            block_index: None,
        }
        .into();
        let error = add_comment(
            &db,
            draft.id,
            "alice",
            draft.draft_revision,
            &comment("bounded body", &oversized),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("quote exceeds"));

        let body = "b".repeat(MAX_COMMENT_BYTES);
        let near_limit: ReviewAnchor = ArtifactTextAnchor {
            quote: "x".repeat(MAX_QUOTE_BYTES),
            prefix: "p".repeat(MAX_CONTEXT_BYTES),
            suffix: "s".repeat(MAX_CONTEXT_BYTES),
            block_index: Some(0),
        }
        .into();
        let mut inserted = 0;
        loop {
            match add_comment(
                &db,
                draft.id,
                "alice",
                draft.draft_revision,
                &comment(&body, &near_limit),
            )
            .await
            {
                Ok(updated) => {
                    draft = updated;
                    inserted += 1;
                }
                Err(error) => {
                    assert!(error.to_string().contains("256 KiB total"));
                    break;
                }
            }
        }
        assert!(inserted > 10);
    }

    #[tokio::test]
    async fn delivery_failure_state_and_manual_retry_are_honest() {
        let (db, artifact) = seeded().await;
        let draft = draft(&db, &artifact, "alice").await;
        let draft = add(&db, &draft, "change this").await;
        submit(&db, draft.id, "alice", draft.draft_revision)
            .await
            .unwrap();

        let error = retry_delivery(&db, draft.id).await.unwrap_err();
        assert!(error.to_string().contains("failed review delivery"));
        for (index, expected) in ["retrying", "retrying", "failed"].into_iter().enumerate() {
            sqlx::query(
                "UPDATE review_delivery_outbox
                 SET next_attempt_at = '1970-01-01T00:00:00.000Z'
                 WHERE review_id = ?",
            )
            .bind(draft.id)
            .execute(&db)
            .await
            .unwrap();
            let lease = claim_delivery(&db, draft.id).await.unwrap().unwrap();
            assert_eq!(
                mark_retry(&db, draft.id, &lease.token, &format!("failure {index}"))
                    .await
                    .unwrap()
                    .as_deref(),
                Some(expected)
            );
        }

        retry_delivery(&db, draft.id).await.unwrap();
        let error = retry_delivery(&db, draft.id).await.unwrap_err();
        assert!(error.to_string().contains("failed review delivery"));
        let state: String = sqlx::query_scalar("SELECT delivery_state FROM reviews WHERE id = ?")
            .bind(draft.id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(state, "queued");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_artifact_write_and_submit_serialize_cleanly() {
        let (db, artifact) = seeded().await;
        let draft = add(&db, &draft(&db, &artifact, "alice").await, "change this").await;
        let barrier = Arc::new(Barrier::new(3));

        let submit_db = db.clone();
        let submit_barrier = barrier.clone();
        let submit_task = tokio::spawn(async move {
            submit_barrier.wait().await;
            submit(&submit_db, draft.id, "alice", draft.draft_revision).await
        });
        let write_db = db.clone();
        let write_barrier = barrier.clone();
        let branch_id = artifact.branch_id.clone();
        let write_task = tokio::spawn(async move {
            write_barrier.wait().await;
            artifact::write(
                &write_db,
                &NewRevision {
                    repo_root: "/repo",
                    branch_id: branch_id.as_deref(),
                    name: "design",
                    kind: "markdown",
                    title: "Design",
                    content: "concurrent revision",
                    author: "agent",
                },
            )
            .await
        });
        barrier.wait().await;
        let submission = submit_task.await.unwrap();
        write_task.await.unwrap().unwrap();

        // There is no stale decision to make any more: the captured revision
        // is provenance, so the submit wins in either interleaving and the
        // review keeps the revision it began against.
        let submission = submission.expect("submit no longer consults the subject");
        assert!(submission.event.is_some());
        assert_eq!(submission.review.subject_version, "1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_draft_mutation_cannot_hide_content_from_submit_preview() {
        let (db, artifact) = seeded().await;
        let draft = add(&db, &draft(&db, &artifact, "alice").await, "first").await;
        let barrier = Arc::new(Barrier::new(3));

        let submit_db = db.clone();
        let submit_barrier = barrier.clone();
        let submit_id = draft.id;
        let expected = draft.draft_revision;
        let submit_task = tokio::spawn(async move {
            submit_barrier.wait().await;
            submit(&submit_db, submit_id, "alice", expected).await
        });
        let mutate_db = db.clone();
        let mutate_barrier = barrier.clone();
        let mutation_task = tokio::spawn(async move {
            let anchor = anchor("gamma");
            mutate_barrier.wait().await;
            add_comment(
                &mutate_db,
                submit_id,
                "alice",
                expected,
                &NewComment {
                    subject_version: "1",
                    anchor: &anchor,
                    body: "concurrent",
                },
            )
            .await
        });
        barrier.wait().await;
        let submission = submit_task.await.unwrap();
        let mutation = mutation_task.await.unwrap();

        match (submission, mutation) {
            (Ok(submitted), Ok(mutated)) => {
                // Submit won the race: the initial round's event carries
                // exactly the pre-existing comment — nothing sneaks into
                // the delivered payload — and the racing add simply queues
                // on the now-submitted envelope as the continuation's first
                // pending comment, delivered by the next round.
                let event_comments = submitted
                    .event
                    .as_ref()
                    .and_then(|event| event.data.get("comments"))
                    .and_then(|comments| comments.as_array())
                    .expect("the submission event carries its comments");
                assert_eq!(event_comments.len(), 1);
                assert_eq!(event_comments[0]["body"], "first");
                assert_eq!(mutated.comments.len(), 2);
                assert_eq!(mutated.comments.last().unwrap().status, "pending");
            }
            (Err(error), Ok(mutated)) => {
                // The mutation won the revision race: the submit conflicts
                // and the comment rides the envelope to the next submit.
                assert!(error.downcast_ref::<DraftRevisionConflict>().is_some());
                assert_eq!(mutated.comments.len(), 2);
            }
            outcome => panic!("one of the two race outcomes must hold: {outcome:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_submit_retries_freeze_one_event_and_outbox_item() {
        let (db, artifact) = seeded().await;
        let draft = add(&db, &draft(&db, &artifact, "alice").await, "submit once").await;
        let barrier = Arc::new(Barrier::new(3));
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let task_db = db.clone();
            let task_barrier = barrier.clone();
            let review_id = draft.id;
            let expected = draft.draft_revision;
            tasks.push(tokio::spawn(async move {
                task_barrier.wait().await;
                submit(&task_db, review_id, "alice", expected).await
            }));
        }
        barrier.wait().await;
        let first = tasks.remove(0).await.unwrap().unwrap();
        let second = tasks.remove(0).await.unwrap().unwrap();
        assert_eq!(
            usize::from(first.event.is_some()) + usize::from(second.event.is_some()),
            1
        );
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'review_submitted'")
                .fetch_one(&db)
                .await
                .unwrap();
        let outbox: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM review_delivery_outbox WHERE review_id = ?")
                .bind(draft.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!((events, outbox), (1, 1));
    }

    #[test]
    fn structured_message_keeps_anchor_context_out_of_the_prompt() {
        let text = Review {
            comments: vec![ReviewComment {
                id: 1,
                review_id: 7,
                subject_version: "2".to_string(),
                anchor_kind: "text".to_string(),
                anchor_json: serde_json::to_string(&ArtifactTextAnchor {
                    quote: "beta".to_string(),
                    prefix: "Alpha ".to_string(),
                    suffix: " gamma".to_string(),
                    block_index: Some(1),
                })
                .unwrap(),
                body: "Explain this.".to_string(),
                status: "submitted".to_string(),
                replies: Vec::new(),
                created_at: String::new(),
                updated_at: String::new(),
            }],
            subject_kind: "artifact".to_string(),
            subject_label: "design".to_string(),
            subject_version: "2".to_string(),
            ..review_fixture()
        };
        let message = structured_message(&text);
        assert!(message.contains("1. \"beta\", block 1 (revision 2, comment id 1):\nExplain this."));
        assert!(!message.contains("Alpha "));
        assert!(!message.contains(" gamma"));
        assert!(message.contains(
            "Take action on any comment that calls for a change, then respond to every comment \
             with the `review_comment_reply` tool"
        ));
        assert!(message
            .contains("full anchor and surrounding context is served by the `review_list` tool"));

        let change = Review {
            comments: vec![ReviewComment {
                id: 2,
                review_id: 7,
                subject_version: "3".to_string(),
                anchor_kind: "change".to_string(),
                anchor_json: serde_json::to_string(&ChangeLineAnchor {
                    path_bytes: "crates/foo.rs".to_string(),
                    path_display: "crates/foo.rs".to_string(),
                    side: "new".to_string(),
                    start_line: 10,
                    end_line: 14,
                    hunk_header: "@@ -9,7 +9,7 @@".to_string(),
                    context_before: vec!["fn old() {".to_string()],
                    selected: vec!["    let x = 1;".to_string()],
                    context_after: vec!["}".to_string()],
                })
                .unwrap(),
                body: "Tighten this.".to_string(),
                status: "submitted".to_string(),
                replies: Vec::new(),
                created_at: String::new(),
                updated_at: String::new(),
            }],
            subject_kind: "changes".to_string(),
            subject_version: "3".to_string(),
            ..review_fixture()
        };
        let message = structured_message(&change);
        assert!(message.contains("1. crates/foo.rs · new lines 10-14 (revision 3, comment id 2):"));
        assert!(!message.contains("fn old()"));
        assert!(!message.contains("let x = 1;"));
        assert!(message.contains("[review_id: 7; delivery_key: key-7]"));
        assert!(message.contains("Resolving a comment is the reviewer's action, not yours."));
    }

    #[tokio::test]
    async fn reanchoring_moves_the_comment_not_the_envelope() {
        let (db, artifact) = seeded().await;
        let draft = add(&db, &draft(&db, &artifact, "alice").await, "change this").await;
        let moved_anchor = anchor("gamma");
        let moved = patch_comment(
            &db,
            draft.id,
            draft.comments[0].id,
            "alice",
            draft.draft_revision,
            &CommentPatch {
                subject_version: Some("2"),
                anchor: Some(&moved_anchor),
                body: None,
            },
        )
        .await
        .unwrap();
        // The captured revision is provenance: a re-anchored comment carries
        // the new version, the envelope keeps where the review began, and the
        // delivered message quotes each comment's own revision.
        assert_eq!(moved.subject_version, "1");
        assert_eq!(moved.comments[0].subject_version, "2");
        assert!(structured_message(&moved).contains("revision 2"));
    }

    #[tokio::test]
    async fn follow_up_rounds_carry_new_comments_and_replies_together() {
        let (db, artifact) = seeded().await;
        let first = draft(&db, &artifact, "alice").await;
        let first = add(&db, &first, "change this").await;
        let submitted = submit(&db, first.id, "alice", first.draft_revision)
            .await
            .unwrap();
        assert_eq!(submitted.review.reply_round, 0);
        let base_key = submitted.review.delivery_key.clone();
        let comment_id = submitted.review.comments[0].id;

        // New comments attach to the submitted envelope directly — the
        // comment-level continuation — while get_or_create keeps composing
        // fresh drafts for a brand-new round workflow.
        let continuation = add_comment(
            &db,
            submitted.review.id,
            "alice",
            submitted.review.draft_revision,
            &NewComment {
                subject_version: "1",
                anchor: &anchor("gamma"),
                body: "a brand-new follow-up comment",
            },
        )
        .await
        .unwrap();
        let new_comment_id = continuation.comments.last().unwrap().id;
        assert_eq!(continuation.comments.last().unwrap().status, "pending");
        assert_eq!(continuation.pending_replies, 0);
        let fresh = get_or_create(
            &db,
            &NewReview {
                repo_root: &artifact.repo_root,
                branch_id: artifact.branch_id.as_deref().unwrap(),
                session_id: "session-1",
                subject_kind: "artifact",
                subject_id: &artifact.id.to_string(),
                subject_key: &artifact.name,
                subject_label: &artifact.name,
                subject_version: "1",
                created_by: "alice",
            },
        )
        .await
        .unwrap();
        assert_ne!(fresh.id, continuation.id, "a fresh draft is a new envelope");

        // The initial delivery lands: its delivered cursors move past its
        // comments.
        let lease = claim_delivery(&db, submitted.review.id)
            .await
            .unwrap()
            .unwrap();
        assert!(mark_delivered(&db, submitted.review.id, &lease.token)
            .await
            .unwrap());
        let delivered = get(&db, submitted.review.id).await.unwrap().unwrap();
        assert_eq!(delivered.comment_upto, comment_id);
        assert_eq!(delivered.reply_upto, 0);
        assert_eq!(delivered.pending_replies, 0);

        // The session answers, the reviewer replies, and — the new shape —
        // the reviewer also writes a brand-new comment on the submitted
        // review. It queues there, exactly like a draft comment does.
        reply_comment(&db, comment_id, ReplyAuthor::Session("session-1"), "done")
            .await
            .unwrap();
        reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Operator("alice"),
            "thanks — one more thing",
        )
        .await
        .unwrap();
        let live = get(&db, continuation.id).await.unwrap().unwrap();
        assert_eq!(live.pending_replies, 1);

        // One submission sends both kinds as one round: a fresh delivery
        // key, the pending comment joining the delivered set, and the frozen
        // outbox payload quoting comment and reply alike.
        let round = submit(&db, continuation.id, "alice", continuation.draft_revision)
            .await
            .unwrap();
        assert_eq!(round.review.reply_round, 1);
        assert_eq!(round.review.delivery_key, format!("{base_key}#1"));
        assert_eq!(round.review.delivery_state, "queued");
        assert!(round.event.is_some());
        let flipped = get(&db, continuation.id).await.unwrap().unwrap();
        assert_eq!(
            flipped.comments.last().unwrap().status,
            "submitted",
            "the round owns the pending comment now"
        );
        // Undelivered until the transport says so — the cursors have not
        // moved, so the words are still the reviewer's side of the loop.
        assert_eq!(flipped.pending_replies, 1);
        let payload: String =
            sqlx::query_scalar("SELECT payload FROM review_delivery_outbox WHERE review_id = ?")
                .bind(continuation.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(
            payload.contains("a brand-new follow-up comment"),
            "{payload}"
        );
        assert!(payload.contains("thanks — one more thing"), "{payload}");
        assert!(!payload.contains("change this"), "{payload}");
        assert!(
            payload.contains(&format!("comment id {new_comment_id}")),
            "{payload}"
        );

        // The round delivers: both cursors advance past what it carried, and
        // the resubmit afterwards is the read it always was.
        let lease = claim_delivery(&db, continuation.id).await.unwrap().unwrap();
        assert!(mark_delivered(&db, continuation.id, &lease.token)
            .await
            .unwrap());
        let after = get(&db, continuation.id).await.unwrap().unwrap();
        assert_eq!(after.comment_upto, new_comment_id);
        assert!(after.reply_upto >= 2, "past the operator reply");
        assert_eq!(after.pending_replies, 0);
        let read = submit(&db, continuation.id, "alice", after.draft_revision)
            .await
            .unwrap();
        assert!(read.event.is_none());
        assert_eq!(read.review.delivery_key, round.review.delivery_key);
    }

    #[tokio::test]
    async fn a_changed_note_rides_a_follow_up_round() {
        let (db, artifact) = seeded().await;
        let first = draft(&db, &artifact, "alice").await;
        let first = add(&db, &first, "change this").await;
        let first = update_draft(
            &db,
            first.id,
            "alice",
            first.draft_revision,
            &DraftPatch {
                summary: Some("First overall note"),
            },
        )
        .await
        .unwrap();
        let submitted = submit(&db, first.id, "alice", first.draft_revision)
            .await
            .unwrap();
        let base_key = submitted.review.delivery_key.clone();
        let lease = claim_delivery(&db, submitted.review.id)
            .await
            .unwrap()
            .unwrap();
        assert!(mark_delivered(&db, submitted.review.id, &lease.token)
            .await
            .unwrap());
        let delivered = get(&db, first.id).await.unwrap().unwrap();
        assert_eq!(delivered.delivered_summary, "First overall note");

        // The reviewer refines the note on the still-open envelope. It is
        // pending words now, exactly like a pending comment.
        let edited = update_draft(
            &db,
            first.id,
            "alice",
            delivered.draft_revision,
            &DraftPatch {
                summary: Some("Refined overall note"),
            },
        )
        .await
        .unwrap();
        assert_eq!(edited.status, "submitted");
        let round = submit(&db, first.id, "alice", edited.draft_revision)
            .await
            .unwrap();
        assert_eq!(round.review.reply_round, 1);
        assert_eq!(round.review.delivery_key, format!("{base_key}#1"));
        let payload: String =
            sqlx::query_scalar("SELECT payload FROM review_delivery_outbox WHERE review_id = ?")
                .bind(first.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(
            payload.contains("Overall:\nRefined overall note"),
            "{payload}"
        );
        assert!(!payload.contains("First overall note"), "{payload}");

        // The live batch carries the note, so a resubmit with nothing new is
        // the read it always was — both before and after delivery lands.
        let read = submit(&db, first.id, "alice", round.review.draft_revision)
            .await
            .unwrap();
        assert!(read.event.is_none());
        let lease = claim_delivery(&db, first.id).await.unwrap().unwrap();
        assert!(mark_delivered(&db, first.id, &lease.token).await.unwrap());
        let after = get(&db, first.id).await.unwrap().unwrap();
        assert_eq!(after.delivered_summary, "Refined overall note");
        let read = submit(&db, first.id, "alice", after.draft_revision)
            .await
            .unwrap();
        assert!(read.event.is_none());

        // The note that never delivered is never stranded either: an
        // undelivered initial batch carries its note, and a note edited while
        // it sits there rides the superseding round.
        let second_subject = draft(&db, &artifact, "bob").await;
        let bob_anchor = anchor("beta");
        let second = add_comment(
            &db,
            second_subject.id,
            "bob",
            second_subject.draft_revision,
            &NewComment {
                subject_version: "1",
                anchor: &bob_anchor,
                body: "another change",
            },
        )
        .await
        .unwrap();
        let second = update_draft(
            &db,
            second.id,
            "bob",
            second.draft_revision,
            &DraftPatch {
                summary: Some("Undelivered note"),
            },
        )
        .await
        .unwrap();
        let queued = submit(&db, second.id, "bob", second.draft_revision)
            .await
            .unwrap();
        assert_eq!(queued.review.delivery_state, "queued");
        let edited = update_draft(
            &db,
            second.id,
            "bob",
            queued.review.draft_revision,
            &DraftPatch {
                summary: Some("Note while in flight"),
            },
        )
        .await
        .unwrap();
        let superseded = submit(&db, second.id, "bob", edited.draft_revision)
            .await
            .unwrap();
        assert_eq!(superseded.review.reply_round, 1);
        let payload: String =
            sqlx::query_scalar("SELECT payload FROM review_delivery_outbox WHERE review_id = ?")
                .bind(second.id)
                .fetch_one(&db)
                .await
                .unwrap();
        // The single live batch re-carries everything undelivered: the
        // comment, and the note as it stands now — never the stale first
        // note, and never dropped.
        assert!(payload.contains("another change"), "{payload}");
        assert!(
            payload.contains("Overall:\nNote while in flight"),
            "{payload}"
        );
        assert!(!payload.contains("Undelivered note"), "{payload}");
    }

    #[tokio::test]
    async fn a_cleared_note_rides_as_words_not_silence() {
        let (db, artifact) = seeded().await;
        let first = add(&db, &draft(&db, &artifact, "alice").await, "change this").await;
        let first = update_draft(
            &db,
            first.id,
            "alice",
            first.draft_revision,
            &DraftPatch {
                summary: Some("A note that will be retracted"),
            },
        )
        .await
        .unwrap();
        let submitted = submit(&db, first.id, "alice", first.draft_revision)
            .await
            .unwrap();
        let lease = claim_delivery(&db, first.id).await.unwrap().unwrap();
        assert!(mark_delivered(&db, first.id, &lease.token).await.unwrap());

        // Retracting the note is a change the session must hear about, and
        // the delivered cursor must move past it — an empty round would
        // leave the cursor stranded and re-queue forever.
        let cleared = update_draft(
            &db,
            first.id,
            "alice",
            submitted.review.draft_revision,
            &DraftPatch { summary: Some("") },
        )
        .await
        .unwrap();
        assert_eq!(cleared.summary, "");
        let round = submit(&db, first.id, "alice", cleared.draft_revision)
            .await
            .unwrap();
        assert_eq!(round.review.reply_round, 1);
        let payload: String =
            sqlx::query_scalar("SELECT payload FROM review_delivery_outbox WHERE review_id = ?")
                .bind(first.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(payload.contains("cleared the overall note"), "{payload}");
        assert!(
            !payload.contains("A note that will be retracted"),
            "{payload}"
        );

        let lease = claim_delivery(&db, first.id).await.unwrap().unwrap();
        assert!(mark_delivered(&db, first.id, &lease.token).await.unwrap());
        let after = get(&db, first.id).await.unwrap().unwrap();
        assert_eq!(after.delivered_summary, "");
        let read = submit(&db, first.id, "alice", after.draft_revision)
            .await
            .unwrap();
        assert!(read.event.is_none());
    }

    #[tokio::test]
    async fn the_initial_payload_freezes_at_submit() {
        let (db, artifact) = seeded().await;
        let first = add(
            &db,
            &draft(&db, &artifact, "alice").await,
            "the original line",
        )
        .await;
        let first = update_draft(
            &db,
            first.id,
            "alice",
            first.draft_revision,
            &DraftPatch {
                summary: Some("Original note"),
            },
        )
        .await
        .unwrap();
        let submitted = submit(&db, first.id, "alice", first.draft_revision)
            .await
            .unwrap();
        assert_eq!(submitted.review.delivery_state, "queued");

        // Words keep moving while the initial round sits queued: the note is
        // refined, and a follow-up comment joins the open envelope. Neither
        // may leak into the initial delivery — its payload froze at submit,
        // or the delivered cursors would record words the session never saw
        // and the next round would duplicate them.
        let live = get(&db, first.id).await.unwrap().unwrap();
        assert_eq!(
            live.queued_summary.as_deref(),
            Some("Original note"),
            "the in-flight batch carries the note it froze with"
        );
        let edited = update_draft(
            &db,
            first.id,
            "alice",
            submitted.review.draft_revision,
            &DraftPatch {
                summary: Some("Refined note"),
            },
        )
        .await
        .unwrap();
        let later = add_comment(
            &db,
            first.id,
            "alice",
            edited.draft_revision,
            &NewComment {
                subject_version: "1",
                anchor: &anchor("gamma"),
                body: "a comment that arrived late",
            },
        )
        .await
        .unwrap();

        let lease = claim_delivery(&db, first.id).await.unwrap().unwrap();
        assert!(mark_delivered(&db, first.id, &lease.token).await.unwrap());
        let delivered = get(&db, first.id).await.unwrap().unwrap();
        assert_eq!(delivered.delivered_summary, "Original note");
        assert!(delivered.queued_summary.is_none(), "the batch landed");
        let frozen: String =
            sqlx::query_scalar("SELECT payload FROM review_delivery_outbox WHERE review_id = ?")
                .bind(first.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(frozen.contains("the original line"), "{frozen}");
        assert!(frozen.contains("Original note"), "{frozen}");
        assert!(!frozen.contains("Refined note"), "{frozen}");
        assert!(!frozen.contains("a comment that arrived late"), "{frozen}");

        // The next round carries exactly the late words, once each.
        let round = submit(&db, first.id, "alice", later.draft_revision)
            .await
            .unwrap();
        assert_eq!(round.review.reply_round, 1);
        let payload: String =
            sqlx::query_scalar("SELECT payload FROM review_delivery_outbox WHERE review_id = ?")
                .bind(first.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(payload.contains("Refined note"), "{payload}");
        assert!(payload.contains("a comment that arrived late"), "{payload}");
        assert!(!payload.contains("Original note"), "{payload}");
        assert!(!payload.contains("the original line"), "{payload}");
    }

    #[tokio::test]
    async fn preview_message_speaks_for_the_surface_it_is_on() {
        // A draft previews the initial payload it would submit.
        let (db, artifact) = seeded().await;
        let first = add(&db, &draft(&db, &artifact, "alice").await, "change this").await;
        assert!(preview_message(&first).contains("change this"));

        // A quiescent envelope previews its standing message: everything is
        // delivered, nothing would queue.
        submit(&db, first.id, "alice", first.draft_revision)
            .await
            .unwrap();
        let lease = claim_delivery(&db, first.id).await.unwrap().unwrap();
        assert!(mark_delivered(&db, first.id, &lease.token).await.unwrap());
        let quiescent = get(&db, first.id).await.unwrap().unwrap();
        assert!(preview_message(&quiescent).contains("change this"));

        // An envelope with pending words previews the round a submit would
        // send — the follow-up shape, not the standing one.
        let with_reply = reply_comment(
            &db,
            first.comments[0].id,
            ReplyAuthor::Operator("alice"),
            "and here is my answer",
        )
        .await
        .unwrap();
        let _ = with_reply;
        let live = get(&db, first.id).await.unwrap().unwrap();
        let preview = preview_message(&live);
        assert!(preview.contains("follow-up feedback"), "{preview}");
        assert!(preview.contains("and here is my answer"), "{preview}");
    }

    #[tokio::test]
    async fn a_reply_reopens_a_resolved_thread() {
        let (db, artifact) = seeded().await;
        let first = add(
            &db,
            &draft(&db, &artifact, "alice").await,
            "the original line",
        )
        .await;
        let submitted = submit(&db, first.id, "alice", first.draft_revision)
            .await
            .unwrap();
        let comment_id = submitted.review.comments[0].id;
        let resolved = set_comment_resolved(&db, first.id, comment_id, true)
            .await
            .unwrap();
        assert_eq!(resolved.status, "resolved");

        // A reply is the conversation continuing: whoever answers reopens
        // the closed thread, and it stays open until resolved again.
        let answered = reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Operator("bob"),
            "one more thing",
        )
        .await
        .unwrap();
        assert_eq!(answered.status, "submitted", "{answered:?}");
        let closed_again = set_comment_resolved(&db, first.id, comment_id, true)
            .await
            .unwrap();
        assert_eq!(closed_again.status, "resolved");
        let session_answer = reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Session("session-1"),
            "the session answers",
        )
        .await
        .unwrap();
        assert_eq!(session_answer.status, "submitted");
    }

    #[tokio::test]
    async fn the_author_revises_their_own_reply() {
        let (db, artifact) = seeded().await;
        let first = add(&db, &draft(&db, &artifact, "alice").await, "change this").await;
        let submitted = submit(&db, first.id, "alice", first.draft_revision)
            .await
            .unwrap();
        let comment_id = submitted.review.comments[0].id;
        let with_reply = reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Operator("alice"),
            "first take",
        )
        .await
        .unwrap();
        let reply_id = with_reply.replies[0].id;

        // Only the reply's own author rewrites it — not another operator.
        let err = update_reply(&db, reply_id, ReplyAuthor::Operator("bob"), "not mine")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("own author"), "{err}");

        // The author revises in place: the thread gains no second entry.
        let revised = update_reply(&db, reply_id, ReplyAuthor::Operator("alice"), "second take")
            .await
            .unwrap();
        assert_eq!(revised.replies.len(), 1);
        assert_eq!(revised.replies[0].body, "second take");

        // Retraction removes the words, same reach.
        let retracted = delete_reply(&db, reply_id, ReplyAuthor::Operator("alice"))
            .await
            .unwrap();
        assert!(retracted.replies.is_empty());

        // A session's reply stays the session's own words: no operator may
        // rewrite or retract them, the session revises its own answer.
        let answered = reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Session("session-1"),
            "the session answers",
        )
        .await
        .unwrap();
        let session_reply = answered
            .replies
            .iter()
            .find(|reply| reply.author_kind == "session")
            .unwrap();
        let err = update_reply(
            &db,
            session_reply.id,
            ReplyAuthor::Operator("alice"),
            "words in its mouth",
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("own author"), "{err}");
        let err = delete_reply(&db, session_reply.id, ReplyAuthor::Operator("alice"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("own author"), "{err}");
        let revised = update_reply(
            &db,
            session_reply.id,
            ReplyAuthor::Session("session-1"),
            "the session answers, revised",
        )
        .await
        .unwrap();
        assert!(revised
            .replies
            .iter()
            .any(|reply| reply.body.contains("revised")));
    }

    #[tokio::test]
    async fn a_delivered_reply_freezes_like_submitted_comment_content() {
        let (db, artifact) = seeded().await;
        let first = add(&db, &draft(&db, &artifact, "alice").await, "change this").await;
        let submitted = submit(&db, first.id, "alice", first.draft_revision)
            .await
            .unwrap();
        let comment_id = submitted.review.comments[0].id;
        let lease = claim_delivery(&db, first.id).await.unwrap().unwrap();
        assert!(mark_delivered(&db, first.id, &lease.token).await.unwrap());

        // An operator reply rides a follow-up round out to the session...
        reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Operator("alice"),
            "final words",
        )
        .await
        .unwrap();
        submit(&db, first.id, "alice", submitted.review.draft_revision)
            .await
            .unwrap();
        let lease = claim_delivery(&db, first.id).await.unwrap().unwrap();
        assert!(mark_delivered(&db, first.id, &lease.token).await.unwrap());
        let reply_id: i64 =
            sqlx::query_scalar("SELECT id FROM review_comment_replies WHERE comment_id = ?")
                .bind(comment_id)
                .fetch_one(&db)
                .await
                .unwrap();

        // ... and freezes at the cursor: the session has read it, so it is
        // history — the author can no longer rewrite or retract it.
        let err = update_reply(&db, reply_id, ReplyAuthor::Operator("alice"), "too late")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("history"), "{err}");
        let err = delete_reply(&db, reply_id, ReplyAuthor::Operator("alice"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("history"), "{err}");
    }

    #[tokio::test]
    async fn a_failed_round_self_heals_into_the_next_batch() {
        let (db, artifact) = seeded().await;
        let first = draft(&db, &artifact, "alice").await;
        let first = add(&db, &first, "change this").await;
        let submitted = submit(&db, first.id, "alice", first.draft_revision)
            .await
            .unwrap();
        let comment_id = submitted.review.comments[0].id;
        let lease = claim_delivery(&db, submitted.review.id)
            .await
            .unwrap()
            .unwrap();
        assert!(mark_delivered(&db, submitted.review.id, &lease.token)
            .await
            .unwrap());

        // A round is queued but its transport never completes: no cursor
        // advance, so nothing is presumed delivered.
        reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Operator("alice"),
            "words that failed to arrive",
        )
        .await
        .unwrap();
        let failed = submit(&db, first.id, "alice", submitted.review.draft_revision)
            .await
            .unwrap();
        assert_eq!(failed.review.reply_round, 1);

        // More words arrive, and the next round's batch recomputes
        // everything past the delivered cursor — the failed round's reply
        // rides too instead of being stranded by a watermark that moved at
        // submit time.
        reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Operator("alice"),
            "and these came later",
        )
        .await
        .unwrap();
        let round = submit(&db, first.id, "alice", failed.review.draft_revision)
            .await
            .unwrap();
        assert_eq!(round.review.reply_round, 2);
        let payload: String =
            sqlx::query_scalar("SELECT payload FROM review_delivery_outbox WHERE review_id = ?")
                .bind(first.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(payload.contains("words that failed to arrive"), "{payload}");
        assert!(payload.contains("and these came later"), "{payload}");
        // One live batch: the superseded round's window is gone with its
        // delivery key, so the once-only inbox sees each message exactly
        // once.
        assert_eq!(
            round.review.delivery_key,
            format!("{}#2", {
                let base = round
                    .review
                    .delivery_key
                    .split('#')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                base
            })
        );
    }

    #[tokio::test]
    async fn a_session_reply_alone_never_queues_a_round() {
        let (db, artifact) = seeded().await;
        let first = draft(&db, &artifact, "alice").await;
        let first = add(&db, &first, "change this").await;
        let submitted = submit(&db, first.id, "alice", first.draft_revision)
            .await
            .unwrap();
        let revision = submitted.review.draft_revision;
        let comment_id = submitted.review.comments[0].id;

        // The session answers after delivery. Its words are its own —
        // submitting again must not round-trip them back into the
        // conversation it answers as an empty "the reviewer replied".
        reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Session("session-1"),
            "already fixed",
        )
        .await
        .unwrap();
        let read = submit(&db, first.id, "alice", revision).await.unwrap();
        assert!(read.event.is_none(), "a session reply alone is not a round");
        assert_eq!(read.review.reply_round, 0);
        assert_eq!(read.review.delivery_key, submitted.review.delivery_key);
        assert_eq!(read.review.delivery_state, submitted.review.delivery_state);
        assert_eq!(read.review.pending_replies, 0);

        // The reviewer's words, whenever they arrive, do queue the round.
        reply_comment(
            &db,
            comment_id,
            ReplyAuthor::Operator("alice"),
            "one more thing",
        )
        .await
        .unwrap();
        let round = submit(&db, first.id, "alice", revision).await.unwrap();
        assert!(round.event.is_some());
        assert_eq!(round.review.reply_round, 1);
    }
}
