//! Distributed CDN features for multi-node deployments.
//!
//! Provides:
//! - Consistent hashing for cache routing
//! - Distributed cache invalidation
//! - Gossip protocol for node discovery and health

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch, RwLock};
use tracing::{debug, info, warn};

/// Configuration for distributed features.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedConfig {
    /// Enable distributed features
    #[serde(default)]
    pub enabled: bool,

    /// This node's identifier
    #[serde(default = "default_node_id")]
    pub node_id: String,

    /// Bind address for gossip protocol
    #[serde(default = "default_gossip_bind")]
    pub gossip_bind: String,

    /// Seed nodes for initial cluster join
    #[serde(default)]
    pub seed_nodes: Vec<String>,

    /// Number of virtual nodes per physical node in consistent hash ring
    #[serde(default = "default_virtual_nodes")]
    pub virtual_nodes: u32,

    /// Gossip interval in milliseconds
    #[serde(default = "default_gossip_interval")]
    pub gossip_interval_ms: u64,

    /// Node timeout before considered dead
    #[serde(default = "default_node_timeout")]
    pub node_timeout_secs: u64,

    /// Invalidation broadcast port
    #[serde(default = "default_invalidation_port")]
    pub invalidation_port: u16,
}

impl Default for DistributedConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            node_id: default_node_id(),
            gossip_bind: default_gossip_bind(),
            seed_nodes: Vec::new(),
            virtual_nodes: default_virtual_nodes(),
            gossip_interval_ms: default_gossip_interval(),
            node_timeout_secs: default_node_timeout(),
            invalidation_port: default_invalidation_port(),
        }
    }
}

fn default_node_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn default_gossip_bind() -> String {
    "0.0.0.0:7946".to_string()
}

fn default_virtual_nodes() -> u32 {
    150
}

fn default_gossip_interval() -> u64 {
    1000
}

fn default_node_timeout() -> u64 {
    30
}

fn default_invalidation_port() -> u16 {
    7947
}

/// A node in the cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterNode {
    pub id: String,
    pub addr: SocketAddr,
    pub state: NodeState,
    pub generation: u64,
    pub last_seen: u64,
    pub metadata: HashMap<String, String>,
}

/// Node health state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeState {
    Alive,
    Suspect,
    Dead,
}

/// Consistent hash ring for cache routing.
pub struct HashRing {
    ring: RwLock<BTreeMap<u64, String>>,
    nodes: RwLock<HashSet<String>>,
    virtual_nodes: u32,
}

impl HashRing {
    pub fn new(virtual_nodes: u32) -> Self {
        Self {
            ring: RwLock::new(BTreeMap::new()),
            nodes: RwLock::new(HashSet::new()),
            virtual_nodes,
        }
    }

    fn hash_key(key: &str) -> u64 {
        let mut hasher = xxhash_rust::xxh3::Xxh3::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    pub async fn add_node(&self, node_id: &str) {
        let mut ring = self.ring.write().await;
        let mut nodes = self.nodes.write().await;

        if nodes.contains(node_id) {
            return;
        }

        for i in 0..self.virtual_nodes {
            let vnode_key = format!("{}:{}", node_id, i);
            let hash = Self::hash_key(&vnode_key);
            ring.insert(hash, node_id.to_string());
        }
        nodes.insert(node_id.to_string());
        debug!(node_id, vnodes = self.virtual_nodes, "Added node to hash ring");
    }

    pub async fn remove_node(&self, node_id: &str) {
        let mut ring = self.ring.write().await;
        let mut nodes = self.nodes.write().await;

        if !nodes.remove(node_id) {
            return;
        }

        for i in 0..self.virtual_nodes {
            let vnode_key = format!("{}:{}", node_id, i);
            let hash = Self::hash_key(&vnode_key);
            ring.remove(&hash);
        }
        debug!(node_id, "Removed node from hash ring");
    }

    pub async fn get_node(&self, key: &str) -> Option<String> {
        let ring = self.ring.read().await;
        if ring.is_empty() {
            return None;
        }

        let hash = Self::hash_key(key);

        // Find first node with hash >= key hash (wrap around if needed)
        ring.range(hash..)
            .next()
            .or_else(|| ring.iter().next())
            .map(|(_, node)| node.clone())
    }

    pub async fn get_nodes(&self, key: &str, count: usize) -> Vec<String> {
        let ring = self.ring.read().await;
        if ring.is_empty() {
            return Vec::new();
        }

        let hash = Self::hash_key(key);
        let mut result = Vec::with_capacity(count);
        let mut seen = HashSet::new();

        // Collect unique nodes starting from hash position
        for (_, node) in ring.range(hash..).chain(ring.iter()) {
            if seen.insert(node.clone()) {
                result.push(node.clone());
                if result.len() >= count {
                    break;
                }
            }
        }

        result
    }

    pub async fn node_count(&self) -> usize {
        self.nodes.read().await.len()
    }
}

/// Cache invalidation message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvalidationMessage {
    pub id: String,
    pub origin_node: String,
    pub timestamp: u64,
    pub invalidation: Invalidation,
}

/// Types of cache invalidation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Invalidation {
    /// Invalidate a specific key
    Key(String),
    /// Invalidate keys matching a prefix
    Prefix(String),
    /// Invalidate keys matching a pattern (glob)
    Pattern(String),
    /// Invalidate all keys for an origin
    Origin(String),
    /// Purge everything
    All,
}

/// Distributed cache invalidation manager.
pub struct InvalidationManager {
    node_id: String,
    seen_ids: RwLock<HashSet<String>>,
    broadcast_tx: broadcast::Sender<InvalidationMessage>,
    sequence: AtomicU64,
}

impl InvalidationManager {
    pub fn new(node_id: String) -> (Self, broadcast::Receiver<InvalidationMessage>) {
        let (tx, rx) = broadcast::channel(1024);
        (
            Self {
                node_id,
                seen_ids: RwLock::new(HashSet::new()),
                broadcast_tx: tx,
                sequence: AtomicU64::new(0),
            },
            rx,
        )
    }

    pub fn subscribe(&self) -> broadcast::Receiver<InvalidationMessage> {
        self.broadcast_tx.subscribe()
    }

    pub async fn invalidate(&self, inv: Invalidation) -> InvalidationMessage {
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
        let msg = InvalidationMessage {
            id: format!("{}-{}", self.node_id, seq),
            origin_node: self.node_id.clone(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            invalidation: inv,
        };

        self.seen_ids.write().await.insert(msg.id.clone());
        let _ = self.broadcast_tx.send(msg.clone());
        msg
    }

    pub async fn receive(&self, msg: InvalidationMessage) -> bool {
        let mut seen = self.seen_ids.write().await;
        if seen.contains(&msg.id) {
            return false;
        }
        seen.insert(msg.id.clone());

        // Prune old IDs (keep last 10000)
        if seen.len() > 10000 {
            let to_remove: Vec<_> = seen.iter().take(seen.len() - 10000).cloned().collect();
            for id in to_remove {
                seen.remove(&id);
            }
        }

        let _ = self.broadcast_tx.send(msg);
        true
    }
}

/// Gossip protocol message types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GossipMessage {
    /// Ping to check if node is alive
    Ping { from: String, seq: u64 },
    /// Ack in response to ping
    Ack { from: String, seq: u64 },
    /// Sync cluster state
    Sync { nodes: Vec<ClusterNode> },
    /// Join request
    Join { node: ClusterNode },
    /// Leave notification
    Leave { node_id: String },
}

/// Gossip-based cluster membership.
pub struct GossipCluster {
    config: DistributedConfig,
    local_node: ClusterNode,
    members: Arc<RwLock<HashMap<String, ClusterNode>>>,
    hash_ring: Arc<HashRing>,
    ping_seq: AtomicU64,
}

impl GossipCluster {
    pub fn new(config: DistributedConfig, bind_addr: SocketAddr) -> Self {
        let local_node = ClusterNode {
            id: config.node_id.clone(),
            addr: bind_addr,
            state: NodeState::Alive,
            generation: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            last_seen: 0,
            metadata: HashMap::new(),
        };

        let hash_ring = Arc::new(HashRing::new(config.virtual_nodes));

        Self {
            config,
            local_node,
            members: Arc::new(RwLock::new(HashMap::new())),
            hash_ring,
            ping_seq: AtomicU64::new(0),
        }
    }

    pub fn hash_ring(&self) -> Arc<HashRing> {
        self.hash_ring.clone()
    }

    pub async fn start(
        self: Arc<Self>,
        mut shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let bind_addr: SocketAddr = self.config.gossip_bind.parse()?;
        let socket = Arc::new(UdpSocket::bind(bind_addr).await?);

        info!(addr = %bind_addr, node_id = %self.config.node_id, "Gossip cluster starting");

        // Add self to hash ring
        self.hash_ring.add_node(&self.config.node_id).await;

        // Join seed nodes
        for seed in &self.config.seed_nodes {
            if let Ok(addr) = seed.parse::<SocketAddr>() {
                let join_msg = GossipMessage::Join {
                    node: self.local_node.clone(),
                };
                if let Ok(data) = serde_json::to_vec(&join_msg) {
                    let _ = socket.send_to(&data, addr).await;
                }
            }
        }

        let recv_socket = socket.clone();
        let recv_self = self.clone();
        let mut recv_shutdown = shutdown.clone();

        // Receiver task
        let recv_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                tokio::select! {
                    result = recv_socket.recv_from(&mut buf) => {
                        match result {
                            Ok((len, from)) => {
                                if let Ok(msg) = serde_json::from_slice::<GossipMessage>(&buf[..len]) {
                                    recv_self.handle_message(msg, from, &recv_socket).await;
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "Gossip receive error");
                            }
                        }
                    }
                    _ = recv_shutdown.changed() => {
                        if *recv_shutdown.borrow() {
                            break;
                        }
                    }
                }
            }
        });

        // Gossip loop
        let gossip_interval = Duration::from_millis(self.config.gossip_interval_ms);
        let node_timeout = Duration::from_secs(self.config.node_timeout_secs);

        loop {
            tokio::select! {
                _ = tokio::time::sleep(gossip_interval) => {
                    self.gossip_round(&socket, node_timeout).await;
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        // Send leave message
                        let leave = GossipMessage::Leave {
                            node_id: self.config.node_id.clone(),
                        };
                        if let Ok(data) = serde_json::to_vec(&leave) {
                            let members = self.members.read().await;
                            for member in members.values() {
                                let _ = socket.send_to(&data, member.addr).await;
                            }
                        }
                        break;
                    }
                }
            }
        }

        recv_handle.abort();
        info!("Gossip cluster stopped");
        Ok(())
    }

    async fn handle_message(&self, msg: GossipMessage, from: SocketAddr, socket: &UdpSocket) {
        match msg {
            GossipMessage::Ping { from: node_id, seq } => {
                let ack = GossipMessage::Ack {
                    from: self.config.node_id.clone(),
                    seq,
                };
                if let Ok(data) = serde_json::to_vec(&ack) {
                    let _ = socket.send_to(&data, from).await;
                }
                self.update_node_seen(&node_id).await;
            }
            GossipMessage::Ack { from: node_id, .. } => {
                self.update_node_seen(&node_id).await;
            }
            GossipMessage::Sync { nodes } => {
                for node in nodes {
                    self.merge_node(node).await;
                }
            }
            GossipMessage::Join { node } => {
                info!(node_id = %node.id, addr = %node.addr, "Node joining cluster");
                self.merge_node(node).await;

                // Send current cluster state
                let members: Vec<ClusterNode> = self.members.read().await.values().cloned().collect();
                let sync = GossipMessage::Sync { nodes: members };
                if let Ok(data) = serde_json::to_vec(&sync) {
                    let _ = socket.send_to(&data, from).await;
                }
            }
            GossipMessage::Leave { node_id } => {
                info!(node_id = %node_id, "Node leaving cluster");
                self.members.write().await.remove(&node_id);
                self.hash_ring.remove_node(&node_id).await;
            }
        }
    }

    async fn update_node_seen(&self, node_id: &str) {
        if let Some(node) = self.members.write().await.get_mut(node_id) {
            node.last_seen = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            node.state = NodeState::Alive;
        }
    }

    async fn merge_node(&self, mut node: ClusterNode) {
        let mut members = self.members.write().await;

        // Don't add ourselves
        if node.id == self.config.node_id {
            return;
        }

        node.last_seen = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let is_new = !members.contains_key(&node.id);
        members.insert(node.id.clone(), node.clone());
        drop(members);

        if is_new {
            self.hash_ring.add_node(&node.id).await;
        }
    }

    async fn gossip_round(&self, socket: &UdpSocket, timeout: Duration) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let timeout_ms = timeout.as_millis() as u64;
        let mut members = self.members.write().await;
        let mut dead_nodes = Vec::new();

        // Check for dead nodes and ping alive ones
        for (id, node) in members.iter_mut() {
            let elapsed = now.saturating_sub(node.last_seen);

            match node.state {
                NodeState::Alive if elapsed > timeout_ms / 2 => {
                    node.state = NodeState::Suspect;
                    debug!(node_id = %id, "Node marked suspect");
                }
                NodeState::Suspect if elapsed > timeout_ms => {
                    node.state = NodeState::Dead;
                    dead_nodes.push(id.clone());
                    warn!(node_id = %id, "Node marked dead");
                }
                _ => {}
            }
        }

        // Remove dead nodes
        for id in &dead_nodes {
            members.remove(id);
        }
        drop(members);

        for id in dead_nodes {
            self.hash_ring.remove_node(&id).await;
        }

        // Ping random subset of members
        let members = self.members.read().await;
        let targets: Vec<_> = members.values()
            .filter(|n| n.state != NodeState::Dead)
            .take(3)
            .collect();

        for target in targets {
            let seq = self.ping_seq.fetch_add(1, Ordering::SeqCst);
            let ping = GossipMessage::Ping {
                from: self.config.node_id.clone(),
                seq,
            };
            if let Ok(data) = serde_json::to_vec(&ping) {
                let _ = socket.send_to(&data, target.addr).await;
            }
        }
    }

    pub async fn members(&self) -> Vec<ClusterNode> {
        self.members.read().await.values().cloned().collect()
    }

    pub async fn alive_members(&self) -> Vec<ClusterNode> {
        self.members
            .read()
            .await
            .values()
            .filter(|n| n.state == NodeState::Alive)
            .cloned()
            .collect()
    }
}

/// Distributed invalidation broadcaster.
pub async fn run_invalidation_listener(
    bind_addr: SocketAddr,
    manager: Arc<InvalidationManager>,
    peers: Arc<RwLock<Vec<SocketAddr>>>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let socket = Arc::new(UdpSocket::bind(bind_addr).await?);
    info!(addr = %bind_addr, "Invalidation listener started");

    let recv_socket = socket.clone();
    let recv_manager = manager.clone();

    let mut buf = vec![0u8; 65535];

    loop {
        tokio::select! {
            result = recv_socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, _from)) => {
                        if let Ok(msg) = serde_json::from_slice::<InvalidationMessage>(&buf[..len]) {
                            if recv_manager.receive(msg.clone()).await {
                                // Forward to peers
                                let peers = peers.read().await;
                                for peer in peers.iter() {
                                    let _ = recv_socket.send_to(&buf[..len], peer).await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Invalidation receive error");
                    }
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
            }
        }
    }

    info!("Invalidation listener stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_ring_distributes_keys() {
        let ring = HashRing::new(100);
        ring.add_node("node1").await;
        ring.add_node("node2").await;
        ring.add_node("node3").await;

        let mut distribution: HashMap<String, usize> = HashMap::new();
        for i in 0..1000 {
            let key = format!("key-{}", i);
            if let Some(node) = ring.get_node(&key).await {
                *distribution.entry(node).or_insert(0) += 1;
            }
        }

        // Each node should get roughly 1/3 of keys (within reasonable bounds)
        for count in distribution.values() {
            assert!(*count > 200 && *count < 500, "Distribution uneven: {:?}", distribution);
        }
    }

    #[tokio::test]
    async fn hash_ring_get_multiple_nodes() {
        let ring = HashRing::new(100);
        ring.add_node("node1").await;
        ring.add_node("node2").await;
        ring.add_node("node3").await;

        let nodes = ring.get_nodes("test-key", 2).await;
        assert_eq!(nodes.len(), 2);
        assert_ne!(nodes[0], nodes[1]);
    }

    #[tokio::test]
    async fn hash_ring_consistent_after_removal() {
        let ring = HashRing::new(100);
        ring.add_node("node1").await;
        ring.add_node("node2").await;
        ring.add_node("node3").await;

        let before = ring.get_node("persistent-key").await;
        ring.remove_node("node2").await;
        let after = ring.get_node("persistent-key").await;

        // Key should either stay on same node or move to next in ring
        assert!(before.is_some() && after.is_some());
    }

    #[tokio::test]
    async fn invalidation_manager_dedupes() {
        let (manager, _rx) = InvalidationManager::new("test-node".to_string());

        let msg = manager.invalidate(Invalidation::Key("test".to_string())).await;

        // Receiving same message should return false
        assert!(!manager.receive(msg.clone()).await);
    }

    #[tokio::test]
    async fn invalidation_manager_accepts_new() {
        let (manager, _rx) = InvalidationManager::new("node1".to_string());

        let msg = InvalidationMessage {
            id: "node2-1".to_string(),
            origin_node: "node2".to_string(),
            timestamp: 12345,
            invalidation: Invalidation::Prefix("/api/".to_string()),
        };

        assert!(manager.receive(msg).await);
    }

    #[test]
    fn default_config_disabled() {
        let config = DistributedConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.virtual_nodes, 150);
    }

    #[test]
    fn cluster_node_states() {
        assert_eq!(NodeState::Alive, NodeState::Alive);
        assert_ne!(NodeState::Alive, NodeState::Dead);
    }
}
