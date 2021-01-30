use futures::prelude::*;
use futures::channel::mpsc::{channel, Sender, Receiver};
use libp2p::PeerId;
use log::trace;
use sp_runtime::traits::{Block as BlockT, Hash, HashFor};
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use wasm_timer::Instant;
use std::time;
use std::iter;
use crate::{Event, ReputationChange, ExHashT, NetworkService, ObservedRole};
use std::collections::HashSet;
use lru::LruCache;
use log::{error,info};

const KNOWN_MESSAGES_CACHE_SIZE: usize = 4096;

const REBROADCAST_INTERVAL: time::Duration = time::Duration::from_secs(30);

pub(crate) const PERIODIC_MAINTENANCE_INTERVAL: time::Duration = time::Duration::from_millis(1100);

/// Abstraction over a network.
pub trait GroupNetwork<B: BlockT> {
    /// Returns a stream of events representing what happens on the network.
    fn event_stream(&self) -> Pin<Box<dyn Stream<Item = Event> + Send>>;


    /// Adjust the reputation of a node.
    fn report_peer(&self, peer_id: PeerId, reputation: ReputationChange);

    /// Force-disconnect a peer.
    fn disconnect_peer(&self, who: PeerId);

    /// Send a notification to a peer.
    fn write_notification(&self, who: PeerId, protocol: Cow<'static, str>, message: Vec<u8>);

    fn leave_group(&self,group_id:String);

    fn join_group(&self,group_id:String) ->Result<(),String>;

    fn publish_message(&self,group_id:String,data:Vec<u8>);

}


impl<B: BlockT, H: ExHashT> GroupNetwork<B> for Arc<NetworkService<B, H>> {
    fn event_stream(&self) -> Pin<Box<dyn Stream<Item = Event> + Send>> {
        Box::pin(NetworkService::event_stream(self, "network-group"))
    }

    fn report_peer(&self, peer_id: PeerId, reputation: ReputationChange) {
        NetworkService::report_peer(self, peer_id, reputation);
    }

    fn disconnect_peer(&self, who: PeerId) {
        NetworkService::disconnect_peer(self, who)
    }

    fn write_notification(&self, who: PeerId, protocol: Cow<'static, str>, message: Vec<u8>) {
        NetworkService::write_notification(self, who, protocol, message)
    }

    fn leave_group(&self, group_id: String) {
        NetworkService::leave_group(self,group_id)
    }

    fn join_group(&self, group_id: String) -> Result<(), String> {
        NetworkService::join_group(self,group_id)
    }

    fn publish_message(&self, group_id: String, data: Vec<u8>) {
        NetworkService::publish_message(self,group_id,data)
    }
}


mod rep {
    use crate::ReputationChange as Rep;
    /// Reputation change when a peer sends us a gossip message that we didn't know about.
    pub const GROUP_SUCCESS: Rep = Rep::new(1 << 4, "Successfull gossip");
    /// Reputation change when a peer sends us a gossip message that we already knew about.
    pub const DUPLICATE_GROUP: Rep = Rep::new(-(1 << 2), "Duplicate gossip");
}

/// Wraps around an implementation of the `Network` crate and provides gossiping capabilities on
/// top of it.
pub struct GroupEngine<B: BlockT> {
    state_machine: ConsensusGroup<B>,
    network: Box<dyn GroupNetwork<B> + Send>,
    periodic_maintenance_interval: futures_timer::Delay,
    protocol: Cow<'static, str>,

    /// Incoming events from the network.
    network_event_stream: Pin<Box<dyn Stream<Item = Event> + Send>>,
    /// Outgoing events to the consumer.
    message_sinks: HashMap<Vec<u8>, Vec<Sender<GroupNotification>>>,
    /// Buffered messages (see [`ForwardingState`]).
    forwarding_state: ForwardingState<B>,
}

/// A gossip engine receives messages from the network via the `network_event_stream` and forwards
/// them to upper layers via the `message sinks`. In the scenario where messages have been received
/// from the network but a subscribed message sink is not yet ready to receive the messages, the
/// messages are buffered. To model this process a gossip engine can be in two states.
enum ForwardingState<B: BlockT> {
    /// The gossip engine is currently not forwarding any messages and will poll the network for
    /// more messages to forward.
    Idle,
    /// The gossip engine is in the progress of forwarding messages and thus will not poll the
    /// network for more messages until it has send all current messages into the subscribed message
    /// sinks.
    Busy(VecDeque<(B::Hash, GroupNotification)>),
}

impl<B: BlockT> Unpin for GroupEngine<B> {}

impl<B: BlockT> GroupEngine<B> {
    /// Create a new instance.
    pub fn new<N: GroupNetwork<B> + Send + Clone + 'static>(
        network: N,
        protocol: impl Into<Cow<'static, str>>,
    ) -> Self where B: 'static {
        let protocol = protocol.into();
        let network_event_stream = network.event_stream();

        GroupEngine {
            state_machine: ConsensusGroup::new(protocol.clone()),
            network: Box::new(network),
            periodic_maintenance_interval: futures_timer::Delay::new(PERIODIC_MAINTENANCE_INTERVAL),
            protocol,
            network_event_stream,
            message_sinks: HashMap::new(),
            forwarding_state: ForwardingState::Idle,
        }
    }

    // pub fn report(&self, who: PeerId, reputation: ReputationChange) {
    //     self.network.report_peer(who, reputation);
    // }

    /// Registers a message without propagating it to any peers. The message
    /// becomes available to new peers or when the service is asked to gossip
    /// the message's topic. No validation is performed on the message, if the
    /// message is already expired it should be dropped on the next garbage
    /// collection.
    pub fn register_group_message(
        &mut self,
        group: Vec<u8>,
        message: Vec<u8>,
    ) {
        self.network.join_group(hex::encode(&group[..]));
        self.state_machine.register_message(group, message);
    }

    /// Broadcast all messages with given group.
    pub fn broadcast_group(&mut self, group: Vec<u8>, force: bool) {
        self.state_machine.broadcast_group(&mut *self.network, group, force);
    }

    /// Get data of valid, incoming messages for a topic (but might have expired meanwhile).
    pub fn messages_for(&mut self, group: Vec<u8>)
                        -> Receiver<GroupNotification>
    {
        let past_messages = self.state_machine.messages_for(group.clone()).collect::<Vec<_>>();
        // The channel length is not critical for correctness. By the implementation of `channel`
        // each sender is guaranteed a single buffer slot, making it a non-rendezvous channel and
        // thus preventing direct dead-locks. A minimum channel length of 10 is an estimate based on
        // the fact that despite `NotificationsReceived` having a `Vec` of messages, it only ever
        // contains a single message.
        let (mut tx, rx) = channel(usize::max(past_messages.len(), 10));

        for notification in past_messages{
            tx.try_send(notification)
                .expect("receiver known to be live, and buffer size known to suffice; qed");
        }

        self.message_sinks.entry(group).or_default().push(tx);

        rx
    }

    /// Send all messages with given topic to a peer.
    pub fn send_group(
        &mut self,
        who: &PeerId,
        group: Vec<u8>,
        force: bool
    ) {
        self.state_machine.send_group(&mut *self.network, who, group, force)
    }

    /// Multicast a message to all peers.
    pub fn group_message(
        &mut self,
        group: Vec<u8>,
        message: Vec<u8>,
        force: bool,
    ) {
        self.state_machine.multicast(&mut *self.network, group, message, force)
    }

    /// Send addressed message to the given peers. The message is not kept or multicast
    /// later on.
    pub fn send_message(&mut self, who: Vec<crate::PeerId>, data: Vec<u8>) {
        for who in &who {
            self.state_machine.send_message(&mut *self.network, who, data.clone());
        }
    }

}

impl<B: BlockT> Future for GroupEngine<B> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = &mut *self;

        'outer: loop {
            match &mut this.forwarding_state {
                ForwardingState::Idle => {
                    match this.network_event_stream.poll_next_unpin(cx) {
                        Poll::Ready(Some(event)) => match event {
                            Event::NotificationStreamOpened { remote, protocol, role } => {
                                if protocol != this.protocol {
                                    continue;
                                }
                                this.state_machine.new_peer(&mut *this.network, remote, role);
                            }
                            Event::NotificationStreamClosed { remote, protocol } => {
                                if protocol != this.protocol {
                                    continue;
                                }
                                this.state_machine.peer_disconnected(&mut *this.network, remote);
                            },
                            Event::NotificationsReceived { remote, messages } => {
                                let messages = messages.into_iter().filter_map(|(engine, data)| {
                                    if engine == this.protocol {
                                        Some(data.to_vec())
                                    } else {
                                        None
                                    }
                                }).collect();

                                let to_forward = this.state_machine.on_incoming(
                                    &mut *this.network,
                                    remote,
                                    messages,
                                );

                                this.forwarding_state = ForwardingState::Busy(to_forward.into());
                            },
                            Event::Dht(_) => {}
                        }
                        // The network event stream closed. Do the same for [`GossipValidator`].
                        Poll::Ready(None) => return Poll::Ready(()),
                        Poll::Pending => break,
                    }
                }
                ForwardingState::Busy(to_forward) => {
                    let (group, notification) = match to_forward.pop_front() {
                        Some(n) => n,
                        None => {
                            this.forwarding_state = ForwardingState::Idle;
                            continue;
                        }
                    };

                    let sinks = match this.message_sinks.get_mut(&group) {
                        Some(sinks) => sinks,
                        None => {
                            continue;
                        },
                    };

                    // Make sure all sinks for the given topic are ready.
                    for sink in sinks.iter_mut() {
                        match sink.poll_ready(cx) {
                            Poll::Ready(Ok(())) => {},
                            // Receiver has been dropped. Ignore for now, filtered out in (1).
                            Poll::Ready(Err(_)) => {},
                            Poll::Pending => {
                                // Push back onto queue for later.
                                to_forward.push_front((group, notification));
                                break 'outer;
                            }
                        }
                    }

                    // Filter out all closed sinks.
                    sinks.retain(|sink| !sink.is_closed()); // (1)

                    if sinks.is_empty() {
                        this.message_sinks.remove(&group);
                        continue;
                    }

                    trace!(
                        target: "group",
                        "Pushing consensus message to sinks for {}.", group,
                    );

                    // Send the notification on each sink.
                    for sink in sinks {
                        match sink.start_send(notification.clone()) {
                            Ok(()) => {},
                            Err(e) if e.is_full() => unreachable!(
                                "Previously ensured that all sinks are ready; qed.",
                            ),
                            // Receiver got dropped. Will be removed in next iteration (See (1)).
                            Err(_) => {},
                        }
                    }
                }
            }
        }


        while let Poll::Ready(()) = this.periodic_maintenance_interval.poll_unpin(cx) {
            this.periodic_maintenance_interval.reset(PERIODIC_MAINTENANCE_INTERVAL);
            this.state_machine.tick(&mut *this.network);

            this.message_sinks.retain(|_, sinks| {
                sinks.retain(|sink| !sink.is_closed());
                !sinks.is_empty()
            });
        }

        Poll::Pending
    }
}



struct PeerConsensus<H> {
    known_messages: HashSet<H>,
}

/// Topic stream message with sender.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupNotification {
    /// Message data.
    pub message: Vec<u8>,
    /// Sender if available.
    pub sender: Option<PeerId>,
}

struct MessageEntry<B: BlockT> {
    message_hash: B::Hash,
    group: Vec<u8>,
    message: Vec<u8>,
    sender: Option<PeerId>,
    create_time:time::Instant,
}


/// The reason for sending out the message.
#[derive(Eq, PartialEq, Copy, Clone)]
#[cfg_attr(test, derive(Debug))]
pub enum MessageIntent {
    /// Requested broadcast.
    Broadcast,
    /// Requested broadcast to all peers.
    ForcedBroadcast,
    /// Periodic rebroadcast of all messages to all peers.
    PeriodicRebroadcast,
}

fn propagate<'a, B: BlockT, I>(
    network: &mut dyn GroupNetwork<B>,
    protocol: Cow<'static, str>,
    messages: I,
    intent: MessageIntent,
    peers: &mut HashMap<PeerId, PeerConsensus<B::Hash>>,
)
    where I: Clone + IntoIterator<Item=(&'a B::Hash, &'a B::Hash, &'a Vec<u8>)>,
{

    for (id, ref mut peer) in peers.iter_mut() {
        for (message_hash, _group, message) in messages.clone() {
            let intent = match intent {
                MessageIntent::Broadcast { .. } =>
                    if peer.known_messages.contains(&message_hash) {
                        continue;
                    } else {
                        MessageIntent::Broadcast
                    },
                MessageIntent::PeriodicRebroadcast =>
                    if peer.known_messages.contains(&message_hash) {
                        MessageIntent::PeriodicRebroadcast
                    } else {
                        // peer doesn't know message, so the logic should treat it as an
                        // initial broadcast.
                        MessageIntent::Broadcast
                    },
                other => other,
            };


            peer.known_messages.insert(message_hash.clone());

            trace!(target: "group", "Propagating to {}: {:?}", id, message);
            network.write_notification(id.clone(), protocol.clone(), message.clone());
        }
    }
}



/// Consensus network protocol handler. Manages statements and candidate requests.
pub struct ConsensusGroup<B: BlockT> {
    peers: HashMap<PeerId, PeerConsensus<B::Hash>>,
    messages: Vec<MessageEntry<B>>,
    known_messages: LruCache<B::Hash, ()>,
    protocol: Cow<'static, str>,
    next_broadcast: Instant,
}

impl<B: BlockT> ConsensusGroup<B> {
    /// Create a new instance using the given validator.
    pub fn new(protocol: Cow<'static, str>) -> Self {
        ConsensusGroup {
            peers: HashMap::new(),
            messages: Default::default(),
            known_messages: LruCache::new(KNOWN_MESSAGES_CACHE_SIZE),
            protocol,
            next_broadcast: Instant::now() + REBROADCAST_INTERVAL,
        }
    }

    /// Handle new connected peer.
    pub fn new_peer(&mut self, network: &mut dyn GroupNetwork<B>, who: PeerId, role: ObservedRole) {
        // light nodes are not valid targets for consensus gossip messages
        if role.is_light() {
            return;
        }

        trace!(target:"gossip", "Registering {:?} {}", role, who);
        self.peers.insert(who.clone(), PeerConsensus {
            known_messages: HashSet::new(),
        });
    }

    fn register_message_hashed(
        &mut self,
        message_hash: B::Hash,
        group: Vec<u8>,
        message: Vec<u8>,
        sender: Option<PeerId>,
    ) {
        if self.known_messages.put(message_hash.clone(), ()).is_none() {
            let create_time = time::Instant::now();
            self.messages.push(MessageEntry {
                message_hash,
                group,
                message,
                sender,
                create_time,
            });
        }
    }

    /// Registers a message without propagating it to any peers. The message
    /// becomes available to new peers or when the service is asked to gossip
    /// the message's topic. No validation is performed on the message, if the
    /// message is already expired it should be dropped on the next garbage
    /// collection.
    pub fn register_message(
        &mut self,
        group: Vec<u8>,
        message: Vec<u8>,
    ) {
        let message_hash = HashFor::<B>::hash(&message[..]);
        self.register_message_hashed(message_hash, group, message, None);
    }

    /// Call when a peer has been disconnected to stop tracking gossip status.
    pub fn peer_disconnected(&mut self, network: &mut dyn GroupNetwork<B>, who: PeerId) {

        self.peers.remove(&who);
    }

    /// Perform periodic maintenance
    pub fn tick(&mut self, network: &mut dyn GroupNetwork<B>) {
        self.collect_garbage();
        if Instant::now() >= self.next_broadcast {
            self.rebroadcast(network);
            self.next_broadcast = Instant::now() + REBROADCAST_INTERVAL;
        }
    }

    /// Rebroadcast all messages to all peers.
    fn rebroadcast(&mut self, network: &mut dyn GroupNetwork<B>) {
        let messages = self.messages.iter()
            .map(|entry| (&entry.message_hash, &entry.group, &entry.message));
        propagate(
            network,
            self.protocol.clone(),
            messages,
            MessageIntent::PeriodicRebroadcast,
            &mut self.peers,
        );
    }

    /// Broadcast all messages with given topic.
    pub fn broadcast_group(&mut self, network: &mut dyn GroupNetwork<B>, group: Vec<u8>, force: bool) {
        let messages = self.messages.iter()
            .filter_map(|entry|
                if entry.group == group {
                    Some((&entry.message_hash, &entry.group, &entry.message))
                } else { None }
            );
        for (_,g,msg) in messages {
            network.publish_message(hex::encode(g),msg.clone());
        }
        // let intent = if force { MessageIntent::ForcedBroadcast } else { MessageIntent::Broadcast };
        // propagate(network, self.protocol.clone(), messages, intent, &mut self.peers);
    }

    /// Prune old or no longer relevant consensus messages. Provide a predicate
    /// for pruning, which returns `false` when the items with a given topic should be pruned.
    pub fn collect_garbage(&mut self) {
        let known_messages = &mut self.known_messages;
        let before = self.messages.len();

        self.messages.retain(|entry| entry.create_time.elapsed() < time::Duration::from_secs(120));

        trace!(target: "group", "Cleaned up {} stale messages, {} left ({} known)",
               before - self.messages.len(),
               self.messages.len(),
               known_messages.len(),
        );

        for (_, ref mut peer) in self.peers.iter_mut() {
            peer.known_messages.retain(|h| known_messages.contains(h));
        }
    }

    /// Get valid messages received in the past for a topic (might have expired meanwhile).
    pub fn messages_for(&mut self, group: Vec<u8>) -> impl Iterator<Item = GroupNotification> + '_ {
        self.messages.iter().filter(move |e| e.group == group).map(|entry| GroupNotification {
            message: entry.message.clone(),
            sender: entry.sender.clone(),
        })
    }

    /// Register incoming messages and return the ones that are new and valid (according to a gossip
    /// validator) and should thus be forwarded to the upper layers.
    pub fn on_incoming(
        &mut self,
        network: &mut dyn GroupNetwork<B>,
        who: PeerId,
        messages: Vec<Vec<u8>>,
    ) -> Vec<(B::Hash, GroupNotification)> {
        let mut to_forward = vec![];

        if !messages.is_empty() {
            trace!(target: "group", "Received {} messages from peer {}", messages.len(), who);
        }

        for message in messages {
            let message_hash = HashFor::<B>::hash(&message[..]);

            if self.known_messages.contains(&message_hash) {
                trace!(target:"group", "Ignored already known message from {}", who);
                // network.report_peer(who.clone(), rep::DUPLICATE_GROUP);
                continue;
            }


            let peer = match self.peers.get_mut(&who) {
                Some(peer) => peer,
                None => {
                    error!(target:"group", "Got message from unregistered peer {}", who);
                    continue;
                }
            };

            // network.report_peer(who.clone(), rep::GROUP_SUCCESS);
            peer.known_messages.insert(message_hash);
            // to_forward.push((group, GroupNotification {
            //     message: message.clone(),
            //     sender: Some(who.clone())
            // }));
            // self.register_message_hashed(
            //     message_hash,
            //     group,
            //     message,
            //     Some(who.clone()),
            // );

        }

        to_forward
    }

    /// Send all messages with given topic to a peer.
    pub fn send_group(
        &mut self,
        network: &mut dyn GroupNetwork<B>,
        who: &PeerId,
        group: Vec<u8>,
        force: bool
    ) {

        if let Some(ref mut peer) = self.peers.get_mut(who) {
            for entry in self.messages.iter().filter(|m| m.group == group) {
                let intent = if force {
                    MessageIntent::ForcedBroadcast
                } else {
                    MessageIntent::Broadcast
                };

                if !force && peer.known_messages.contains(&entry.message_hash) {
                    continue;
                }

                peer.known_messages.insert(entry.message_hash.clone());

                trace!(target: "group", "Sending topic message to {}: {:?}", who, entry.message);
                // network.write_notification(who.clone(), self.protocol.clone(), entry.message.clone());
            }
        }
    }

    /// Multicast a message to all peers.
    pub fn multicast(
        &mut self,
        network: &mut dyn GroupNetwork<B>,
        group: Vec<u8>,
        message: Vec<u8>,
        force: bool,
    ) {
        let message_hash = HashFor::<B>::hash(&message);
        self.register_message_hashed(message_hash, group.clone(), message.clone(), None);
        network.publish_message(hex::encode(group),message);
        // let intent = if force { MessageIntent::ForcedBroadcast } else { MessageIntent::Broadcast };
        // propagate(
        //     network,
        //     self.protocol.clone(),
        //     iter::once((&message_hash, &group, &message)),
        //     intent,
        //     &mut self.peers,
        // );
    }

    /// Send addressed message to a peer. The message is not kept or multicast
    /// later on.
    pub fn send_message(
        &mut self,
        network: &mut dyn GroupNetwork<B>,
        who: &PeerId,
        message: Vec<u8>,
    ) {
        let peer = match self.peers.get_mut(who) {
            None => return,
            Some(peer) => peer,
        };

        let message_hash = HashFor::<B>::hash(&message);

        trace!(target: "group", "Sending direct to {}: {:?}", who, message);

        peer.known_messages.insert(message_hash);
        network.write_notification(who.clone(), self.protocol.clone(), message);
    }
}
