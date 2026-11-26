-- Drop `reviews.acknowledged_outdated`. The open-threads review model
-- removed its only writers: staleness is now a property of what a comment
-- anchors, decided where the anchor renders, not a flag the reviewer
-- acknowledges — no operation can set the column, so the schema stops
-- advertising a staleness gate that no longer exists.
ALTER TABLE reviews DROP COLUMN acknowledged_outdated;
