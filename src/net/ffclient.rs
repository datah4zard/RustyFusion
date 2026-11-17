use std::{
    collections::VecDeque,
    fmt::{Debug, Display},
    net::{IpAddr, SocketAddr},
    sync::{atomic::AtomicU64, Arc},
};

// We use parking_lot's RwLock instead of std's because it's more efficient and has a simpler API.
// On top of that, client metadata needs to be accessed from pure-sync context like the TUI,
// so we can't use tokio's async RwLock.
use parking_lot::RwLock;

use tokio::sync::mpsc::UnboundedSender;

use crate::{
    error::{panic_if_failed, FFError, FFResult, Severity},
    net::{
        crypto::EncryptionMode,
        packet::{FFPacket, Packet, PacketID},
        ClientMessage,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientType {
    Unknown,
    UnauthedClient {
        username: String,
        dup_pc_uid: Option<i64>,
    },
    GameClient {
        account_id: i64,
        serial_key: i64,    // iEnterSerialKey
        pc_id: Option<i32>, // iPC_ID
    },
    LoginServer,
    UnauthedShardServer(Arc<Vec<u8>>), // auth challenge
    ShardServer(i32),                  // shard ID
}
impl Display for ClientType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientType::Unknown => write!(f, "Unknown"),
            ClientType::UnauthedClient { username, .. } => {
                write!(f, "UnauthedClient({})", username)
            }
            ClientType::GameClient { account_id, .. } => write!(f, "GameClient({})", account_id),
            ClientType::LoginServer => write!(f, "LoginServer"),
            ClientType::UnauthedShardServer(_) => write!(f, "UnauthedShardServer"),
            ClientType::ShardServer(shard_id) => write!(f, "ShardServer({})", shard_id),
        }
    }
}

#[derive(Debug)]
pub struct ClientMetadata {
    pub addr: SocketAddr,
    pub client_type: ClientType,
    pub ping_ms: Option<AtomicU64>,
}
impl ClientMetadata {
    pub fn new(addr: SocketAddr, client_type: Option<ClientType>) -> Self {
        Self {
            addr,
            client_type: client_type.unwrap_or(ClientType::Unknown),
            ping_ms: None,
        }
    }
}

fn make_packet<T: FFPacket>(pkt_id: PacketID, pkt: &T) -> Packet {
    // It should be impossible for a single packet struct to be
    // bigger than the packet buffer, so fail fast if that happens.
    panic_if_failed(Packet::new(pkt_id, pkt))
}

/// Handle to a connected client.
/// This is cheap to clone and safe to send across threads.
#[derive(Clone)]
pub struct FFClient {
    tx: UnboundedSender<ClientMessage>,
    pub meta: Arc<RwLock<ClientMetadata>>,
}
impl Debug for FFClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let meta = self.meta.read();
        f.debug_struct("FFClient")
            .field("addr", &meta.addr)
            .field("client_type", &meta.client_type)
            .finish()
    }
}
impl FFClient {
    pub fn new(tx: UnboundedSender<ClientMessage>, meta: ClientMetadata) -> Self {
        Self {
            tx,
            meta: Arc::new(RwLock::new(meta)),
        }
    }

    pub fn send_packet<T: FFPacket>(&self, pkt_id: PacketID, pkt: &T) {
        let pkt = make_packet(pkt_id, pkt);
        self.send_payload(pkt);
    }

    pub fn send_payload(&self, pkt: Packet) {
        // it's okay to silently fail; if the channel is closed,
        // the client has already disconnected
        let _ = self.tx.send(ClientMessage::SendPacket(pkt));
    }

    pub fn batch(&self) -> PacketBatch<'_> {
        PacketBatch {
            client: self,
            queue: VecDeque::new(),
        }
    }

    pub fn update_encryption(
        &self,
        new_e_key: Option<u64>,
        new_fe_key: Option<u64>,
        new_mode: Option<EncryptionMode>,
    ) {
        let _ = self.tx.send(ClientMessage::UpdateEncryption {
            new_e_key,
            new_fe_key,
            new_mode,
        });
    }

    pub fn disconnect(&self) {
        let _ = self.tx.send(ClientMessage::Shutdown);
        let mut meta = self.meta.write();
        meta.client_type = ClientType::Unknown;
    }

    pub fn get_ip(&self) -> IpAddr {
        let meta = self.meta.read();
        meta.addr.ip()
    }

    pub fn get_addr(&self) -> String {
        let meta = self.meta.read();
        meta.addr.to_string()
    }

    pub fn get_client_type(&self) -> ClientType {
        let meta = self.meta.read();
        meta.client_type.clone()
    }

    pub fn set_client_type(&self, client_type: ClientType) {
        let mut meta = self.meta.write();
        meta.client_type = client_type;
    }

    pub fn get_account_id(&self) -> FFResult<i64> {
        let meta = self.meta.read();
        if let ClientType::GameClient { account_id, .. } = meta.client_type {
            Ok(account_id)
        } else {
            Err(FFError::build(
                Severity::Warning,
                "Couldn't get account ID for client".to_string(),
            ))
        }
    }

    pub fn get_player_id(&self) -> FFResult<i32> {
        let meta = self.meta.read();
        if let ClientType::GameClient {
            pc_id: Some(pc_id), ..
        } = meta.client_type
        {
            Ok(pc_id)
        } else {
            Err(FFError::build(
                Severity::Warning,
                "Couldn't get player ID for client".to_string(),
            ))
        }
    }

    pub fn get_shard_id(&self) -> FFResult<i32> {
        let meta = self.meta.read();
        if let ClientType::ShardServer(shard_id) = meta.client_type {
            Ok(shard_id)
        } else {
            Err(FFError::build(
                Severity::Warning,
                "Couldn't get shard ID for client".to_string(),
            ))
        }
    }

    pub fn clear_player_id(&self) -> FFResult<i32> {
        let pc_id = self.get_player_id()?;
        let mut meta = self.meta.write();
        if let ClientType::GameClient { pc_id, .. } = &mut meta.client_type {
            *pc_id = None;
        }
        Ok(pc_id)
    }

    pub fn get_serial_key(&self) -> FFResult<i64> {
        let meta = self.meta.read();
        if let ClientType::GameClient { serial_key, .. } = meta.client_type {
            Ok(serial_key)
        } else {
            Err(FFError::build(
                Severity::Warning,
                "Couldn't get serial key for client".to_string(),
            ))
        }
    }

    pub fn clear_live_check(&self) {
        let _ = self.tx.send(ClientMessage::ClearLiveCheck);
    }
}

pub struct PacketBatch<'a> {
    client: &'a FFClient,
    queue: VecDeque<Packet>,
}
impl PacketBatch<'_> {
    pub fn queue_packet<T: FFPacket>(&mut self, pkt_id: PacketID, pkt: &T) {
        let pkt = make_packet(pkt_id, pkt);
        self.queue_payload(pkt);
    }

    pub fn queue_payload(&mut self, pkt: Packet) {
        self.queue.push_back(pkt);
    }

    pub fn send_all(mut self) {
        while let Some(pkt) = self.queue.pop_front() {
            self.client.send_payload(pkt);
        }
    }
}
