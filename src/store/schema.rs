pub(super) const MIGRATIONS: &[&str] = &[
    // v1
    "
    CREATE TABLE repositories (
        id TEXT PRIMARY KEY,
        host TEXT NOT NULL,
        owner TEXT NOT NULL,
        name TEXT NOT NULL,
        provider_id TEXT UNIQUE,
        created_at TEXT NOT NULL
    );
    CREATE TABLE repository_aliases (
        alias TEXT PRIMARY KEY,
        repository_id TEXT NOT NULL REFERENCES repositories(id)
    );
    CREATE TABLE repository_exclusion_rules (
        id TEXT PRIMARY KEY,
        repository_id TEXT NOT NULL REFERENCES repositories(id),
        action TEXT NOT NULL CHECK (action IN ('include', 'exclude')),
        kind TEXT NOT NULL CHECK (kind IN ('file', 'directory', 'glob')),
        expression TEXT NOT NULL,
        enabled INTEGER NOT NULL DEFAULT 1,
        ord INTEGER NOT NULL,
        created_by TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE INDEX idx_rules_repo ON repository_exclusion_rules(repository_id, ord);
    CREATE TABLE pull_requests (
        id TEXT PRIMARY KEY,
        repository_id TEXT NOT NULL REFERENCES repositories(id),
        number INTEGER NOT NULL,
        title TEXT NOT NULL,
        url TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        UNIQUE (repository_id, number)
    );
    CREATE TABLE pr_snapshots (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        head_sha TEXT NOT NULL,
        base_sha TEXT NOT NULL,
        fetched_at TEXT NOT NULL,
        snapshot_json TEXT NOT NULL
    );
    CREATE TABLE snapshot_files (
        snapshot_id INTEGER NOT NULL REFERENCES pr_snapshots(id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        previous_path TEXT,
        fingerprint_version INTEGER NOT NULL,
        diff_digest TEXT NOT NULL,
        content_digest TEXT,
        PRIMARY KEY (snapshot_id, path)
    );
    CREATE TABLE review_acknowledgements (
        id TEXT PRIMARY KEY,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        path TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('reviewed', 'cleared')),
        fingerprint_version INTEGER NOT NULL,
        diff_digest TEXT NOT NULL,
        content_digest TEXT,
        head_sha TEXT NOT NULL,
        base_sha TEXT NOT NULL,
        actor TEXT NOT NULL,
        created_at TEXT NOT NULL,
        seq INTEGER NOT NULL
    );
    CREATE INDEX idx_acks_pr_path ON review_acknowledgements(pr_id, path);
    CREATE TABLE review_sessions (
        pr_id TEXT PRIMARY KEY REFERENCES pull_requests(id),
        lifecycle_state TEXT NOT NULL DEFAULT 'in-progress',
        verdict TEXT,
        summary TEXT NOT NULL DEFAULT '',
        finished_at TEXT,
        published_at TEXT,
        created_at TEXT NOT NULL,
        last_opened_at TEXT NOT NULL
    );
    CREATE TABLE review_threads (
        id TEXT PRIMARY KEY,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        source TEXT NOT NULL CHECK (source IN ('local', 'github')),
        github_thread_id TEXT UNIQUE,
        path TEXT NOT NULL,
        side TEXT NOT NULL,
        line INTEGER,
        start_line INTEGER,
        line_text TEXT,
        diff_digest TEXT,
        head_sha TEXT NOT NULL,
        anchor_state TEXT NOT NULL,
        anchor_reason TEXT,
        resolved INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE INDEX idx_threads_pr ON review_threads(pr_id);
    CREATE TABLE review_comments (
        id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL REFERENCES review_threads(id),
        author_kind TEXT NOT NULL,
        author TEXT NOT NULL,
        body TEXT NOT NULL,
        publish_state TEXT NOT NULL,
        github_comment_id INTEGER,
        github_node_id TEXT UNIQUE,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        seq INTEGER NOT NULL
    );
    CREATE INDEX idx_comments_thread ON review_comments(thread_id);
    CREATE TABLE publication_attempts (
        id TEXT PRIMARY KEY,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        plan_id TEXT NOT NULL,
        plan_json TEXT NOT NULL,
        head_sha TEXT NOT NULL,
        status TEXT NOT NULL,
        error TEXT,
        github_review_id INTEGER,
        created_at TEXT NOT NULL,
        finished_at TEXT
    );
    ",
    // v2: one durable unresolved claim per PR, including pre-migration attempts.
    "
    CREATE TABLE publication_claims (
        pr_id TEXT PRIMARY KEY REFERENCES pull_requests(id),
        attempt_id TEXT NOT NULL REFERENCES publication_attempts(id)
    );
    INSERT INTO publication_claims (pr_id, attempt_id)
    SELECT pr_id, MIN(id) FROM publication_attempts WHERE status != 'succeeded' GROUP BY pr_id;
    CREATE TABLE publication_reservations (
        comment_id TEXT PRIMARY KEY REFERENCES review_comments(id),
        attempt_id TEXT NOT NULL REFERENCES publication_attempts(id)
    );
    ",
    // v3: local drafts the human held back from publication. No row means the
    // draft is selected, so every eligible draft is published unless deselected.
    "
    CREATE TABLE publication_deselections (
        comment_id TEXT PRIMARY KEY REFERENCES review_comments(id) ON DELETE CASCADE,
        created_at TEXT NOT NULL
    );
    ",
    // v4: a local thread whose first comment was deleted while replies remained.
    // Its remaining drafts were replies, so none of them may be published as the
    // thread's inline comment.
    "
    ALTER TABLE review_threads ADD COLUMN root_deleted INTEGER NOT NULL DEFAULT 0;
    ",
];
