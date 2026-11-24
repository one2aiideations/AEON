use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub type ConversationId = Uuid;
pub type MessageId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: MessageId,
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Result<Self, ChatError> {
        let content = content.into();
        if content.trim().is_empty() {
            return Err(ChatError::EmptyContent);
        }
        Ok(Self {
            id: Uuid::new_v4(),
            role,
            content,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub conversation_id: ConversationId,
    pub messages: Vec<ChatMessage>,
}

impl ChatRequest {
    pub fn validate(&self) -> Result<(), ChatError> {
        if self.messages.is_empty() {
            return Err(ChatError::EmptyConversation);
        }
        if self
            .messages
            .iter()
            .any(|message| message.content.trim().is_empty())
        {
            return Err(ChatError::EmptyContent);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub conversation_id: ConversationId,
    pub message: ChatMessage,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChatError {
    #[error("chat message content cannot be empty")]
    EmptyContent,
    #[error("chat request must contain at least one message")]
    EmptyConversation,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_rejects_empty_content() {
        assert_eq!(
            ChatMessage::new(ChatRole::User, "  "),
            Err(ChatError::EmptyContent)
        );
    }

    #[test]
    fn request_requires_messages() {
        let request = ChatRequest {
            conversation_id: Uuid::new_v4(),
            messages: Vec::new(),
        };
        assert_eq!(request.validate(), Err(ChatError::EmptyConversation));
    }

    #[test]
    fn request_serializes_for_http_transport() {
        let message = ChatMessage::new(ChatRole::User, "Hello Topstar").expect("message");
        let request = ChatRequest {
            conversation_id: Uuid::new_v4(),
            messages: vec![message],
        };
        let encoded = serde_json::to_string(&request).expect("json");
        let decoded: ChatRequest = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, request);
    }
}
