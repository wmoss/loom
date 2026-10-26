-- The overall note becomes part of the follow-up loop: a review's summary
-- is delivered state, tracked the same way comments and replies are. The
-- review gains `delivered_summary` — the note as the session last received
-- it — and each outbox batch snapshots the summary it carries, so a note
-- edited after submission (or never delivered at all) rides the next round
-- instead of being stranded or silently dropped.
ALTER TABLE reviews ADD COLUMN delivered_summary TEXT NOT NULL DEFAULT '';

-- The batch's carried note; on delivery success it becomes the review's
-- `delivered_summary`.
ALTER TABLE review_delivery_outbox ADD COLUMN summary TEXT NOT NULL DEFAULT '';

-- Existing delivered reviews: their notes, like their comments, have
-- reached the session. Undelivered ones keep the empty baseline so the
-- next batch re-carries whatever the summary holds.
UPDATE reviews
   SET delivered_summary = summary
 WHERE status = 'submitted'
   AND EXISTS (SELECT 1 FROM review_delivery_outbox o
               WHERE o.review_id = reviews.id AND o.state = 'delivered');
