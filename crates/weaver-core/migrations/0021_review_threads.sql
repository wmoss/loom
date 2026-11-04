-- Review threads and follow-up rounds: a submitted review stays open to its
-- reviewer. Comments carry reply threads — the reviewed session and any
-- operator converse, oldest first, each entry naming its speaker — and new
-- comments, new replies, and a changed overall note all queue on the
-- submitted envelope and ride the next follow-up round through the same
-- delivery machinery the initial review used.
--
-- `reviews.comment_upto` and `reviews.reply_upto` are the delivered cursors:
-- the greatest comment id (and operator reply id) whose words have reached
-- the session, advanced at delivery and never at submit, so a failed or
-- superseded round can never strand undelivered words — the next batch
-- recomputes everything past the delivered point. `reviews.reply_round`
-- counts queued rounds; each round's delivery key is the review's original
-- key with `#<round>` appended, so the conversation inbox (unique on
-- delivery_key) accepts one message per round. `reviews.delivered_summary`
-- is the note as the session last received it.
--
-- The outbox row freezes the round's payload and snapshots its window
-- (`comment_upto`/`reply_from`/`reply_upto`) and its carried summary: one
-- live batch per review, overwritten by each newer round, so the inbox sees
-- each message exactly once and a retry re-sends byte-identical text.
CREATE TABLE review_comment_replies (
    id          INTEGER PRIMARY KEY,
    comment_id  INTEGER NOT NULL REFERENCES review_comments(id) ON DELETE CASCADE,
    author_kind TEXT NOT NULL DEFAULT 'session',
    author      TEXT NOT NULL DEFAULT '',
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX idx_review_comment_replies_comment ON review_comment_replies(comment_id, id);

ALTER TABLE reviews ADD COLUMN reply_round INTEGER NOT NULL DEFAULT 0;
ALTER TABLE reviews ADD COLUMN comment_upto INTEGER NOT NULL DEFAULT 0;
ALTER TABLE reviews ADD COLUMN reply_upto INTEGER NOT NULL DEFAULT 0;
ALTER TABLE reviews ADD COLUMN delivered_summary TEXT NOT NULL DEFAULT '';

ALTER TABLE review_delivery_outbox ADD COLUMN payload TEXT;
ALTER TABLE review_delivery_outbox ADD COLUMN comment_upto INTEGER NOT NULL DEFAULT 0;
ALTER TABLE review_delivery_outbox ADD COLUMN reply_from INTEGER NOT NULL DEFAULT 0;
ALTER TABLE review_delivery_outbox ADD COLUMN reply_upto INTEGER NOT NULL DEFAULT 0;
ALTER TABLE review_delivery_outbox ADD COLUMN summary TEXT NOT NULL DEFAULT '';

-- Reviews delivered before this schema existed: their comments and notes
-- have all reached the session, so the delivered cursors start past them.
-- Undelivered ones keep the empty baselines so the next batch re-carries
-- whatever the envelope holds.
UPDATE reviews
   SET comment_upto = COALESCE(
     (SELECT MAX(c.id) FROM review_comments c WHERE c.review_id = reviews.id), 0)
 WHERE status = 'submitted'
   AND EXISTS (SELECT 1 FROM review_delivery_outbox o
               WHERE o.review_id = reviews.id AND o.state = 'delivered');

UPDATE reviews
   SET delivered_summary = summary
 WHERE status = 'submitted'
   AND EXISTS (SELECT 1 FROM review_delivery_outbox o
               WHERE o.review_id = reviews.id AND o.state = 'delivered');
