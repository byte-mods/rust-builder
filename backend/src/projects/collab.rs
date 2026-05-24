//! Real-time visual graph collaboration manager (Milestone 5).
//!
//! Manages in-memory project session rooms, multi-client pointer replication,
//! temporary node dragging coordinates, and collaborative graph persistence.

use std::sync::Arc;
use axum::{
    extract::{Path, State, ws::{Message, WebSocket, WebSocketUpgrade}},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::AppState;

/// Active collaborator visual identity packet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollabUser {
    /// Unique connection user id.
    pub id: String,
    /// Developer visual nickname (e.g. `Tonic-Scheduler`).
    pub username: String,
    /// Active neon cursor color theme (assigned on load).
    pub color: String,
}

/// Collaborative visual synchronization event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CollabMessage {
    /// Dispatched by joining client.
    #[serde(rename = "join")]
    Join { user: CollabUser },
    /// Broadcasts the current active developers vector in the project.
    #[serde(rename = "presence")]
    Presence { users: Vec<CollabUser> },
    /// Pointer coordinates of a user.
    #[serde(rename = "cursor")]
    CursorMove { user_id: String, x: f32, y: f32 },
    /// Temporary drag coordinates of a visual node.
    #[serde(rename = "node_drag")]
    NodeDrag { user_id: String, node_id: String, x: f32, y: f32 },
    /// Live graph structural modification (persisted to disk).
    #[serde(rename = "graph_edit")]
    GraphEdit { user_id: String, graph: serde_json::Value },
}

/// WebSocket sender channel handle.
pub type ClientTx = mpsc::UnboundedSender<Message>;

/// Active collaborator state handle.
pub struct CollabClient {
    pub user: CollabUser,
    pub tx: ClientTx,
}

/// Dynamic collaborator room channel pool.
#[derive(Default)]
pub struct CollabRoom {
    pub clients: Vec<CollabClient>,
}

/// Global collaboration session supervisor.
pub struct CollabManager {
    rooms: dashmap::DashMap<String, CollabRoom>,
}

impl CollabManager {
    /// Initialize a fresh CollabManager registry.
    pub fn new() -> Self {
        Self {
            rooms: dashmap::DashMap::new(),
        }
    }

    /// Add a collaborator connection to the room and return the updated presence list.
    pub fn join_room(&self, slug: &str, user: CollabUser, tx: ClientTx) -> Vec<CollabUser> {
        let mut room = self.rooms.entry(slug.to_string()).or_default();
        room.clients.push(CollabClient { user, tx });
        room.clients.iter().map(|c| c.user.clone()).collect()
    }

    /// Remove a collaborator from the room and return the updated presence list.
    pub fn leave_room(&self, slug: &str, user_id: &str) -> Vec<CollabUser> {
        if let Some(mut room) = self.rooms.get_mut(slug) {
            room.clients.retain(|c| c.user.id != user_id);
            room.clients.iter().map(|c| c.user.clone()).collect()
        } else {
            Vec::new()
        }
    }

    /// Broadcast a collaborative message to all other users in the room.
    pub fn broadcast(&self, slug: &str, sender_id: &str, msg: CollabMessage) {
        if let Some(room) = self.rooms.get(slug) {
            let serialized = serde_json::to_string(&msg).unwrap_or_default();
            let ws_msg = Message::Text(serialized);
            for client in &room.clients {
                if client.user.id != sender_id {
                    let _ = client.tx.send(ws_msg.clone());
                }
            }
        }
    }
}

/// `GET /ws/collab/:slug` — upgrade connection to WebSocket and join collaboration.
pub async fn collab_ws(
    ws: WebSocketUpgrade,
    Path(slug): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_collab_socket(socket, slug, state))
}

async fn handle_collab_socket(socket: WebSocket, slug: String, state: AppState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    // Asynchronous WebSocket write task.
    let writer_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut user_opt: Option<CollabUser> = None;
    let collab_manager = &state.collab_manager;
    let slug_clone = slug.clone();
    let collab_manager_clone = Arc::clone(collab_manager);

    // Process incoming client synchronization messages.
    while let Some(Ok(msg)) = ws_receiver.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };

        let parsed: Result<CollabMessage, _> = serde_json::from_str(&text);
        let collab_msg = match parsed {
            Ok(m) => m,
            Err(e) => {
                warn!("malformed collab message: {}, text: {}", e, text);
                continue;
            }
        };

        match collab_msg {
            CollabMessage::Join { user } => {
                user_opt = Some(user.clone());
                let users = collab_manager_clone.join_room(&slug_clone, user.clone(), tx.clone());
                collab_manager_clone.broadcast(
                    &slug_clone,
                    "",
                    CollabMessage::Presence { users },
                );
            }
            CollabMessage::CursorMove { user_id, x, y } => {
                let sender_id = user_id.clone();
                collab_manager_clone.broadcast(
                    &slug_clone,
                    &sender_id,
                    CollabMessage::CursorMove { user_id, x, y },
                );
            }
            CollabMessage::NodeDrag { user_id, node_id, x, y } => {
                let sender_id = user_id.clone();
                collab_manager_clone.broadcast(
                    &slug_clone,
                    &sender_id,
                    CollabMessage::NodeDrag { user_id, node_id, x, y },
                );
            }
            CollabMessage::GraphEdit { user_id, graph } => {
                let sender_id = user_id.clone();
                // Persist updated visual flow graph to project stores.
                if let Ok(parsed_graph) = serde_json::from_value(graph.clone()) {
                    if let Ok(slug_typed) = crate::projects::types::Slug::new(&slug_clone) {
                        if let Err(e) = state.store.save_graph(&slug_typed, &parsed_graph, &state.registry).await {
                            error!("collaborative save failed: {:?}", e);
                        }
                    }
                }
                collab_manager_clone.broadcast(
                    &slug_clone,
                    &sender_id,
                    CollabMessage::GraphEdit { user_id, graph },
                );
            }
            _ => {}
        }
    }

    // Cleanup and notify other users when a connection drops.
    if let Some(user) = user_opt {
        info!("collab user disconnected: {} ({})", user.username, user.id);
        let users = collab_manager_clone.leave_room(&slug_clone, &user.id);
        collab_manager_clone.broadcast(
            &slug_clone,
            "",
            CollabMessage::Presence { users },
        );
    }

    writer_task.abort();
}
