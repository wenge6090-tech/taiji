//! Test-only support: a minimal [`rig::completion::CompletionModel`]
//! implementation used to disambiguate generic `PromptHook<M>` method calls
//! in hook unit tests (the provider models are generic over their HTTP
//! client, which is not nameable from outside rig-core).

use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, GetTokenUsage, Usage,
};
use rig::streaming::StreamingCompletionResponse;

/// Minimal completion model whose calls always fail — hook tests only
/// exercise the `PromptHook` trait methods, never actual completions.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct TestCompletionModel;

impl GetTokenUsage for TestCompletionModel {
    fn token_usage(&self) -> Usage {
        Usage::new()
    }
}

impl CompletionModel for TestCompletionModel {
    type Response = TestCompletionModel;
    type StreamingResponse = TestCompletionModel;
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        TestCompletionModel
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::ProviderError("unused".into()))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Err(CompletionError::ProviderError("unused".into()))
    }
}
