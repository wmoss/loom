-- Agent replies to review comments: the reviewed session's own responses,
-- appended only after the review is submitted. Replies never mutate the
-- comment they answer; resolving stays an operator action.
CREATE TABLE review_comment_replies (
    id         INTEGER PRIMARY KEY,
    comment_id INTEGER NOT NULL REFERENCES review_comments(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    body       TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX idx_review_comment_replies_comment ON review_comment_replies(comment_id, id);
