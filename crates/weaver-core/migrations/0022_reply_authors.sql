-- Comment reply threads hold a conversation, not one message: the reviewed
-- session and any operator may both append. Each entry records who spoke —
-- `author_kind` is 'session' or 'operator'; `author` carries the operator's
-- username and stays empty for the session, whose identity is the review's.
-- Existing rows are all session replies from before operators could join.
ALTER TABLE review_comment_replies ADD COLUMN author_kind TEXT NOT NULL DEFAULT 'session';
ALTER TABLE review_comment_replies ADD COLUMN author TEXT NOT NULL DEFAULT '';
-- The reply's session was always the review's own — derivable through the
-- comment — so the denormalized copy goes now that entries carry an author.
ALTER TABLE review_comment_replies DROP COLUMN session_id;
