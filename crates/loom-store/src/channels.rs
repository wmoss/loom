//! Durable user/agent communication contexts.
//!
//! A session's default channel is inserted in the same transaction as the
//! session row. Messages are append-only and read state lives on per-subject
//! subscriptions; runtime delivery is a separate receipt.

use anyhow::{anyhow, Result};
use serde_json::Value;
use sqlx::{FromRow, Row, SqliteConnection};
use weaver_api::{ChannelDeliveryView, ChannelMessageView, ChannelSubscriptionView, ChannelView};

pub use crate::channel_data::{
    MessageKind, Subject, SubjectKind, SubscriptionMode, Urgency, ARCHIVED_STATE, CUSTOM_KIND,
    OPEN_STATE, SESSION_KIND,
};
use crate::db::{now_iso, Db};

#[derive(Debug, Clone, FromRow)]
struct ChannelRow {
    id: String,
    kind: String,
    repo_root: String,
    branch_id: Option<String>,
    session_id: Option<String>,
    name: String,
    topic: String,
    state: String,
    created_by_kind: String,
    created_by: String,
    created_at: String,
    archived_at: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
struct MessageRow {
    id: String,
    channel_id: String,
    seq: i64,
    kind: String,
    urgency: String,
    author_kind: String,
    author_id: String,
    body: String,
    payload: String,
    reply_to: Option<String>,
    created_at: String,
}

#[derive(Debug, Clone, FromRow)]
struct DeliveryRow {
    binding_id: String,
    binding_kind: String,
    target_session_id: Option<String>,
    state: String,
    attempts: i64,
    last_error: Option<String>,
    external_id: Option<String>,
    updated_at: String,
}

#[derive(Debug, Clone, FromRow)]
struct SubscriptionRow {
    channel_id: String,
    subject_kind: String,
    subject_id: String,
    mode: String,
    read_seq: i64,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone)]
pub struct ChannelAccess {
    pub branch_id: Option<String>,
    pub session_id: Option<String>,
    pub state: String,
    pub created_by_kind: String,
    pub created_by: String,
}

async fn upsert_subscription_tx(
    tx: &mut SqliteConnection,
    channel_id: &str,
    subject: &Subject,
    mode: Option<SubscriptionMode>,
    read_seq: Option<i64>,
    now: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO channel_subscriptions
         (channel_id, subject_kind, subject_id, mode, read_seq, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(channel_id, subject_kind, subject_id) DO UPDATE SET
           mode = CASE WHEN ? THEN excluded.mode ELSE channel_subscriptions.mode END,
           read_seq = MAX(channel_subscriptions.read_seq, excluded.read_seq),
           updated_at = excluded.updated_at",
    )
    .bind(channel_id)
    .bind(subject.kind.as_str())
    .bind(&subject.id)
    .bind(mode.unwrap_or(SubscriptionMode::Observe).as_str())
    .bind(read_seq.unwrap_or(0))
    .bind(now)
    .bind(now)
    .bind(mode.is_some())
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn advance_read_tx(
    tx: &mut SqliteConnection,
    channel_id: &str,
    subject: &Subject,
    seq: i64,
    now: &str,
) -> Result<()> {
    upsert_subscription_tx(tx, channel_id, subject, None, Some(seq), now).await
}

pub async fn create_custom(
    db: &Db,
    repo_root: &str,
    branch_id: Option<&str>,
    name: &str,
    topic: &str,
    creator: &Subject,
) -> Result<ChannelView> {
    let id = weaver_core::branch::new_id();
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    sqlx::query(
        "INSERT INTO channels
         (id, kind, repo_root, branch_id, name, topic, state,
          created_by_kind, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(CUSTOM_KIND)
    .bind(repo_root)
    .bind(branch_id)
    .bind(name)
    .bind(topic)
    .bind(OPEN_STATE)
    .bind(creator.kind.as_str())
    .bind(&creator.id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    upsert_subscription_tx(
        &mut tx,
        &id,
        creator,
        Some(SubscriptionMode::Observe),
        Some(0),
        &now,
    )
    .await?;
    tx.commit().await?;
    get(db, &id, creator)
        .await?
        .ok_or_else(|| anyhow!("channel vanished after insert"))
}

pub async fn get(db: &Db, id: &str, subject: &Subject) -> Result<Option<ChannelView>> {
    let row = sqlx::query_as::<_, ChannelRow>("SELECT * FROM channels WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?;
    match row {
        Some(row) => Ok(Some(channel_view(db, row, subject).await?)),
        None => Ok(None),
    }
}

pub async fn access(db: &Db, id: &str) -> Result<Option<ChannelAccess>> {
    let row = sqlx::query(
        "SELECT branch_id, session_id, state, created_by_kind, created_by
         FROM channels WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| ChannelAccess {
        branch_id: row.get("branch_id"),
        session_id: row.get("session_id"),
        state: row.get("state"),
        created_by_kind: row.get("created_by_kind"),
        created_by: row.get("created_by"),
    }))
}

pub async fn list_all(db: &Db, subject: &Subject, archived: bool) -> Result<Vec<ChannelView>> {
    let rows = sqlx::query_as::<_, ChannelRow>(
        "SELECT c.*
         FROM channels c
         LEFT JOIN sessions s ON s.id = c.session_id
         WHERE (c.session_id IS NULL OR s.managed_by IS NULL)
           AND (? OR c.state = 'open')
         ORDER BY c.created_at DESC",
    )
    .bind(archived)
    .fetch_all(db)
    .await?;
    views(db, rows, subject).await
}

pub async fn list_for_session_tree(
    db: &Db,
    root_session_id: &str,
    subject: &Subject,
    archived: bool,
) -> Result<Vec<ChannelView>> {
    let rows = sqlx::query_as::<_, ChannelRow>(
        "WITH RECURSIVE tree(id) AS (
           SELECT ?
           UNION ALL
           SELECT child.id
           FROM sessions child JOIN tree ON child.parent_session_id = tree.id
         )
         SELECT DISTINCT c.*
         FROM channels c
         LEFT JOIN channel_subscriptions sub
           ON sub.channel_id = c.id
          AND sub.subject_kind = ?
          AND sub.subject_id = ?
         WHERE (c.session_id IN (SELECT id FROM tree) OR sub.channel_id IS NOT NULL)
           AND (? OR c.state = 'open')
         ORDER BY c.created_at DESC",
    )
    .bind(root_session_id)
    .bind(subject.kind.as_str())
    .bind(&subject.id)
    .bind(archived)
    .fetch_all(db)
    .await?;
    views(db, rows, subject).await
}

async fn views(db: &Db, rows: Vec<ChannelRow>, subject: &Subject) -> Result<Vec<ChannelView>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(channel_view(db, row, subject).await?);
    }
    Ok(out)
}

async fn channel_view(db: &Db, row: ChannelRow, subject: &Subject) -> Result<ChannelView> {
    let read_seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE((
           SELECT read_seq FROM channel_subscriptions
           WHERE channel_id = ? AND subject_kind = ? AND subject_id = ?
         ), 0)",
    )
    .bind(&row.id)
    .bind(subject.kind.as_str())
    .bind(&subject.id)
    .fetch_one(db)
    .await?;
    let unread_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM channel_messages
         WHERE channel_id = ? AND seq > ?
           AND NOT (kind = 'goal' AND seq = 1)",
    )
    .bind(&row.id)
    .bind(read_seq)
    .fetch_one(db)
    .await?;
    let unread_urgent_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM channel_messages
         WHERE channel_id = ? AND seq > ? AND urgency IN ('attention', 'blocked')",
    )
    .bind(&row.id)
    .bind(read_seq)
    .fetch_one(db)
    .await?;
    let last_message = sqlx::query_as::<_, MessageRow>(
        "SELECT * FROM channel_messages WHERE channel_id = ? ORDER BY seq DESC LIMIT 1",
    )
    .bind(&row.id)
    .fetch_optional(db)
    .await?;
    Ok(ChannelView {
        id: row.id,
        kind: row.kind,
        repo_root: row.repo_root,
        branch_id: row.branch_id,
        session_id: row.session_id,
        name: row.name,
        topic: row.topic,
        state: row.state,
        created_by_kind: row.created_by_kind,
        created_by: row.created_by,
        created_at: row.created_at,
        archived_at: row.archived_at,
        unread_count,
        unread_urgent_count,
        last_message: match last_message {
            Some(message) => Some(message_view(db, message).await?),
            None => None,
        },
        // Populated by the server handler, which knows how to resolve
        // delivery bindings (session targets, the Slack origin thread); this
        // row-mapper only has the channel's own columns.
        bindings: Vec::new(),
    })
}

pub async fn messages(db: &Db, channel_id: &str, after: i64) -> Result<Vec<ChannelMessageView>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT * FROM channel_messages
         WHERE channel_id = ? AND seq > ? ORDER BY seq ASC",
    )
    .bind(channel_id)
    .bind(after)
    .fetch_all(db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(message_view(db, row).await?);
    }
    Ok(out)
}

async fn message_view(db: &Db, row: MessageRow) -> Result<ChannelMessageView> {
    let deliveries = sqlx::query_as::<_, DeliveryRow>(
        "SELECT binding_id, binding_kind, target_session_id, state, attempts,
                last_error, external_id, updated_at
         FROM channel_deliveries WHERE message_id = ? ORDER BY binding_id",
    )
    .bind(&row.id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|delivery| ChannelDeliveryView {
        binding_id: delivery.binding_id,
        binding_kind: delivery.binding_kind,
        target_session_id: delivery.target_session_id,
        state: delivery.state,
        attempts: delivery.attempts,
        last_error: delivery.last_error,
        external_id: delivery.external_id,
        updated_at: delivery.updated_at,
    })
    .collect();
    Ok(ChannelMessageView {
        id: row.id,
        channel_id: row.channel_id,
        seq: row.seq,
        kind: row.kind,
        urgency: row.urgency,
        author_kind: row.author_kind,
        author_id: row.author_id,
        body: row.body,
        payload: serde_json::from_str(&row.payload)?,
        reply_to: row.reply_to,
        created_at: row.created_at,
        deliveries,
    })
}

#[derive(Debug)]
pub struct NewMessage<'a> {
    pub kind: MessageKind,
    pub urgency: Urgency,
    pub author: &'a Subject,
    pub body: &'a str,
    pub payload: &'a Value,
    pub reply_to: Option<&'a str>,
    pub idempotency_key: Option<&'a str>,
}

pub async fn append(db: &Db, channel_id: &str, new: NewMessage<'_>) -> Result<ChannelMessageView> {
    Ok(append_with_outcome(db, channel_id, new).await?.message)
}

pub struct AppendOutcome {
    pub message: ChannelMessageView,
    pub inserted: bool,
}

pub async fn append_with_outcome(
    db: &Db,
    channel_id: &str,
    new: NewMessage<'_>,
) -> Result<AppendOutcome> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    if let Some(key) = new.idempotency_key {
        if let Some(row) = sqlx::query_as::<_, MessageRow>(
            "SELECT * FROM channel_messages
             WHERE channel_id = ? AND idempotency_key = ?",
        )
        .bind(channel_id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?
        {
            let read_at = now_iso();
            advance_read_tx(&mut tx, channel_id, new.author, row.seq, &read_at).await?;
            tx.commit().await?;
            return Ok(AppendOutcome {
                message: message_view(db, row).await?,
                inserted: false,
            });
        }
    }
    let seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM channel_messages WHERE channel_id = ?",
    )
    .bind(channel_id)
    .fetch_one(&mut *tx)
    .await?;
    let id = weaver_core::branch::new_id();
    let now = now_iso();
    sqlx::query(
        "INSERT INTO channel_messages
         (id, channel_id, seq, kind, urgency, author_kind, author_id, body,
          payload, reply_to, idempotency_key, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(channel_id)
    .bind(seq)
    .bind(new.kind.as_str())
    .bind(new.urgency.as_str())
    .bind(new.author.kind.as_str())
    .bind(&new.author.id)
    .bind(new.body)
    .bind(serde_json::to_string(new.payload)?)
    .bind(new.reply_to)
    .bind(new.idempotency_key)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    advance_read_tx(&mut tx, channel_id, new.author, seq, &now).await?;
    let row = sqlx::query_as::<_, MessageRow>("SELECT * FROM channel_messages WHERE id = ?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(AppendOutcome {
        message: message_view(db, row).await?,
        inserted: true,
    })
}

pub async fn set_subscription(
    db: &Db,
    channel_id: &str,
    subject: &Subject,
    mode: SubscriptionMode,
) -> Result<ChannelSubscriptionView> {
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    upsert_subscription_tx(&mut tx, channel_id, subject, Some(mode), None, &now).await?;
    let row = sqlx::query_as::<_, SubscriptionRow>(
        "SELECT * FROM channel_subscriptions
         WHERE channel_id = ? AND subject_kind = ? AND subject_id = ?",
    )
    .bind(channel_id)
    .bind(subject.kind.as_str())
    .bind(&subject.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(subscription_view(row))
}

pub async fn mark_read(
    db: &Db,
    channel_id: &str,
    subject: &Subject,
    requested_seq: Option<i64>,
) -> Result<ChannelSubscriptionView> {
    let max_seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) FROM channel_messages WHERE channel_id = ?",
    )
    .bind(channel_id)
    .fetch_one(db)
    .await?;
    let seq = requested_seq.unwrap_or(max_seq).clamp(0, max_seq);
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    // Advancing a read marker must not silently downgrade a `deliver`
    // subscription to `observe`. The default only applies when this is the
    // subject's first interaction with the channel.
    advance_read_tx(&mut tx, channel_id, subject, seq, &now).await?;
    retract_consumed_result_notices_tx(&mut tx, channel_id, subject, seq).await?;
    let row = sqlx::query_as::<_, SubscriptionRow>(
        "SELECT * FROM channel_subscriptions
         WHERE channel_id = ? AND subject_kind = ? AND subject_id = ?",
    )
    .bind(channel_id)
    .bind(subject.kind.as_str())
    .bind(&subject.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(subscription_view(row))
}

/// The idempotency-key prefix linking a child's `result` to the notice Loom
/// appends to the parent's channel for it.
pub const CHILD_RESULT_NOTICE_KEY_PREFIX: &str = "child-result:";

/// Idempotency key of the notice a child's `result` produces on the parent's
/// channel. Defined beside the retraction that looks the notice up by it.
pub fn child_result_notice_key(result_message_id: &str) -> String {
    format!("{CHILD_RESULT_NOTICE_KEY_PREFIX}{result_message_id}")
}

/// A queued child-result notice tells the parent to read the result. Once the
/// parent's read marker on the child's channel passes the result, the notice
/// is stale: retract it from the parent's prompt queue so the next turn
/// boundary does not deliver a pointer to something already consumed. Only the
/// notice's own recipient retracts — another ancestor reading the child's
/// channel says nothing about the parent's queue. The check is best-effort at
/// read time: a read that races in after the result exists but before the
/// notice paragraph is queued retracts nothing, and that notice is then
/// delivered as it always was.
async fn retract_consumed_result_notices_tx(
    tx: &mut SqliteConnection,
    channel_id: &str,
    reader: &Subject,
    read_seq: i64,
) -> Result<()> {
    if reader.kind != SubjectKind::Session {
        return Ok(());
    }
    let child: Option<String> = sqlx::query_scalar(
        "SELECT session_id FROM channels WHERE id = ? AND session_id IS NOT NULL",
    )
    .bind(channel_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(child) = child else {
        return Ok(());
    };
    let parent: Option<String> =
        sqlx::query_scalar("SELECT parent_session_id FROM sessions WHERE id = ?")
            .bind(&child)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
    if parent.as_deref() != Some(reader.id.as_str()) {
        return Ok(());
    }
    let notice_bodies: Vec<String> = sqlx::query_scalar(
        "SELECT n.body
         FROM channel_messages m
         JOIN channel_messages n
           ON n.channel_id = ? AND n.idempotency_key = ? || m.id
         WHERE m.channel_id = ? AND m.kind = 'result' AND m.seq <= ?",
    )
    .bind(&reader.id)
    .bind(CHILD_RESULT_NOTICE_KEY_PREFIX)
    .bind(channel_id)
    .bind(read_seq)
    .fetch_all(&mut *tx)
    .await?;
    for body in notice_bodies {
        crate::session::retract_pending_prompt_paragraph_tx(tx, &reader.id, &body).await?;
    }
    Ok(())
}

fn subscription_view(row: SubscriptionRow) -> ChannelSubscriptionView {
    ChannelSubscriptionView {
        channel_id: row.channel_id,
        subject_kind: row.subject_kind,
        subject_id: row.subject_id,
        mode: row.mode,
        read_seq: row.read_seq,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

pub async fn create_delivery(
    db: &Db,
    message_id: &str,
    binding_id: &str,
    binding_kind: &str,
    target_session_id: Option<&str>,
) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO channel_deliveries
         (message_id, binding_id, binding_kind, target_session_id, state, attempts, updated_at)
         VALUES (?, ?, ?, ?, 'queued', 0, ?)
         ON CONFLICT(message_id, binding_id) DO NOTHING",
    )
    .bind(message_id)
    .bind(binding_id)
    .bind(binding_kind)
    .bind(target_session_id)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn finish_delivery(
    db: &Db,
    message_id: &str,
    binding_id: &str,
    error: Option<&str>,
    external_id: Option<&str>,
) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "UPDATE channel_deliveries
         SET state = ?, attempts = attempts + 1, last_error = ?, external_id = ?, updated_at = ?
         WHERE message_id = ? AND binding_id = ?",
    )
    .bind(if error.is_some() {
        "failed"
    } else {
        "delivered"
    })
    .bind(error)
    .bind(external_id)
    .bind(now)
    .bind(message_id)
    .bind(binding_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn delivery_succeeded(db: &Db, message_id: &str, binding_id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM channel_deliveries
         WHERE message_id = ? AND binding_id = ? AND state = 'delivered')",
    )
    .bind(message_id)
    .bind(binding_id)
    .fetch_one(db)
    .await?)
}

pub async fn refresh_message(db: &Db, message_id: &str) -> Result<ChannelMessageView> {
    let row = sqlx::query_as::<_, MessageRow>("SELECT * FROM channel_messages WHERE id = ?")
        .bind(message_id)
        .fetch_one(db)
        .await?;
    message_view(db, row).await
}

pub async fn delivery_targets(db: &Db, channel_id: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT sub.subject_id
         FROM channel_subscriptions sub
         JOIN sessions s ON s.id = sub.subject_id
         WHERE sub.channel_id = ?
           AND sub.subject_kind = 'session'
           AND sub.mode = 'deliver'
         ORDER BY sub.subject_id",
    )
    .bind(channel_id)
    .fetch_all(db)
    .await?)
}

pub async fn archive_session_channel(db: &Db, session_id: &str) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "UPDATE channels SET state = ?, archived_at = ?
         WHERE session_id = ? AND state != ?",
    )
    .bind(ARCHIVED_STATE)
    .bind(now)
    .bind(session_id)
    .bind(ARCHIVED_STATE)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn reopen_session_channel(db: &Db, session_id: &str) -> Result<()> {
    let changed = sqlx::query(
        "UPDATE channels SET state = ?, archived_at = NULL
         WHERE session_id = ? AND state = ?",
    )
    .bind(OPEN_STATE)
    .bind(session_id)
    .bind(ARCHIVED_STATE)
    .execute(db)
    .await?;
    if changed.rows_affected() > 0 {
        let author = Subject::new(SubjectKind::System, "loom");
        append(
            db,
            session_id,
            NewMessage {
                kind: MessageKind::System,
                urgency: Urgency::Normal,
                author: &author,
                body: "session recovered",
                payload: &Value::Null,
                reply_to: None,
                idempotency_key: None,
            },
        )
        .await?;
    }
    Ok(())
}

pub async fn archive_custom(db: &Db, channel_id: &str) -> Result<bool> {
    let now = now_iso();
    let result = sqlx::query(
        "UPDATE channels SET state = ?, archived_at = ?
         WHERE id = ? AND kind = ? AND state != ?",
    )
    .bind(ARCHIVED_STATE)
    .bind(now)
    .bind(channel_id)
    .bind(CUSTOM_KIND)
    .bind(ARCHIVED_STATE)
    .execute(db)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn update_session_goal(db: &Db, session_id: &str, goal: &str) -> Result<()> {
    let state: Option<String> =
        sqlx::query_scalar("UPDATE channels SET topic = ? WHERE session_id = ? RETURNING state")
            .bind(goal)
            .bind(session_id)
            .fetch_optional(db)
            .await?;
    if state.as_deref() != Some(OPEN_STATE) {
        return Ok(());
    }
    let author = Subject::new(SubjectKind::User, "manual");
    append(
        db,
        session_id,
        NewMessage {
            kind: MessageKind::Goal,
            urgency: Urgency::Normal,
            author: &author,
            body: if goal.trim().is_empty() {
                "(goal cleared)"
            } else {
                goal
            },
            payload: &serde_json::json!({ "updated": true, "goal": goal }),
            reply_to: None,
            idempotency_key: None,
        },
    )
    .await?;
    Ok(())
}

pub async fn update_branch_channel_names(db: &Db, branch_id: &str, name: &str) -> Result<()> {
    sqlx::query(
        "UPDATE channels SET name = ?
         WHERE branch_id = ? AND session_id IS NOT NULL",
    )
    .bind(name)
    .bind(branch_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn session_channel_for_branch(db: &Db, branch_id: &str) -> Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT c.id
         FROM channels c
         JOIN sessions s ON s.id = c.session_id
         WHERE c.branch_id = ? AND c.state = 'open'
         ORDER BY CASE WHEN s.status IN ('done', 'error', 'archived') THEN 1 ELSE 0 END,
                  s.created_at DESC
         LIMIT 1",
    )
    .bind(branch_id)
    .fetch_optional(db)
    .await
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{self, NewSession};
    use weaver_core::branch as branch_mod;

    fn new_session(id: &str, branch_id: &str) -> NewSession {
        NewSession {
            id: id.to_string(),
            branch_id: branch_id.to_string(),
            work_dir: format!("/work/{id}"),
            term_session: format!("weaver-{id}"),
            agent_kind: "shell".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: None,
            created_by: Some("alice".to_string()),
            protocol: "terminal".to_string(),
            origin: "user".to_string(),
            class: "interactive".to_string(),
            tracking_issue_id: None,
        }
    }

    #[tokio::test]
    async fn session_insert_creates_goal_channel_and_preserves_delivery_mode_on_read() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_mod::upsert(&db, "/repo", "weaver/channels", "main")
            .await
            .unwrap();
        branch_mod::set_title(
            &db,
            &branch.id,
            "Channel work",
            branch_mod::TitleProvenance::User,
        )
        .await
        .unwrap();
        branch_mod::set_goal(&db, &branch.id, "Build durable channels", "user")
            .await
            .unwrap();
        session::insert(&db, &new_session("session-1", &branch.id))
            .await
            .unwrap();

        let owner = Subject::new(SubjectKind::Session, "session-1");
        let user = Subject::new(SubjectKind::User, "alice");
        let channel = get(&db, "session-1", &user).await.unwrap().unwrap();
        assert_eq!(channel.kind, SESSION_KIND);
        assert_eq!(channel.name, "Channel work");
        assert_eq!(channel.topic, "Build durable channels");
        assert_eq!(channel.unread_count, 0);

        update_branch_channel_names(&db, &branch.id, "Renamed channel")
            .await
            .unwrap();
        assert_eq!(
            get(&db, "session-1", &user).await.unwrap().unwrap().name,
            "Renamed channel"
        );

        let opening = messages(&db, "session-1", 0).await.unwrap();
        assert_eq!(opening.len(), 1);
        assert_eq!(opening[0].kind, MessageKind::Goal.as_str());
        assert_eq!(opening[0].body, "Build durable channels");

        update_session_goal(&db, "session-1", "Ship the channel API")
            .await
            .unwrap();
        let updated = get(&db, "session-1", &user).await.unwrap().unwrap();
        assert_eq!(updated.topic, "Ship the channel API");
        let goal_updates = messages(&db, "session-1", 1).await.unwrap();
        assert_eq!(goal_updates.len(), 1);
        assert_eq!(goal_updates[0].kind, MessageKind::Goal.as_str());
        assert_eq!(goal_updates[0].body, "Ship the channel API");

        let posted = append(
            &db,
            "session-1",
            NewMessage {
                kind: MessageKind::Message,
                urgency: Urgency::Attention,
                author: &user,
                body: "Please check the API boundary",
                payload: &Value::Null,
                reply_to: None,
                idempotency_key: Some("request-1"),
            },
        )
        .await
        .unwrap();
        let replay = append(
            &db,
            "session-1",
            NewMessage {
                kind: MessageKind::Message,
                urgency: Urgency::Attention,
                author: &user,
                body: "Please check the API boundary",
                payload: &Value::Null,
                reply_to: None,
                idempotency_key: Some("request-1"),
            },
        )
        .await
        .unwrap();
        assert_eq!(posted.id, replay.id, "idempotent replay appends once");

        create_delivery(
            &db,
            &posted.id,
            weaver_api::CHANNEL_SLACK_ORIGIN_BINDING_ID,
            "slack_thread",
            None,
        )
        .await
        .unwrap();
        finish_delivery(
            &db,
            &posted.id,
            weaver_api::CHANNEL_SLACK_ORIGIN_BINDING_ID,
            None,
            Some("1786.1234"),
        )
        .await
        .unwrap();
        let delivered = refresh_message(&db, &posted.id).await.unwrap();
        assert_eq!(delivered.deliveries.len(), 1);
        assert_eq!(delivered.deliveries[0].binding_kind, "slack_thread");
        assert_eq!(delivered.deliveries[0].target_session_id, None);
        assert_eq!(
            delivered.deliveries[0].external_id.as_deref(),
            Some("1786.1234")
        );

        let subscription = mark_read(&db, "session-1", &owner, None).await.unwrap();
        assert_eq!(subscription.mode, SubscriptionMode::Deliver.as_str());
        assert_eq!(subscription.read_seq, posted.seq);

        let custom = create_custom(
            &db,
            "/repo",
            Some(&branch.id),
            "Review room",
            "Explicit monitor",
            &owner,
        )
        .await
        .unwrap();
        set_subscription(&db, &custom.id, &owner, SubscriptionMode::Deliver)
            .await
            .unwrap();
        assert_eq!(
            delivery_targets(&db, &custom.id).await.unwrap(),
            vec!["session-1"],
            "custom channels deliver only after an explicit session subscription"
        );

        session::delete(&db, "session-1").await.unwrap();
        branch_mod::delete(&db, &branch.id).await.unwrap();
        assert!(
            get(&db, &custom.id, &owner).await.unwrap().is_some(),
            "a custom channel outlives its creator's session branch"
        );
    }

    /// A child result queues a notice in the parent's prompt queue; once the
    /// parent's read marker passes the result, that notice is stale and
    /// retracts so the next turn boundary does not deliver a pointer to
    /// something already consumed.
    #[tokio::test]
    async fn reading_past_a_child_result_retracts_the_queued_notice() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let parent_branch = branch_mod::upsert(&db, "/repo", "weaver/notice-parent", "main")
            .await
            .unwrap();
        let child_branch = branch_mod::upsert(&db, "/repo", "weaver/notice-child", "main")
            .await
            .unwrap();
        session::insert(&db, &new_session("notice-parent", &parent_branch.id))
            .await
            .unwrap();
        session::insert(&db, &new_session("notice-child", &child_branch.id))
            .await
            .unwrap();
        sqlx::query(
            "UPDATE sessions SET parent_session_id = 'notice-parent' WHERE id = 'notice-child'",
        )
        .execute(&db)
        .await
        .unwrap();

        let child = Subject::new(SubjectKind::Session, "notice-child");
        let parent = Subject::new(SubjectKind::Session, "notice-parent");
        let system = Subject::new(SubjectKind::System, "loom");
        let queue_notice = |body: String| {
            let db = &db;
            async move {
                session::append_pending_prompt(db, "notice-parent", "say:current work")
                    .await
                    .unwrap();
                session::append_pending_prompt(db, "notice-parent", &body)
                    .await
                    .unwrap();
            }
        };
        let pending = || async {
            session::read_pending_prompt(&db, "notice-parent")
                .await
                .unwrap()
        };

        // Child posts a result; the server's notification would append the
        // linked notice to the parent's channel and queue its delivery.
        let result = append(
            &db,
            "notice-child",
            NewMessage {
                kind: MessageKind::Result,
                urgency: Urgency::Normal,
                author: &child,
                body: "done",
                payload: &Value::Null,
                reply_to: None,
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
        let notice_body = format!(
            "Child session notice-child posted result {}. Read it with \
             `loom channels read --channel notice-child --kinds result`.",
            result.id
        );
        append(
            &db,
            "notice-parent",
            NewMessage {
                kind: MessageKind::Message,
                urgency: Urgency::Normal,
                author: &system,
                body: &notice_body,
                payload: &serde_json::json!({
                    "child_session_id": "notice-child",
                    "source_channel_id": "notice-child",
                    "source_message_id": result.id,
                }),
                reply_to: None,
                idempotency_key: Some(&child_result_notice_key(&result.id)),
            },
        )
        .await
        .unwrap();
        queue_notice(notice_body.clone()).await;
        assert_eq!(
            pending().await,
            format!("say:current work\n\n{notice_body}"),
            "the notice queues behind the parent's live turn"
        );

        // Another reader — even an ancestor further up — does not consume the
        // parent's copy: the notice stays queued.
        let user = Subject::new(SubjectKind::User, "alice");
        mark_read(&db, "notice-child", &user, None).await.unwrap();
        assert!(
            pending().await.contains("posted result"),
            "a non-parent reader does not retract the parent's notice"
        );

        // A marker that has not reached the result changes nothing.
        mark_read(&db, "notice-child", &parent, Some(result.seq - 1))
            .await
            .unwrap();
        assert!(pending().await.contains("posted result"));

        // The parent's own read past the result retracts exactly the notice.
        mark_read(&db, "notice-child", &parent, Some(result.seq))
            .await
            .unwrap();
        assert_eq!(
            pending().await,
            "say:current work",
            "the consumed result's notice retracts, the live prompt stays"
        );

        // Regression: two queued notices must retract independently. Notice
        // bodies are unique per result, so consuming the first result strips
        // only its own notice — not a second result's queued alongside it.
        let result2 = append(
            &db,
            "notice-child",
            NewMessage {
                kind: MessageKind::Result,
                urgency: Urgency::Normal,
                author: &child,
                body: "done again",
                payload: &Value::Null,
                reply_to: None,
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
        let notice2_body = format!(
            "Child session notice-child posted result {}. Read it with \
             `loom channels read --channel notice-child --kinds result`.",
            result2.id
        );
        append(
            &db,
            "notice-parent",
            NewMessage {
                kind: MessageKind::Message,
                urgency: Urgency::Normal,
                author: &system,
                body: &notice2_body,
                payload: &serde_json::json!({
                    "child_session_id": "notice-child",
                    "source_channel_id": "notice-child",
                    "source_message_id": result2.id,
                }),
                reply_to: None,
                idempotency_key: Some(&child_result_notice_key(&result2.id)),
            },
        )
        .await
        .unwrap();
        session::append_pending_prompt(&db, "notice-parent", &notice_body)
            .await
            .unwrap();
        session::append_pending_prompt(&db, "notice-parent", &notice2_body)
            .await
            .unwrap();
        mark_read(&db, "notice-child", &parent, Some(result.seq))
            .await
            .unwrap();
        assert_eq!(
            pending().await,
            format!("say:current work\n\n{notice2_body}"),
            "consuming the first result leaves the second result's notice queued"
        );
        mark_read(&db, "notice-child", &parent, None).await.unwrap();
        assert_eq!(
            pending().await,
            "say:current work",
            "reading through the second result retracts its notice too"
        );
    }
}
