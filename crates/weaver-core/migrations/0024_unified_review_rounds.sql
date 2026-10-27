-- A submitted review stays open to its reviewer: new comments queue on it
-- exactly like draft comments do, thread replies queue past the delivered
-- cursor, and one submission sends everything as a single follow-up round.
--
-- `reviews.comment_upto` is the delivered-comment cursor (the greatest
-- comment id whose body has reached the session); `reviews.reply_upto`
-- becomes the same kind of cursor for operator replies — advanced at
-- delivery, never at submit, so a failed or superseded round can never
-- strand undelivered words: the next batch recomputes everything past the
-- delivered point. The outbox row freezes the round's payload and snapshots
-- its window (`comment_upto`/`reply_upto` cutoffs): one live batch per
-- review, overwritten by each newer round, so the conversation inbox sees
-- each message exactly once and a retry re-sends byte-identical text.
ALTER TABLE reviews ADD COLUMN comment_upto INTEGER NOT NULL DEFAULT 0;
ALTER TABLE review_delivery_outbox ADD COLUMN payload TEXT;
ALTER TABLE review_delivery_outbox ADD COLUMN comment_upto INTEGER NOT NULL DEFAULT 0;

-- Existing delivered reviews: their comments have all reached the session,
-- so the new cursor starts past them. The old `reply_upto` was a
-- queued-marker; for delivered rounds that is exactly the delivered cursor.
UPDATE reviews
   SET comment_upto = COALESCE(
     (SELECT MAX(c.id) FROM review_comments c WHERE c.review_id = reviews.id), 0)
 WHERE status = 'submitted'
   AND EXISTS (SELECT 1 FROM review_delivery_outbox o
               WHERE o.review_id = reviews.id AND o.state = 'delivered');

-- Undelivered rounds at upgrade time: reopen the window they had claimed,
-- so those words ride the next batch instead of being assumed delivered.
UPDATE reviews
   SET reply_upto = COALESCE(
     (SELECT o.reply_from FROM review_delivery_outbox o WHERE o.review_id = reviews.id), 0)
 WHERE status = 'submitted'
   AND EXISTS (SELECT 1 FROM review_delivery_outbox o
               WHERE o.review_id = reviews.id AND o.state != 'delivered');
