CREATE TABLE IF NOT EXISTS conversations (
  id TEXT PRIMARY KEY,
  project_dir TEXT NOT NULL,
  cwd TEXT NOT NULL,
  worktree_json TEXT,
  workspace TEXT NOT NULL,
  title TEXT,
  title_source TEXT,
  title_generation_state TEXT NOT NULL DEFAULT 'complete',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  active_node_id TEXT,
  active_agent TEXT NOT NULL,
  active_mode TEXT NOT NULL,
  normal_provider TEXT,
  normal_model TEXT,
  normal_effort TEXT,
  normal_model_source TEXT NOT NULL DEFAULT 'inherited',
  plan_provider TEXT,
  plan_model TEXT,
  plan_effort TEXT,
  plan_model_source TEXT NOT NULL DEFAULT 'inherited',
  archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0, 1)),
  favourite INTEGER NOT NULL DEFAULT 0 CHECK(favourite IN (0, 1)),
  index_revision INTEGER NOT NULL DEFAULT 1 CHECK(index_revision >= 1),
  revision INTEGER NOT NULL DEFAULT 0 CHECK(revision >= 0)
);

-- Newer sessions persist one selection per mode. The normal/plan columns above
-- remain as a compatibility fallback for databases created by older builds.
CREATE TABLE IF NOT EXISTS mode_model_selections (
  conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  mode TEXT NOT NULL,
  provider TEXT,
  model TEXT,
  effort TEXT,
  source TEXT NOT NULL DEFAULT 'inherited',
  PRIMARY KEY (conversation_id, mode)
);

CREATE TABLE IF NOT EXISTS nodes (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(id),
  sequence INTEGER,
  parent_id TEXT REFERENCES nodes(id),
  turn_id TEXT,
  owner_id TEXT REFERENCES nodes(id),
  request_index INTEGER,
  request_id TEXT,
  attempt_id TEXT,
  context_window_tokens INTEGER,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  role TEXT,
  summary TEXT,
  content_json TEXT NOT NULL,
  provider TEXT,
  model TEXT,
  effort TEXT,
  agent TEXT,
  mode TEXT,
  created_at TEXT NOT NULL,
  completed_at TEXT
);

CREATE INDEX IF NOT EXISTS nodes_parent ON nodes(conversation_id, parent_id);
CREATE UNIQUE INDEX IF NOT EXISTS nodes_sequence ON nodes(conversation_id, sequence);
CREATE INDEX IF NOT EXISTS nodes_turn ON nodes(conversation_id, turn_id);
CREATE INDEX IF NOT EXISTS nodes_owner ON nodes(conversation_id, owner_id);
CREATE INDEX IF NOT EXISTS nodes_created ON nodes(conversation_id, created_at);
CREATE UNIQUE INDEX IF NOT EXISTS nodes_one_root ON nodes(conversation_id) WHERE kind = 'conversation_root';
CREATE UNIQUE INDEX IF NOT EXISTS nodes_attempt_id ON nodes(attempt_id) WHERE attempt_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS nodes_request_id ON nodes(conversation_id, request_id) WHERE request_id IS NOT NULL;
-- Healthy resume checks this rare crash-only state. Indexing the extracted
-- status avoids reparsing every retained tool result on every open.
CREATE INDEX IF NOT EXISTS nodes_bash_recovery_status
ON nodes(json_extract(content_json, '$.output.status'))
WHERE kind = 'tool_result' AND json_extract(content_json, '$.name') = 'bash';

CREATE TRIGGER IF NOT EXISTS nodes_validate_insert
BEFORE INSERT ON nodes
BEGIN
  SELECT CASE
    WHEN NEW.kind = 'conversation_root' AND (NEW.parent_id IS NOT NULL OR NEW.turn_id IS NOT NULL)
    THEN RAISE(ABORT, 'conversation root cannot have a parent or turn')
    WHEN NEW.kind <> 'conversation_root' AND NEW.parent_id IS NULL
    THEN RAISE(ABORT, 'non-root node requires a parent')
    WHEN NEW.kind <> 'conversation_root' AND NEW.turn_id IS NULL
    THEN RAISE(ABORT, 'non-root node requires a turn')
    WHEN NEW.request_index IS NOT NULL AND NEW.request_index < 0
    THEN RAISE(ABORT, 'request index cannot be negative')
    WHEN NEW.parent_id IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM nodes AS parent
      WHERE parent.id = NEW.parent_id AND parent.conversation_id = NEW.conversation_id
    ) THEN RAISE(ABORT, 'parent must belong to the same conversation')
    WHEN NEW.owner_id IS NOT NULL AND NOT EXISTS (
      SELECT 1 FROM nodes AS owner
      WHERE owner.id = NEW.owner_id AND owner.conversation_id = NEW.conversation_id
    ) THEN RAISE(ABORT, 'owner must belong to the same conversation')
  END;
END;

-- SQLite cannot use a subquery in a column default. Assign the durable,
-- per-conversation order inside the same transaction as the insert instead.
CREATE TRIGGER IF NOT EXISTS nodes_assign_sequence
AFTER INSERT ON nodes WHEN NEW.sequence IS NULL
BEGIN
  UPDATE nodes
  SET sequence = (
    SELECT COALESCE(MAX(sequence), 0) + 1
    FROM nodes
    WHERE conversation_id = NEW.conversation_id AND id <> NEW.id
  )
  WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS nodes_snapshot_profile
AFTER INSERT ON nodes
BEGIN
  UPDATE nodes SET
    agent = COALESCE(agent, (SELECT active_agent FROM conversations WHERE id = NEW.conversation_id)),
    mode = COALESCE(mode, (SELECT active_mode FROM conversations WHERE id = NEW.conversation_id)),
    provider = COALESCE(provider, (
      SELECT COALESCE(s.provider, c.normal_provider)
      FROM conversations c LEFT JOIN mode_model_selections s
        ON s.conversation_id = c.id AND s.mode = c.active_mode
      WHERE c.id = NEW.conversation_id
    )),
    model = COALESCE(model, (
      SELECT COALESCE(s.model, c.normal_model)
      FROM conversations c LEFT JOIN mode_model_selections s
        ON s.conversation_id = c.id AND s.mode = c.active_mode
      WHERE c.id = NEW.conversation_id
    )),
    effort = COALESCE(effort, (
      SELECT COALESCE(s.effort, c.normal_effort)
      FROM conversations c LEFT JOIN mode_model_selections s
        ON s.conversation_id = c.id AND s.mode = c.active_mode
      WHERE c.id = NEW.conversation_id
    ))
  WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS nodes_preserve_identity_and_ancestry
BEFORE UPDATE OF id, conversation_id, sequence, parent_id, turn_id, owner_id, request_index, kind, role, created_at ON nodes
BEGIN
  SELECT CASE
    WHEN NEW.id != OLD.id
      OR NEW.conversation_id != OLD.conversation_id
      OR (OLD.sequence IS NOT NULL AND NEW.sequence != OLD.sequence)
      OR NEW.parent_id IS NOT OLD.parent_id
      OR NEW.turn_id IS NOT OLD.turn_id
      OR NEW.owner_id IS NOT OLD.owner_id
      OR NEW.request_index IS NOT OLD.request_index
      OR NEW.role IS NOT OLD.role
      OR NEW.created_at != OLD.created_at
      OR NEW.kind != OLD.kind
    THEN RAISE(ABORT, 'node identity and ancestry are immutable')
  END;
END;

CREATE TRIGGER IF NOT EXISTS nodes_are_append_only
BEFORE DELETE ON nodes
BEGIN
  SELECT RAISE(ABORT, 'conversation nodes are append-only');
END;

CREATE TABLE IF NOT EXISTS blobs (id TEXT PRIMARY KEY, codec TEXT NOT NULL, bytes BLOB NOT NULL);

CREATE TABLE IF NOT EXISTS model_usage (
  node_id TEXT PRIMARY KEY REFERENCES nodes(id),
  input_tokens INTEGER, non_cached_input_tokens INTEGER, cache_read_input_tokens INTEGER,
  cache_write_input_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER,
  total_tokens INTEGER, provider_usage_json TEXT, input_cost TEXT, cache_read_cost TEXT,
  cache_write_cost TEXT, output_cost TEXT, reasoning_cost TEXT, total_cost TEXT,
  currency TEXT, pricing_source TEXT, pricing_version TEXT
);

-- Optional provider acceleration state. Conversation history remains the
-- authority, so these rows contain no credentials and can be discarded at any
-- time. The assistant node anchors the snapshot to one branch.
CREATE TABLE IF NOT EXISTS response_continuations (
  assistant_node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
  conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  response_id TEXT NOT NULL,
  request_json TEXT NOT NULL,
  incorporated_input_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS response_continuations_conversation
  ON response_continuations(conversation_id, created_at DESC);

CREATE TABLE IF NOT EXISTS queued_messages (
  id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES conversations(id),
  position INTEGER NOT NULL, target TEXT NOT NULL, status TEXT NOT NULL,
  content_json TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  UNIQUE(conversation_id, position)
);
CREATE INDEX IF NOT EXISTS queued_messages_conversation ON queued_messages(conversation_id, position);

-- Canonical per-conversation selection history. It is projected into the
-- disposable global model-recents table for cross-conversation UX ranking.
CREATE TABLE IF NOT EXISTS model_recents (
  provider TEXT NOT NULL, model TEXT NOT NULL, recency INTEGER NOT NULL,
  selection_count INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(provider, model)
);
CREATE INDEX IF NOT EXISTS model_recents_rank ON model_recents(provider, recency DESC);

CREATE TABLE IF NOT EXISTS agent_runs (
  id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES conversations(id),
  parent_turn_id TEXT NOT NULL, sequence INTEGER NOT NULL, profile TEXT NOT NULL,
  provider TEXT NOT NULL, model TEXT NOT NULL, effort TEXT, task TEXT NOT NULL, status TEXT NOT NULL,
  result TEXT, error TEXT, usage_json TEXT, created_at TEXT NOT NULL, started_at TEXT,
  completed_at TEXT, UNIQUE(conversation_id, parent_turn_id, sequence)
);
CREATE INDEX IF NOT EXISTS agent_runs_parent ON agent_runs(conversation_id, parent_turn_id, sequence);

CREATE TABLE IF NOT EXISTS agent_run_events (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL REFERENCES agent_runs(id),
  kind TEXT NOT NULL, content_json TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS agent_run_events_run ON agent_run_events(run_id, sequence);

CREATE TABLE IF NOT EXISTS background_terminals (
  id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES conversations(id),
  tool_call_node_id TEXT REFERENCES nodes(id), owner_agent_run_id TEXT REFERENCES agent_runs(id),
  read_safe INTEGER, detached INTEGER NOT NULL DEFAULT 0 CHECK(detached IN (0, 1)),
  anchor_node_id TEXT REFERENCES nodes(id),
  command TEXT NOT NULL, status TEXT NOT NULL,
  created_at TEXT NOT NULL, started_at TEXT NOT NULL, completed_at TEXT, exit_code INTEGER,
  output_base INTEGER NOT NULL DEFAULT 0, output_cursor INTEGER NOT NULL DEFAULT 0,
  output_bytes INTEGER NOT NULL DEFAULT 0, discarded_bytes INTEGER NOT NULL DEFAULT 0,
  truncated INTEGER NOT NULL DEFAULT 0, preview_output TEXT NOT NULL DEFAULT '',
  output BLOB NOT NULL DEFAULT X'',
  ansi_output BLOB NOT NULL DEFAULT X''
);
CREATE INDEX IF NOT EXISTS background_terminals_conversation ON background_terminals(conversation_id, created_at, id);

CREATE TABLE IF NOT EXISTS completion_mailbox (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id TEXT NOT NULL REFERENCES conversations(id), work_kind TEXT NOT NULL,
  work_id TEXT NOT NULL, envelope_json TEXT NOT NULL, completed_at TEXT NOT NULL,
  claimed_at TEXT, claim_kind TEXT, delivered_at TEXT, notice_node_id TEXT REFERENCES nodes(id),
  UNIQUE(work_kind, work_id)
);
CREATE INDEX IF NOT EXISTS completion_mailbox_pending ON completion_mailbox(conversation_id, sequence)
  WHERE claimed_at IS NULL AND delivered_at IS NULL;

CREATE TABLE IF NOT EXISTS composer_history (
  id TEXT PRIMARY KEY NOT NULL,
  conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  node_id TEXT UNIQUE REFERENCES nodes(id) ON DELETE CASCADE,
  entry_kind TEXT NOT NULL CHECK(entry_kind IN ('user_message', 'slash_command')),
  input_kind TEXT NOT NULL DEFAULT 'prompt' CHECK(input_kind IN ('prompt', 'bash')),
  text TEXT NOT NULL, attachment_specs_json TEXT NOT NULL,
  images_json TEXT NOT NULL DEFAULT '[]', image_chips_json TEXT NOT NULL DEFAULT '[]',
  slash_command_user_text TEXT,
  publish_to_global INTEGER NOT NULL DEFAULT 1 CHECK(publish_to_global IN (0, 1)),
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS conversation_permissions (
  id TEXT PRIMARY KEY NOT NULL,
  rule_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS composer_history_conversation_order ON composer_history(conversation_id, id);
CREATE INDEX IF NOT EXISTS composer_history_kind_order ON composer_history(entry_kind, id);

CREATE TRIGGER IF NOT EXISTS conversations_bump_index_revision
AFTER UPDATE OF workspace, title, updated_at, active_node_id, active_agent, active_mode,
                normal_provider, normal_model ON conversations
WHEN NEW.index_revision = OLD.index_revision
BEGIN
  UPDATE conversations SET index_revision = OLD.index_revision + 1 WHERE id = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS nodes_insert_bump_index_revision
AFTER INSERT ON nodes WHEN NEW.kind != 'conversation_root'
BEGIN
  UPDATE conversations SET index_revision = index_revision + 1 WHERE id = NEW.conversation_id;
END;
CREATE TRIGGER IF NOT EXISTS nodes_update_bump_index_revision
AFTER UPDATE OF parent_id, status, content_json, agent, mode ON nodes
BEGIN
  UPDATE conversations SET index_revision = index_revision + 1 WHERE id = NEW.conversation_id;
END;
CREATE TRIGGER IF NOT EXISTS nodes_delete_bump_index_revision
AFTER DELETE ON nodes WHEN OLD.kind != 'conversation_root'
BEGIN
  UPDATE conversations SET index_revision = index_revision + 1 WHERE id = OLD.conversation_id;
END;
CREATE TRIGGER IF NOT EXISTS composer_insert_bump_index_revision
AFTER INSERT ON composer_history
BEGIN
  UPDATE conversations SET index_revision = index_revision + 1 WHERE id = NEW.conversation_id;
END;
CREATE TRIGGER IF NOT EXISTS mode_selection_insert_bump_index_revision
AFTER INSERT ON mode_model_selections
BEGIN
  UPDATE conversations SET index_revision = index_revision + 1 WHERE id = NEW.conversation_id;
END;
CREATE TRIGGER IF NOT EXISTS mode_selection_update_bump_index_revision
AFTER UPDATE OF provider, model, effort, source ON mode_model_selections
BEGIN
  UPDATE conversations SET index_revision = index_revision + 1 WHERE id = NEW.conversation_id;
END;
CREATE TRIGGER IF NOT EXISTS mode_selection_delete_bump_index_revision
AFTER DELETE ON mode_model_selections
BEGIN
  UPDATE conversations SET index_revision = index_revision + 1 WHERE id = OLD.conversation_id;
END;
CREATE TRIGGER IF NOT EXISTS model_recent_insert_bump_index_revision
AFTER INSERT ON model_recents
BEGIN
  UPDATE conversations SET index_revision = index_revision + 1;
END;
CREATE TRIGGER IF NOT EXISTS model_recent_update_bump_index_revision
AFTER UPDATE OF recency, selection_count ON model_recents
BEGIN
  UPDATE conversations SET index_revision = index_revision + 1;
END;
CREATE TRIGGER IF NOT EXISTS model_recent_delete_bump_index_revision
AFTER DELETE ON model_recents
BEGIN
  UPDATE conversations SET index_revision = index_revision + 1;
END;
