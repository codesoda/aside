-- Durable per-thread sequence never decreases when comments are deleted.
ALTER TABLE review_threads ADD COLUMN last_comment_seq INTEGER NOT NULL DEFAULT 0;
UPDATE review_threads SET last_comment_seq = MAX(human_read_seq, COALESCE((SELECT MAX(seq) FROM review_comments WHERE thread_id = review_threads.id), 0));
CREATE TABLE agent_request_history (
    comment_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES review_threads(id),
    state TEXT NOT NULL CHECK (state IN ('waiting', 'responding', 'completed', 'failed')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
-- v8 tracked only its latest request. Conservatively consume every older
-- private human message so a delayed retry cannot replay a forgotten turn.
INSERT INTO agent_request_history (comment_id, thread_id, state, created_at, updated_at)
SELECT c.id, t.id,
       CASE WHEN c.id = t.agent_request_comment_id AND t.agent_state IN ('waiting', 'responding', 'failed')
            THEN t.agent_state ELSE 'completed' END,
       c.created_at, COALESCE(t.agent_updated_at, c.updated_at)
FROM review_comments c JOIN review_threads t ON t.id = c.thread_id
WHERE c.author_kind = 'human' AND c.publish_state = 'private';
CREATE INDEX idx_agent_request_thread ON agent_request_history(thread_id);
