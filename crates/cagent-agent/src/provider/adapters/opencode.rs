use std::path::Path;

use tokio_util::sync::CancellationToken;

use crate::{
    AuthState, ModelBackend, ModelCatalog, ModelDescriptor, ModelRequest, Provider,
    ProviderDescriptor, ProviderError, ProviderFuture, ProviderStream,
};

use super::{GeminiProvider, OpenAiProvider};

/// OpenCode Zen exposes both Responses and Gemini-compatible models under one key.
#[derive(Clone, Debug)]
pub struct OpenCodeProvider {
    responses: OpenAiProvider,
    gemini: GeminiProvider,
    descriptor: ProviderDescriptor,
}

impl OpenCodeProvider {
    #[must_use]
    pub fn new() -> Self {
        let responses = OpenAiProvider::open_code();
        let gemini = GeminiProvider::open_code();
        let mut descriptor = responses.descriptor().clone();
        descriptor
            .supported_model_backends
            .push(ModelBackend::Gemini);
        Self {
            responses,
            gemini,
            descriptor,
        }
    }

    #[must_use]
    pub fn with_credential_dir(mut self, data_dir: &Path) -> Self {
        self.responses = self.responses.with_credential_dir(data_dir);
        self.gemini = self.gemini.with_credential_dir(data_dir);
        self
    }

    #[must_use]
    pub fn with_environment_variable(mut self, variable: impl Into<String>) -> Self {
        let variable = variable.into();
        self.responses = self.responses.with_environment_variable(variable.clone());
        self.gemini = self.gemini.with_environment_variable(variable);
        self
    }

    #[must_use]
    pub fn with_endpoint(mut self, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        self.responses = self.responses.with_endpoint(base_url.clone());
        self.gemini = self.gemini.with_endpoint(base_url);
        self
    }

    fn selected(&self, request: &ModelRequest) -> &dyn Provider {
        if request.backend == Some(ModelBackend::Gemini) {
            &self.gemini
        } else {
            &self.responses
        }
    }
}

impl Default for OpenCodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for OpenCodeProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }
    fn auth_state(&self) -> ProviderFuture<'_, Result<AuthState, ProviderError>> {
        self.responses.auth_state()
    }
    fn discover_models(&self) -> Option<ProviderFuture<'_, Result<ModelCatalog, ProviderError>>> {
        self.responses.discover_models()
    }
    fn model_backends(&self, model: &ModelDescriptor) -> Vec<ModelBackend> {
        model
            .backend
            .or(self.descriptor.default_model_backend)
            .into_iter()
            .collect()
    }
    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<ProviderStream, ProviderError>> {
        self.selected(&request).stream(request, cancellation)
    }
    fn set_api_key(&self, key: String) -> ProviderFuture<'_, Result<(), ProviderError>> {
        self.responses.set_api_key(key)
    }
    fn remove_api_key(&self) -> ProviderFuture<'_, Result<(), ProviderError>> {
        self.responses.remove_api_key()
    }
    fn has_managed_api_key(&self) -> ProviderFuture<'_, bool> {
        self.responses.has_managed_api_key()
    }
}
