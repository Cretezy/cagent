//! Small provider-neutral helpers for prompt-cache identity and resources.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex;

use crate::ModelRequest;

pub(crate) const NAMED_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
pub(crate) const CACHE_ADVANCE_TOKENS: u64 = 4_096;
pub(crate) const CACHE_FAILURE_COOLDOWN: Duration = Duration::from_secs(5 * 60);
pub(crate) const OPENAI_EXTENDED_RETENTION: &str = "24h";

/// Stable provider cache key shared by HTTP and WebSocket transports.
pub(crate) fn native_cache_key(request: &ModelRequest, provider: &str) -> Option<String> {
    let cache = request.prompt_cache.as_ref()?;
    let canonical_prefix = serde_json::to_vec(&json!({
        "stable_prompt": request.stable_prompt,
        "tools": request.tools,
    }))
    .ok()?;
    Some(hash_parts([
        b"cagent-prompt-cache-v1".as_slice(),
        provider.as_bytes(),
        request.model.model.as_bytes(),
        cache.key.as_bytes(),
        canonical_prefix.as_slice(),
    ]))
}

pub(crate) fn fingerprint(value: impl AsRef<[u8]>) -> String {
    hash_parts([value.as_ref()])
}

pub(crate) fn prefix_hash(value: &serde_json::Value) -> Option<String> {
    serde_json::to_vec(value).ok().map(fingerprint)
}

fn hash_parts<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct NamedCacheResource {
    pub provider: String,
    pub credential_scope_hash: String,
    pub model: String,
    pub conversation_key_hash: String,
    pub prefix_hash: String,
    pub cached_input_boundary: usize,
    pub resource_name: String,
    pub cached_token_count: u64,
    pub created_at_millis: u64,
    pub expires_at_millis: u64,
}

impl NamedCacheResource {
    pub(crate) fn identity(&self) -> NamedCacheIdentity {
        NamedCacheIdentity {
            provider: self.provider.clone(),
            credential_scope_hash: self.credential_scope_hash.clone(),
            model: self.model.clone(),
            conversation_key_hash: self.conversation_key_hash.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NamedCacheIdentity {
    pub provider: String,
    pub credential_scope_hash: String,
    pub model: String,
    pub conversation_key_hash: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PromptCacheCoordinator {
    locks: Arc<Mutex<HashMap<NamedCacheIdentity, std::sync::Weak<Mutex<()>>>>>,
}

impl PromptCacheCoordinator {
    pub(crate) async fn lock(&self, key: &NamedCacheIdentity) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(key).and_then(std::sync::Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(Mutex::new(()));
                locks.insert(key.clone(), Arc::downgrade(&lock));
                lock
            }
        };
        lock.lock_owned().await
    }

    #[cfg(test)]
    async fn retained_lock_count(&self) -> usize {
        self.locks.lock().await.len()
    }
}

pub(crate) fn cache_rate_percent(
    input: Option<u64>,
    read: Option<u64>,
    write: Option<u64>,
) -> Option<u64> {
    let inference_input = input?.saturating_sub(write.unwrap_or_default());
    let read = read?;
    (inference_input > 0).then(|| read.saturating_mul(100) / inference_input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttemptId, ModelRef, PromptCacheRequest, PromptCacheScope, RequestId};

    #[tokio::test]
    async fn coordinator_prunes_unused_identity_locks() {
        let coordinator = PromptCacheCoordinator::default();
        let identity = NamedCacheIdentity {
            provider: "google".into(),
            credential_scope_hash: "credential".into(),
            model: "gemini".into(),
            conversation_key_hash: "one".into(),
        };
        drop(coordinator.lock(&identity).await);
        let mut next = identity.clone();
        next.conversation_key_hash = "two".into();
        drop(coordinator.lock(&next).await);
        assert_eq!(coordinator.retained_lock_count().await, 1);
    }

    #[test]
    fn cache_rate_excludes_named_cache_creation() {
        assert_eq!(
            cache_rate_percent(Some(2_100), Some(1_000), Some(1_000)),
            Some(90)
        );
    }

    #[test]
    fn aggregated_cache_rate_subtracts_the_single_creation_write() {
        let mut usage = crate::protocol::SessionUsage::default();
        usage.add(&crate::ModelUsage {
            input_tokens: Some(2_000),
            cache_read_input_tokens: Some(1_000),
            cache_write_input_tokens: Some(1_000),
            ..crate::ModelUsage::default()
        });
        usage.add(&crate::ModelUsage {
            input_tokens: Some(1_000),
            cache_read_input_tokens: Some(800),
            ..crate::ModelUsage::default()
        });
        assert_eq!(usage.cache_write_input_tokens, Some(1_000));
        assert_eq!(usage.cache_rate_percent(), Some(90));
    }

    #[test]
    fn native_identity_is_stable_across_transports() {
        let request = ModelRequest {
            request_id: RequestId::new(),
            attempt_id: AttemptId::new(),
            model: ModelRef {
                provider: "openai".into(),
                model: "gpt-5.6".into(),
            },
            backend: None,
            backend_candidates: Vec::new(),
            effort: None,
            service_tier: None,
            input: Vec::new(),
            tools: Vec::new(),
            stable_prompt: Vec::new(),
            prompt_cache: Some(PromptCacheRequest {
                key: "conversation".into(),
                scope: PromptCacheScope::Conversation,
            }),
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        };
        assert_eq!(
            native_cache_key(&request, "openai"),
            native_cache_key(&request, "openai")
        );
    }
}
