-- NULL diffs mark successful historical patches whose full output is unavailable.
CREATE TABLE conversation_patch_diffs (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  diff_json TEXT
);
CREATE INDEX conversation_patch_diffs_conversation
  ON conversation_patch_diffs(conversation_id, sequence);
