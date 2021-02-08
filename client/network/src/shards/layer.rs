// Copyright 2018 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

use super::protocol::{ShardsProtocol, ShardsMessage, Rpc, ShardsJoin, ShardsAction};
use super::shard::Shard;
use super::ShardsConfig;
use cuckoofilter::{CuckooError, CuckooFilter};
use fnv::FnvHashSet;
use libp2p::core::{Multiaddr, PeerId, connection::ConnectionId, ConnectedPoint};
use libp2p::swarm::{
    NetworkBehaviour,
    NetworkBehaviourAction,
    PollParameters,
    ProtocolsHandler,
    OneShotHandler,
    NotifyHandler,
    DialPeerCondition,
};
use log::warn;
use rand;
use smallvec::SmallVec;
use std::{collections::VecDeque, iter};
use std::collections::hash_map::{DefaultHasher, HashMap};
use std::task::{Context, Poll};

/// Network behaviour that handles the shards protocol.
pub struct Shards {
    /// Events that need to be yielded to the outside when polling.
    events: VecDeque<NetworkBehaviourAction<Rpc, ShardsEvent>>,

    config: ShardsConfig,

    /// List of peers to send messages to.
    target_peers: FnvHashSet<PeerId>,

    /// List of peers the network is connected to, and the shards that they're joined to.
    // TODO: filter out peers that don't support shards, so that we avoid hammering them with
    //       opened substreams
    connected_peers: HashMap<PeerId, SmallVec<[Shard; 1024]>>,

    known_peers:HashMap<PeerId,Vec<Multiaddr>>,

    // pending_join:VecDeque<Shard>,

    // List of shards we're joined to. Necessary to filter out messages that we receive
    // erroneously.
    joined_shards: SmallVec<[Shard; 1024]>,

    // We keep track of the messages we received (in the format `hash(source ID, seq_no)`) so that
    // we don't dispatch the same message twice if we receive it twice on the network.
    received: CuckooFilter<DefaultHasher>,
}

impl Shards {
    /// Creates a `shards` with default configuration.
    pub fn new(local_peer_id: PeerId) -> Self {
        Self::from_config(ShardsConfig::new(local_peer_id))
    }

    /// Creates a `shards` with the given configuration.
    pub fn from_config(config: ShardsConfig) -> Self {
        Shards {
            events: VecDeque::new(),
            config,
            target_peers: FnvHashSet::default(),
            connected_peers: HashMap::new(),
            // pending_join: VecDeque::with_capacity(40),
            known_peers:HashMap::new(),
            joined_shards: SmallVec::new(),
            received: CuckooFilter::new(),
        }
    }

    pub fn add_known_peer(&mut self,peer_id:PeerId,addr:Vec<Multiaddr>) {
        self.known_peers.insert(peer_id,addr);
    }

    /// Add a node to the list of nodes to propagate messages to.
    #[inline]
    pub fn add_node_to_partial_view(&mut self, peer_id: PeerId) {
        // Send our shards to this node if we're already connected to it.
        if self.connected_peers.contains_key(&peer_id) {
            for shard in self.joined_shards.iter().cloned() {
                self.events.push_back(NetworkBehaviourAction::NotifyHandler {
                    peer_id: peer_id.clone(),
                    handler: NotifyHandler::Any,
                    event: Rpc {
                        messages: Vec::new(),
                        shards: vec![ShardsJoin {
                            shard,
                            action: ShardsAction::Join,
                        }],
                        ttl:Some(6),
                    },
                });
            }
        }

        if self.target_peers.insert(peer_id.clone()) {
            self.events.push_back(NetworkBehaviourAction::DialPeer {
                peer_id, condition: DialPeerCondition::Disconnected
            });
        }
        log::info!("target_peers:{},connected_peers:{}",self.target_peers.len(),self.connected_peers.len());
    }

    /// Remove a node from the list of nodes to propagate messages to.
    #[inline]
    pub fn remove_node_from_partial_view(&mut self, peer_id: &PeerId) {
        self.target_peers.remove(peer_id);
    }

    /// join to a shard.
    ///
    /// Returns true if the join worked. Returns false if we were already joined.
    pub fn join(&mut self, shard: Shard) -> bool {
        if self.joined_shards.iter().any(|t| t.id() == shard.id()) {
            return false;
        }
        //
        // if self.connected_peers.is_empty() {
        //     log::warn!("joining for connected peers is empty!");
        //     self.pending_join.push_back(shard);
        //     return false;
        // }

        for peer in self.connected_peers.keys() {
            self.events.push_back(NetworkBehaviourAction::NotifyHandler {
                peer_id: peer.clone(),
                handler: NotifyHandler::Any,
                event: Rpc {
                    messages: Vec::new(),
                    shards: vec![ShardsJoin {
                        shard: shard.clone(),
                        action: ShardsAction::Join,
                    }],
                    ttl:Some(6),
                },
            });
        }

        self.joined_shards.push(shard);
        true
    }

    /// leave from a shard.
    ///
    /// Note that this only requires the shard name.
    ///
    /// Returns true if we were joined to this shard.
    pub fn leave(&mut self, shard: Shard) -> bool {
        let pos = match self.joined_shards.iter().position(|t| *t == shard) {
            Some(pos) => pos,
            None => return false
        };

        self.joined_shards.remove(pos);

        for peer in self.connected_peers.keys() {
            self.events.push_back(NetworkBehaviourAction::NotifyHandler {
                peer_id: peer.clone(),
                handler: NotifyHandler::Any,
                event: Rpc {
                    messages: Vec::new(),
                    shards: vec![ShardsJoin {
                        shard: shard.clone(),
                        action: ShardsAction::Leave,
                    }],
                    ttl:Some(6),
                },
            });
        }

        true
    }

    /// Publishes a message to the network, if we're joined to the shard only.
    pub fn publish(&mut self, shard: impl Into<Shard>, data: impl Into<Vec<u8>>) {
        self.publish_many(iter::once(shard), data)
    }

    /// Publishes a message to the network, even if we're not joined to the shard.
    pub fn publish_any(&mut self, shard: impl Into<Shard>, data: impl Into<Vec<u8>>) {
        self.publish_many_any(iter::once(shard), data)
    }

    /// Publishes a message with multiple shards to the network.
    ///
    ///
    /// > **Note**: Doesn't do anything if we're not joined to any of the shards.
    pub fn publish_many(&mut self, shard: impl IntoIterator<Item = impl Into<Shard>>, data: impl Into<Vec<u8>>) {
        self.publish_many_inner(shard, data, true)
    }

    /// Publishes a message with multiple shards to the network, even if we're not joined to any of the shard.
    pub fn publish_many_any(&mut self, shard: impl IntoIterator<Item = impl Into<Shard>>, data: impl Into<Vec<u8>>) {
        self.publish_many_inner(shard, data, false)
    }

    fn publish_many_inner(&mut self, shard: impl IntoIterator<Item = impl Into<Shard>>, data: impl Into<Vec<u8>>, check_self_joins: bool) {
        let message = ShardsMessage {
            source: self.config.local_peer_id.clone().into_bytes(),
            data: data.into(),
            // If the sequence numbers are predictable, then an attacker could shards the network
            // with packets with the predetermined sequence numbers and absorb our legitimate
            // messages. We therefore use a random number.
            sequence_number: rand::random::<[u8; 20]>().to_vec(),
            shards: shard.into_iter().map(Into::into).collect(),
        };

        let self_joined = self.joined_shards.iter().any(|t| message.shards.iter().any(|u| t == u));
        if self_joined {
            if let Err(e @ CuckooError::NotEnoughSpace) = self.received.add(&message) {
                warn!(
                    "Message was added to 'received' Cuckoofilter but some \
                     other message was removed as a consequence: {}", e,
                );
            }
            if self.config.join_local_messages {
                self.events.push_back(
                    NetworkBehaviourAction::GenerateEvent(ShardsEvent::Message(message.clone())));
            }
            else{
                log::info!("not join_local_messages");
            }
        }
        else{
            log::info!("not self_joined");
        }
        // Don't publish the message if we have to check join
        // and we're not joined ourselves to any of the shards.
        if check_self_joins && !self_joined {
            log::warn!("Don't publish the message if we have to check join and we're not joined ourselves to any of the shards");
            return
        }
        if self.connected_peers.is_empty() {
            log::warn!("connected_peers is empty!!!!!");
        }
        // Send to peers we know are joined to the shard.
        for (peer_id, join_shards) in self.connected_peers.iter() {
            if !join_shards.iter().any(|t| message.shards.iter().any(|u| t == u)) {

                continue;
            }

            self.events.push_back(NetworkBehaviourAction::NotifyHandler {
                peer_id: peer_id.clone(),
                handler: NotifyHandler::Any,
                event: Rpc {
                    shards: Vec::new(),
                    messages: vec![message.clone()],
                    ttl:Some(6),
                }

            });
        }
    }
}

impl NetworkBehaviour for Shards {
    type ProtocolsHandler = OneShotHandler<ShardsProtocol, Rpc, InnerMessage>;
    type OutEvent = ShardsEvent;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        Default::default()
    }

    fn addresses_of_peer(&mut self, remote: &PeerId) -> Vec<Multiaddr> {
        // log::warn!("addresses_of_peer:{:?} is unknown!!!",remote);
        self.known_peers.get(remote).unwrap_or(&vec![]).into_iter().map(|v|v.clone()).collect::<Vec<_>>()
    }
    fn inject_connection_established(&mut self, remote: &PeerId, id: &ConnectionId, point: &ConnectedPoint)
    {
        match point {
            ConnectedPoint::Dialer { address } => {
                log::info!("Dialer:{:?}---->>{:?}",remote,address);
                self.known_peers.entry(remote.clone()).or_insert(Vec::with_capacity(10)).push(address.clone());
                self.target_peers.insert(remote.clone());
            }
            ConnectedPoint::Listener { local_addr, send_back_addr } => {
                log::info!("Listener:{:?}--->{:?},{:?}",remote,local_addr,send_back_addr);
                self.known_peers.entry(remote.clone()).or_insert(Vec::with_capacity(10)).push(send_back_addr.clone());
                self.target_peers.insert(remote.clone());
            }
        }
    }
    fn inject_connected(&mut self, id: &PeerId) {
        // We need to send our shards to the newly-connected node.
        log::info!("inject_connected {:?}",&id);
        if self.target_peers.contains(id) {
            for shard in self.joined_shards.iter().cloned() {
                log::info!("shard {:?} notify to {:?}",&shard,&id);
                self.events.push_back(NetworkBehaviourAction::NotifyHandler {
                    peer_id: id.clone(),
                    handler: NotifyHandler::Any,
                    event: Rpc {
                        messages: Vec::new(),
                        shards: vec![ShardsJoin {
                            shard,
                            action: ShardsAction::Join,
                        }],
                        ttl:Some(6),
                    },
                });
            }
        }
        else{
            log::warn!("inject_connected:target_peer not contains peer:{:?}",&id);
        }

        self.connected_peers.insert(id.clone(), SmallVec::new());
    }


    fn inject_disconnected(&mut self, id: &PeerId) {

        let was_in = self.connected_peers.remove(id);
        debug_assert!(was_in.is_some());
        log::info!("inject_disconnected:{:?}----{:?}",&id,was_in);
        // We can be disconnected by the remote in case of inactivity for example, so we always
        // try to reconnect.
        if self.target_peers.contains(id) {
            self.events.push_back(NetworkBehaviourAction::DialPeer {
                peer_id: id.clone(),
                condition: DialPeerCondition::Disconnected
            });
        }
    }

    fn inject_event(
        &mut self,
        propagation_source: PeerId,
        _connection: ConnectionId,
        event: InnerMessage,
    ) {
        // We ignore successful sends or timeouts.
        let event = match event {
            InnerMessage::Rx(event) => event,
            InnerMessage::Sent => return,
        };
        let ttl = &event.ttl.unwrap_or(6);
        if ttl < &1 {
            log::warn!("ttl<1");
            return;
        }
        // Update connected peers shards
        for join in event.shards {
            let remote_peer_shards = self.connected_peers
                .get_mut(&propagation_source)
                .expect("connected_peers is kept in sync with the peers we are connected to; we are guaranteed to only receive events from connected peers; QED");

            match join.action {
                ShardsAction::Join => {
                    if !remote_peer_shards.contains(&join.shard) {
                        remote_peer_shards.push(join.shard.clone());
                    }
                    self.events.push_back(NetworkBehaviourAction::GenerateEvent(ShardsEvent::Joined {
                        peer_id: propagation_source.clone(),
                        shard: join.shard,
                    }));
                    log::info!("shards join remote:{:?},remote_peer_shards:{}",&propagation_source,remote_peer_shards.len());
                }
                ShardsAction::Leave => {
                    if let Some(pos) = remote_peer_shards.iter().position(|t| t == &join.shard ) {
                        remote_peer_shards.remove(pos);
                    }
                    self.events.push_back(NetworkBehaviourAction::GenerateEvent(ShardsEvent::Left {
                        peer_id: propagation_source.clone(),
                        shard: join.shard,
                    }));
                    log::info!("shards left remote:{:?},connected_count:{}",&propagation_source,self.connected_peers.len());
                }
            }
        }

        // List of messages we're going to propagate on the network.
        let mut rpcs_to_dispatch: Vec<(PeerId, Rpc)> = Vec::new();
        let mut message_count = 0;
        for message in event.messages {
            message_count+=1;
            // Use `self.received` to skip the messages that we have already received in the past.
            // Note that this can result in false positives.
            match self.received.test_and_add(&message) {
                Ok(true) => {
                    // log::warn!("Message  was added");
                }, // Message  was added.
                Ok(false) => {
                    // log::warn!("Message already existed");
                    continue
                }, // Message already existed.
                Err(e @ CuckooError::NotEnoughSpace) => { // Message added, but some other removed.
                    warn!(
                        "Message was added to 'received' Cuckoofilter but some \
                         other message was removed as a consequence: {}", e,
                    );
                }
            }
            if message.shards.is_empty() {
                log::warn!("-------------------------------message shards is empty!!!!");
            }
            // Add the message to be dispatched to the user.
            if self.joined_shards.iter().any(|t| message.shards.iter().any(|u| t == u)) {
                let event = ShardsEvent::Message(message.clone());
                self.events.push_back(NetworkBehaviourAction::GenerateEvent(event));
            }

            // Propagate the message to everyone else who is joined to any of the shards.
            for (peer_id, join_shards) in self.connected_peers.iter() {
                if peer_id == &propagation_source {
                    continue;
                }

                if !join_shards.iter().any(|t| message.shards.iter().any(|u| t == u)) {
                    continue;
                }
                let mut msg = message.clone();
                // msg.source = self.config.local_peer_id.clone();
                if let Some(pos) = rpcs_to_dispatch.iter().position(|(p, _)| p == peer_id) {
                    rpcs_to_dispatch[pos].1.messages.push(msg.clone());
                } else {
                    rpcs_to_dispatch.push((peer_id.clone(), Rpc {
                        shards: Vec::new(),
                        messages: vec![msg.clone()],
                        ttl:Some((*ttl).max(1) - 1),
                    }));
                }
            }
        }

        if message_count > 0 && rpcs_to_dispatch.is_empty() {
            log::warn!("rpcs_to_dispatch is empty!!!joined_shards:{:?}",self.joined_shards);
        }

        for (peer_id, rpc) in rpcs_to_dispatch {
            log::info!("shard message dispatching...to {:?}",&peer_id);
            self.events.push_back(NetworkBehaviourAction::NotifyHandler {
                peer_id,
                handler: NotifyHandler::Any,
                event: rpc,
            });
        }
    }

    fn poll(
        &mut self,
        _: &mut Context<'_>,
        _: &mut impl PollParameters,
    ) -> Poll<
        NetworkBehaviourAction<
            <Self::ProtocolsHandler as ProtocolsHandler>::InEvent,
            Self::OutEvent,
        >,
    > {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }
        //
        // if let Some(shard) = self.pending_join.pop_front() {
        //     for peer in self.connected_peers.keys() {
        //
        //     }
        //     return Poll::Ready(NetworkBehaviourAction::NotifyHandler {
        //         peer_id: ,
        //         handler: NotifyHandler::Any,
        //         event: ShardsRpc {
        //             messages: Vec::new(),
        //             shards: vec![ShardsJoin {
        //                 shard: shard.clone(),
        //                 action: ShardsAction::Join,
        //             }],
        //         },
        //     });
        // }




        Poll::Pending
    }
}

/// Transmission between the `OneShotHandler` and the `ShardsHandler`.
pub enum InnerMessage {
    /// We received an RPC from a remote.
    Rx(Rpc),
    /// We successfully sent an RPC request.
    Sent,
}

impl From<Rpc> for InnerMessage {
    #[inline]
    fn from(rpc: Rpc) -> InnerMessage {
        InnerMessage::Rx(rpc)
    }
}

impl From<()> for InnerMessage {
    #[inline]
    fn from(_: ()) -> InnerMessage {
        InnerMessage::Sent
    }
}

/// Event that can happen on the shards behaviour.
#[derive(Debug)]
pub enum ShardsEvent {
    /// A message has been received.
    Message(ShardsMessage),

    /// A remote joined to a shard.
    Joined {
        /// Remote that has joined.
        peer_id: PeerId,
        /// The shard it has joined to.
        shard: Shard,
    },

    /// A remote left from a shard.
    Left {
        /// Remote that has left.
        peer_id: PeerId,
        /// The topic it has subscribed from.
        shard: Shard,
    },
}
