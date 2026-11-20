/// Migrations up to and including this version predate `schema_migrations`;
/// the migration that creates it backfills their rows.
pub(super) const MIGRATION_TABLE_VERSION: usize = 3;

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
    // v3: analysis persistence (#18, analyze.md §12). Forward-only.
    //
    // Isolation: no table below references review_acknowledgements,
    // review_sessions, review_threads or review_comments, and nothing here can
    // express file-reviewed state. Cascades apply only to analysis output
    // (runs → occurrences/evidence/links); user dispositions have no FK to
    // findings so pruning analysis output never deletes local triage.
    "
    CREATE TABLE schema_migrations (
        version INTEGER PRIMARY KEY,
        applied_at TEXT NOT NULL
    );
    INSERT INTO schema_migrations (version, applied_at) VALUES
        (1, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        (2, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        (3, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));

    -- One analysed base/head pair. Linked to the fetched pr_snapshots row while
    -- that row is retained; analysis history outlives snapshot JSON retention.
    CREATE TABLE analysis_snapshots (
        id TEXT PRIMARY KEY,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        base_sha TEXT NOT NULL,
        head_sha TEXT NOT NULL,
        pr_snapshot_id INTEGER REFERENCES pr_snapshots(id) ON DELETE SET NULL,
        registered_at TEXT NOT NULL,
        seq INTEGER NOT NULL,
        UNIQUE (id, pr_id)
    );
    CREATE INDEX idx_analysis_snapshots_pr ON analysis_snapshots(pr_id, seq);

    -- The only source of truth for which snapshot is current for a PR.
    CREATE TABLE analysis_current_snapshots (
        pr_id TEXT PRIMARY KEY REFERENCES pull_requests(id),
        snapshot_id TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        FOREIGN KEY (snapshot_id, pr_id) REFERENCES analysis_snapshots(id, pr_id)
    );

    CREATE TABLE analysis_runs (
        id TEXT PRIMARY KEY,
        pr_id TEXT NOT NULL,
        snapshot_id TEXT NOT NULL,
        provider_id TEXT NOT NULL,
        engine_version TEXT NOT NULL,
        configuration_digest TEXT NOT NULL,
        input_digest TEXT NOT NULL,
        attempt INTEGER NOT NULL CHECK (attempt >= 0),
        mode TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN
            ('queued', 'running', 'succeeded', 'partial', 'skipped', 'cancelled', 'failed', 'stale')),
        -- Status the provider reported, kept when late output is stored as stale.
        reported_status TEXT NOT NULL,
        completeness TEXT,
        started_at TEXT,
        finished_at TEXT,
        run_json TEXT NOT NULL CHECK (json_valid(run_json)),
        revision INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        FOREIGN KEY (snapshot_id, pr_id) REFERENCES analysis_snapshots(id, pr_id) ON DELETE CASCADE
    );
    CREATE INDEX idx_analysis_runs_snapshot ON analysis_runs(snapshot_id, provider_id);
    CREATE INDEX idx_analysis_runs_status ON analysis_runs(status);
    CREATE INDEX idx_analysis_runs_pr ON analysis_runs(pr_id);

    -- First produced revision of a logical finding. finding_json is the
    -- verbatim source JSON (unknown enum values survive).
    CREATE TABLE review_findings (
        id TEXT PRIMARY KEY,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        observed_snapshot_id TEXT NOT NULL,
        schema_version INTEGER NOT NULL,
        source_kind TEXT NOT NULL,
        provider_id TEXT NOT NULL,
        assertion_kind TEXT NOT NULL,
        attention_level TEXT NOT NULL,
        source_status TEXT NOT NULL,
        file_id TEXT,
        current_path TEXT,
        github_thread_id TEXT,
        first_seen_at TEXT NOT NULL,
        last_seen_at TEXT NOT NULL,
        finding_json TEXT NOT NULL CHECK (json_valid(finding_json)),
        created_at TEXT NOT NULL
    );
    CREATE INDEX idx_findings_pr ON review_findings(pr_id);
    CREATE INDEX idx_findings_snapshot_file ON review_findings(observed_snapshot_id, file_id);
    CREATE INDEX idx_findings_source ON review_findings(source_kind, source_status);
    CREATE INDEX idx_findings_thread ON review_findings(github_thread_id);

    -- One observation of a finding by a run on a snapshot. Applicability is
    -- per observation, so a finding re-observed on a later snapshot does not
    -- inherit the earlier label (and vice versa).
    CREATE TABLE finding_occurrences (
        finding_id TEXT NOT NULL REFERENCES review_findings(id) ON DELETE CASCADE,
        run_id TEXT NOT NULL REFERENCES analysis_runs(id) ON DELETE CASCADE,
        snapshot_id TEXT NOT NULL,
        applicability TEXT NOT NULL,
        observed_at TEXT NOT NULL,
        schema_version INTEGER NOT NULL,
        PRIMARY KEY (finding_id, run_id)
    );
    CREATE INDEX idx_occurrences_snapshot ON finding_occurrences(snapshot_id, applicability);
    CREATE INDEX idx_occurrences_run ON finding_occurrences(run_id);

    -- Evidence is owned by the run that wrote it: it cannot exist without a run.
    CREATE TABLE finding_evidence (
        id TEXT PRIMARY KEY,
        owner_run_id TEXT NOT NULL REFERENCES analysis_runs(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        observed_at TEXT NOT NULL,
        evidence_json TEXT NOT NULL CHECK (json_valid(evidence_json))
    );
    CREATE INDEX idx_evidence_run ON finding_evidence(owner_run_id);

    CREATE TABLE finding_evidence_links (
        finding_id TEXT NOT NULL REFERENCES review_findings(id) ON DELETE CASCADE,
        evidence_id TEXT NOT NULL REFERENCES finding_evidence(id) ON DELETE CASCADE,
        PRIMARY KEY (finding_id, evidence_id)
    );
    CREATE INDEX idx_evidence_links_evidence ON finding_evidence_links(evidence_id);

    CREATE TABLE finding_relations (
        source_finding_id TEXT NOT NULL REFERENCES review_findings(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        target_kind TEXT NOT NULL,
        target_id TEXT NOT NULL,
        run_id TEXT NOT NULL REFERENCES analysis_runs(id) ON DELETE CASCADE,
        relation_json TEXT NOT NULL CHECK (json_valid(relation_json)),
        PRIMARY KEY (source_finding_id, kind, target_kind, target_id)
    );

    -- Several findings (from different providers) may report the same
    -- logical claim; aliases point them at one canonical finding without
    -- rewriting either source record.
    CREATE TABLE finding_aliases (
        alias_finding_id TEXT PRIMARY KEY REFERENCES review_findings(id) ON DELETE CASCADE,
        canonical_finding_id TEXT NOT NULL REFERENCES review_findings(id) ON DELETE CASCADE,
        method TEXT NOT NULL,
        created_at TEXT NOT NULL,
        CHECK (alias_finding_id != canonical_finding_id)
    );
    CREATE INDEX idx_finding_aliases_canonical ON finding_aliases(canonical_finding_id);

    -- Native imported records (check runs, annotations, statuses, ...).
    -- raw_json is stored verbatim as received; key columns are read from the
    -- raw JSON so values unknown to this build are kept, not rewritten.
    CREATE TABLE imported_source_records (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        host TEXT NOT NULL,
        repository_id TEXT NOT NULL,
        source_kind TEXT NOT NULL,
        key_kind TEXT NOT NULL,
        key_value TEXT NOT NULL,
        resource_kind TEXT NOT NULL,
        native_json TEXT NOT NULL CHECK (json_valid(native_json)),
        raw_json TEXT NOT NULL CHECK (json_valid(raw_json)),
        available INTEGER NOT NULL DEFAULT 1,
        first_seen_at TEXT NOT NULL,
        last_seen_at TEXT NOT NULL,
        UNIQUE (host, repository_id, source_kind, key_kind, key_value)
    );
    CREATE INDEX idx_imported_pr ON imported_source_records(pr_id, resource_kind);

    -- Alternative identities (GraphQL node ID, REST ID, ...) for one record.
    CREATE TABLE imported_source_aliases (
        alias_kind TEXT NOT NULL,
        alias_value TEXT NOT NULL,
        source_record_id INTEGER NOT NULL REFERENCES imported_source_records(id) ON DELETE CASCADE,
        PRIMARY KEY (alias_kind, alias_value)
    );

    -- Findings that project a native record. Many findings may share one.
    CREATE TABLE finding_source_links (
        finding_id TEXT NOT NULL REFERENCES review_findings(id) ON DELETE CASCADE,
        source_record_id INTEGER NOT NULL REFERENCES imported_source_records(id) ON DELETE CASCADE,
        PRIMARY KEY (finding_id, source_record_id)
    );

    -- Local triage overlay. Deliberately no FK to review_findings: it is kept
    -- until explicit deletion, even when analysis output is pruned. Never
    -- published to GitHub.
    CREATE TABLE finding_user_state (
        session_id TEXT NOT NULL,
        finding_id TEXT NOT NULL,
        pr_id TEXT NOT NULL REFERENCES pull_requests(id),
        disposition TEXT NOT NULL CHECK (disposition IN
            ('untouched', 'investigating', 'accepted_for_review', 'dismissed_local', 'snoozed', 'promoted_to_draft')),
        snoozed_until TEXT,
        note TEXT,
        promoted_draft_id TEXT,
        updated_at TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK (revision >= 1),
        PRIMARY KEY (session_id, finding_id)
    );
    CREATE INDEX idx_user_state_disposition ON finding_user_state(disposition);
    CREATE INDEX idx_user_state_pr ON finding_user_state(pr_id);
    ",
];
