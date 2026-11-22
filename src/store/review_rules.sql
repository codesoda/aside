-- v7: durable PR rules and repository extension preference provenance.
CREATE TABLE review_exclusion_rules (
    id TEXT PRIMARY KEY,
    pr_id TEXT NOT NULL REFERENCES pull_requests(id),
    action TEXT NOT NULL CHECK (action IN ('include', 'exclude')),
    kind TEXT NOT NULL CHECK (kind IN ('file', 'directory', 'glob')),
    expression TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    ord INTEGER NOT NULL,
    created_by TEXT NOT NULL CHECK (created_by = 'human'),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_review_rules_pr ON review_exclusion_rules(pr_id, ord);
CREATE TABLE repository_extension_preferences (
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    category TEXT NOT NULL,
    rule_id TEXT NOT NULL UNIQUE REFERENCES repository_exclusion_rules(id) ON DELETE CASCADE,
    PRIMARY KEY (repository_id, category)
);
