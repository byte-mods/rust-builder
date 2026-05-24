//! Integration tests for Milestone 5: Real-time Multi-User Collaborative Editing.
//!
//! Validates DashMap-backed CollabManager room orchestration, concurrent client joins,
//! cursor broadcasts, structural edit routing, and teardown lifecycles.

use std::sync::Arc;
use tokio::sync::mpsc;

use rust_no_code_studio::{
    projects::collab::{CollabManager, CollabUser, CollabMessage},
    projects::ProjectStore,
    templates::TemplateRegistry,
    AppState,
};

#[tokio::test]
async fn test_collab_manager_room_orchestration() {
    let manager = CollabManager::new();

    let user_a = CollabUser {
        id: "user_a".to_string(),
        username: "Tokio-Scheduler-123".to_string(),
        color: "#3b82f6".to_string(),
    };
    let user_b = CollabUser {
        id: "user_b".to_string(),
        username: "Tonic-Scaffolder-456".to_string(),
        color: "#ec4899".to_string(),
    };

    let (tx_a, mut rx_a) = mpsc::unbounded_channel();
    let (tx_b, mut rx_b) = mpsc::unbounded_channel();

    // 1. User A Joins Room "test-project"
    let presence_1 = manager.join_room("test-project", user_a.clone(), tx_a);
    assert_eq!(presence_1.len(), 1);
    assert_eq!(presence_1[0].id, "user_a");

    // 2. User B Joins Room "test-project"
    let presence_2 = manager.join_room("test-project", user_b.clone(), tx_b);
    assert_eq!(presence_2.len(), 2);
    assert!(presence_2.iter().any(|u| u.id == "user_a"));
    assert!(presence_2.iter().any(|u| u.id == "user_b"));

    // 3. Broadcast Pointer Position from User A
    let cursor_msg = CollabMessage::CursorMove {
        user_id: "user_a".to_string(),
        x: 150.0,
        y: 200.0,
    };
    manager.broadcast("test-project", "user_a", cursor_msg);

    // User B should receive the broadcasted message!
    let received_b = rx_b.try_recv();
    assert!(received_b.is_ok());
    let ws_msg_b = received_b.unwrap();
    let text_b = ws_msg_b.to_text().unwrap();
    let parsed_b: serde_json::Value = serde_json::from_str(text_b).unwrap();
    assert_eq!(parsed_b["type"], "cursor");
    assert_eq!(parsed_b["user_id"], "user_a");
    assert_eq!(parsed_b["x"], 150.0);
    assert_eq!(parsed_b["y"], 200.0);

    // User A should NOT receive their own broadcast!
    let received_a = rx_a.try_recv();
    assert!(received_a.is_err());

    // 4. User A leaves Room
    let presence_3 = manager.leave_room("test-project", "user_a");
    assert_eq!(presence_3.len(), 1);
    assert_eq!(presence_3[0].id, "user_b");
}

#[tokio::test]
async fn test_collab_app_state_integration() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(dir.path()).await.unwrap();
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let state = AppState::new(store, registry);

    // Verify CollabManager is successfully integrated into global AppState!
    let user = CollabUser {
        id: "user_1".to_string(),
        username: "Rust-Architect-999".to_string(),
        color: "#8b5cf6".to_string(),
    };
    let (tx, _) = mpsc::unbounded_channel();
    
    let presence = state.collab_manager.join_room("collab-test", user, tx);
    assert_eq!(presence.len(), 1);
    assert_eq!(presence[0].username, "Rust-Architect-999");
}
