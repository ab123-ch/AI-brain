use std::future::Future;

use brain_memory::conversation_memory::ConversationMemoryScope;

tokio::task_local! {
    static CONVERSATION_MEMORY_SCOPE: ConversationMemoryScope;
}

pub async fn with_conversation_memory_scope<F>(
    scope: &ConversationMemoryScope,
    future: F,
) -> F::Output
where
    F: Future,
{
    CONVERSATION_MEMORY_SCOPE.scope(scope.clone(), future).await
}

pub fn current_conversation_memory_scope() -> Option<ConversationMemoryScope> {
    CONVERSATION_MEMORY_SCOPE.try_with(Clone::clone).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scope_is_visible_only_inside_the_web_query_future() {
        let scope = ConversationMemoryScope::new("chat_1", "generation_1").unwrap();
        assert!(current_conversation_memory_scope().is_none());
        let captured = with_conversation_memory_scope(&scope, async {
            current_conversation_memory_scope().unwrap()
        })
        .await;
        assert_eq!(captured, scope);
        assert!(current_conversation_memory_scope().is_none());
    }
}
