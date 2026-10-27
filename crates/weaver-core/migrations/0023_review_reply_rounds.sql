-- Operator replies to a submitted review queue on the review itself and go
-- back to the reviewed session when the reviewer submits again, through the
-- same delivery machinery the initial comments rode.
--
-- `reviews.reply_round` counts queued follow-up rounds; each round's delivery
-- key is the review's original key with `#<round>` appended, so the
-- conversation inbox (unique on delivery_key) accepts one message per round.
-- `reviews.reply_upto` is the watermark: the greatest reply id already
-- included in a queued round, so replies still pending are exactly the
-- operator words past it. The outbox row snapshots the round's window —
-- `reply_from`/`reply_upto` — so a retry re-derives the same reply set no
-- matter what was appended in the meantime; the rest waits for the next
-- round.
ALTER TABLE reviews ADD COLUMN reply_round INTEGER NOT NULL DEFAULT 0;
ALTER TABLE reviews ADD COLUMN reply_upto INTEGER NOT NULL DEFAULT 0;
ALTER TABLE review_delivery_outbox ADD COLUMN reply_from INTEGER NOT NULL DEFAULT 0;
ALTER TABLE review_delivery_outbox ADD COLUMN reply_upto INTEGER NOT NULL DEFAULT 0;
