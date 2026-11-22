-- Conversation state; appended as the next migration by the schema owner.
ALTER TABLE review_threads ADD COLUMN subject TEXT NOT NULL DEFAULT 'line' CHECK (subject IN ('line', 'file'));
ALTER TABLE review_threads ADD COLUMN finding_id TEXT;
ALTER TABLE review_threads ADD COLUMN human_read_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE review_threads ADD COLUMN agent_request_comment_id TEXT;
ALTER TABLE review_threads ADD COLUMN agent_state TEXT NOT NULL DEFAULT 'idle' CHECK (agent_state IN ('idle', 'no_agent', 'waiting', 'responding', 'failed'));
ALTER TABLE review_threads ADD COLUMN agent_error TEXT;
ALTER TABLE review_threads ADD COLUMN agent_updated_at TEXT;
ALTER TABLE review_threads ADD COLUMN linked_thread_id TEXT;
CREATE UNIQUE INDEX idx_threads_finding ON review_threads(pr_id, finding_id) WHERE source = 'local' AND finding_id IS NOT NULL;
UPDATE review_threads SET human_read_seq = COALESCE((SELECT MAX(seq) FROM review_comments WHERE thread_id = review_threads.id), 0);

CREATE TABLE review_comment_drafts (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES review_threads(id),
    body TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('draft', 'published')),
    source_comment_id TEXT REFERENCES review_comments(id),
    reserved_attempt_id TEXT REFERENCES publication_attempts(id),
    github_comment_id INTEGER,
    github_review_id INTEGER,
    published_head_sha TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    published_at TEXT
);
CREATE UNIQUE INDEX idx_drafts_open_thread ON review_comment_drafts(thread_id) WHERE state = 'draft';
CREATE INDEX idx_drafts_github_id ON review_comment_drafts(github_comment_id);
-- Only a human-authored root on a local thread was an outgoing draft in the old model.
-- Preserve its ID and reservation; every other old draft is private, never publishable.
INSERT INTO review_comment_drafts (id, thread_id, body, state, reserved_attempt_id, created_at, updated_at)
SELECT c.id, c.thread_id, c.body, 'draft', r.attempt_id, c.created_at, c.updated_at
FROM review_comments c JOIN review_threads t ON t.id = c.thread_id
LEFT JOIN publication_reservations r ON r.comment_id = c.id
WHERE t.source = 'local' AND c.author_kind = 'human' AND c.publish_state = 'draft'
  AND trim(c.body) != '' AND c.in_reply_to_id IS NULL
  AND c.seq = (SELECT MIN(root.seq) FROM review_comments root WHERE root.thread_id = c.thread_id);
-- Preserve already published local comments as history; never infer a match by text.
INSERT INTO review_comment_drafts (id, thread_id, body, state, github_comment_id, created_at, updated_at, published_at)
SELECT c.id, c.thread_id, c.body, 'published', c.github_comment_id, c.created_at, c.updated_at, c.updated_at
FROM review_comments c JOIN review_threads t ON t.id = c.thread_id
WHERE t.source = 'local' AND c.author_kind = 'human' AND c.publish_state = 'published';
UPDATE review_comments SET publish_state = 'private' WHERE publish_state = 'draft';
