CREATE TABLE IF NOT EXISTS model_catalog_cache (
  provider TEXT PRIMARY KEY NOT NULL,
  catalog_json TEXT NOT NULL,
  fetched_at_millis INTEGER NOT NULL CHECK(fetched_at_millis >= 0),
  version TEXT
);

-- Disposable handles for provider-owned prompt cache resources. Prompt text
-- and credentials are deliberately never stored here.
CREATE TABLE IF NOT EXISTS prompt_cache_resources (
  provider TEXT NOT NULL,
  credential_scope_hash TEXT NOT NULL,
  model TEXT NOT NULL,
  conversation_key_hash TEXT NOT NULL,
  prefix_hash TEXT NOT NULL,
  cached_input_boundary INTEGER NOT NULL CHECK(cached_input_boundary >= 0),
  resource_name TEXT NOT NULL,
  cached_token_count INTEGER NOT NULL CHECK(cached_token_count >= 0),
  created_at_millis INTEGER NOT NULL CHECK(created_at_millis >= 0),
  expires_at_millis INTEGER NOT NULL CHECK(expires_at_millis >= 0),
  PRIMARY KEY(provider, credential_scope_hash, model, conversation_key_hash)
);

CREATE TABLE IF NOT EXISTS model_recents (
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  recency INTEGER NOT NULL,
  selection_count INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY(provider, model)
);
CREATE INDEX IF NOT EXISTS model_recents_rank
  ON model_recents(provider, recency DESC);

CREATE TABLE IF NOT EXISTS conversation_index (
  conversation_id TEXT PRIMARY KEY NOT NULL,
  workspace TEXT NOT NULL,
  title TEXT NOT NULL,
  title_search TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  active_node_id TEXT NOT NULL,
  active_status TEXT NOT NULL,
  active_branch_preview TEXT NOT NULL,
  active_branch_message_count INTEGER NOT NULL CHECK(active_branch_message_count >= 0),
  agent TEXT NOT NULL,
  mode TEXT NOT NULL,
  provider TEXT,
  model TEXT,
  archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0, 1)),
  favourite INTEGER NOT NULL DEFAULT 0 CHECK(favourite IN (0, 1)),
  source_revision INTEGER NOT NULL CHECK(source_revision >= 0)
);
CREATE INDEX IF NOT EXISTS conversation_index_workspace_updated
  ON conversation_index(workspace, updated_at DESC, conversation_id DESC);
CREATE INDEX IF NOT EXISTS conversation_index_updated
  ON conversation_index(updated_at DESC, conversation_id DESC);

-- Rebuildable cross-conversation usage. Canonical records remain in each
-- conversation database; the reset watermark prevents reconciliation from
-- restoring usage that the user explicitly cleared.
CREATE TABLE IF NOT EXISTS usage_projection (
  conversation_id TEXT NOT NULL,
  node_id TEXT NOT NULL,
  created_at_millis INTEGER NOT NULL CHECK(created_at_millis >= 0),
  project TEXT NOT NULL,
  provider TEXT,
  model TEXT,
  input_tokens INTEGER,
  non_cached_input_tokens INTEGER,
  cache_read_input_tokens INTEGER,
  cache_write_input_tokens INTEGER,
  output_tokens INTEGER,
  reasoning_tokens INTEGER,
  total_tokens INTEGER,
  input_cost TEXT,
  cache_read_cost TEXT,
  cache_write_cost TEXT,
  output_cost TEXT,
  reasoning_cost TEXT,
  total_cost TEXT,
  currency TEXT,
  pricing_source TEXT,
  pricing_version TEXT,
  PRIMARY KEY(conversation_id, node_id)
);
CREATE INDEX IF NOT EXISTS usage_projection_created
  ON usage_projection(created_at_millis);
CREATE INDEX IF NOT EXISTS usage_projection_project
  ON usage_projection(project);
CREATE INDEX IF NOT EXISTS usage_projection_model
  ON usage_projection(provider, model);

CREATE TABLE IF NOT EXISTS usage_state (
  singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
  reset_at_millis INTEGER NOT NULL CHECK(reset_at_millis >= 0)
);
INSERT OR IGNORE INTO usage_state(singleton, reset_at_millis) VALUES (1, 0);

CREATE TABLE IF NOT EXISTS conversation_search_messages (
  conversation_id TEXT NOT NULL,
  node_id TEXT NOT NULL,
  normalized_user_text TEXT NOT NULL,
  PRIMARY KEY(conversation_id, node_id)
);
CREATE INDEX IF NOT EXISTS conversation_search_text
  ON conversation_search_messages(normalized_user_text);

CREATE TABLE IF NOT EXISTS composer_recent (
  entry_id TEXT PRIMARY KEY NOT NULL,
  conversation_id TEXT NOT NULL,
  entry_kind TEXT NOT NULL CHECK(entry_kind IN ('user_message', 'slash_command')),
  input_kind TEXT NOT NULL DEFAULT 'prompt' CHECK(input_kind IN ('prompt', 'bash')),
  text TEXT NOT NULL,
  attachment_specs_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS composer_recent_order
  ON composer_recent(created_at DESC, entry_id DESC);
