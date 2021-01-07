use crate::protocol::generic_proto::{GenericProto,GenericProtoOut};

use futures::prelude::*;
use libp2p::{PeerId, Multiaddr, Transport};
use libp2p::core::{
    connection::{ConnectionId, ListenerId},
    ConnectedPoint,
    transport::MemoryTransport,
    upgrade
};
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters, IntoProtocolsHandler, ProtocolsHandler};
use futures::task::{Context, Poll};
use crate::protocol::generic_proto::handler::{NotifsHandlerOut, NotifsHandlerIn};
use core::iter;
use smallvec::alloc::collections::VecDeque;
use wasm_timer::Interval;
use bitflags::_core::time::Duration;
use sc_peerset::Peerset;
use crate::protocol::message::Message;
use crate::protocol::{BlockAnnouncesHandshake, CustomMessageOutcome, Fallback};
use crate::config::ProtocolId;

pub struct GroupProto {
    pub local_peer_id:PeerId,
    inner: GenericProto,
    events:VecDeque<GroupEventOut>,
    timer: Interval,
    best_number: Option<u64>,
    join_timer: Interval,
}

impl GroupProto {
    pub fn new(
        local_peer_id:PeerId,
        peerset:Peerset,
        protocol:impl Into<ProtocolId>
    ) -> Self {
        GroupProto {
            local_peer_id:local_peer_id.clone(),
            inner: GenericProto::new(
                local_peer_id,
                protocol,
                &vec![1],
                vec![],
                peerset,
                iter::once(("/group/1".into(), Vec::new())),
            ),
            events:VecDeque::new(),
            timer:Interval::new(Duration::from_secs(60)),
            best_number:Some(0),
            join_timer:Interval::new(Duration::from_secs(30)),
        }
    }

    /// Returns the list of all the peers we have an open channel to.
    pub fn open_peers(&self) -> impl Iterator<Item = &PeerId> {
        self.inner.open_peers()
    }

}

#[derive(Debug)]
pub enum GroupEventOut{
    Message {
        from_id:Vec<u8>,
        group_id:Vec<u8>,
        data:Vec<u8>,
    },
    Sending {
        target:PeerId,
        data:Vec<u8>,
    },
    Join {
      group_id:u64,
      peer_id:PeerId,
    },
    Number {
      best:u64
    },
    None
}

impl NetworkBehaviour for GroupProto{
    type ProtocolsHandler = <GenericProto as NetworkBehaviour>::ProtocolsHandler;
    type OutEvent = GroupEventOut;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        self.inner.new_handler()
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        self.inner.addresses_of_peer(peer_id)
    }

    fn inject_connection_established(&mut self, peer_id: &PeerId, conn: &ConnectionId, endpoint: &ConnectedPoint) {
        self.inner.inject_connection_established(peer_id, conn, endpoint)

    }

    fn inject_connection_closed(&mut self, peer_id: &PeerId, conn: &ConnectionId, endpoint: &ConnectedPoint) {
        self.inner.inject_connection_closed(peer_id, conn, endpoint)
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {

        log::warn!("inject_connected:{:?}",peer_id);
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        log::warn!("inject_disconnected:{:?}",peer_id);
    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        event: <<Self::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::OutEvent
    ) {
        match event {
            NotifsHandlerOut::Notification{ protocol_name, message } => {

                log::warn!("From:{:?},Notifi---->{:?},{},{}",self.local_peer_id,peer_id,protocol_name,message.len());
            },
            NotifsHandlerOut::CustomMessage { message } =>{
                log::warn!("CustomMessage---->{:?},{}",peer_id,message.len());
            },
            _=>{
                log::warn!("OutEvent---->{:?},{:?}",peer_id,event);
                self.inner.inject_event(peer_id, connection, event)
            }
        }
        // self.inner.inject_event(peer_id, connection, event)
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        params: &mut impl PollParameters
    ) -> Poll<
        NetworkBehaviourAction<
            <<Self::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::InEvent,
            Self::OutEvent>
        > {
        // self.inner.write_notification()
        loop {
            if let Some(ev) = self.events.pop_front() {
                return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
            }
            if let Poll::Ready(Some(())) = &self.timer.poll_next_unpin(cx) {

                return Poll::Ready(NetworkBehaviourAction::GenerateEvent(
                    GroupEventOut::Message {
                        from_id: self.local_peer_id.clone().into_bytes(),
                        group_id: self.local_peer_id.clone().to_base58().into_bytes(),
                        data: b"hello world".to_vec()
                    }
                ));
            }
            if let Poll::Ready(Some(())) = &self.join_timer.poll_next_unpin(cx) {
                {
                    if self.best_number.is_some() {
                        self.best_number = Some(self.best_number.unwrap() + 1);
                    }
                }

                return Poll::Ready(NetworkBehaviourAction::GenerateEvent(
                    GroupEventOut::Join {
                        group_id: self.best_number.unwrap_or(0) % 3,
                        peer_id: self.local_peer_id.clone()
                    }
                ));
            }

            break;
        }
        let event = match self.inner.poll(cx, params) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev)) => ev,
            Poll::Ready(NetworkBehaviourAction::DialAddress { address }) =>
                return Poll::Ready(NetworkBehaviourAction::DialAddress { address }),
            Poll::Ready(NetworkBehaviourAction::DialPeer { peer_id, condition }) =>
                return Poll::Ready(NetworkBehaviourAction::DialPeer { peer_id, condition }),
            Poll::Ready(NetworkBehaviourAction::NotifyHandler { peer_id, handler, event }) =>
                return Poll::Ready(NetworkBehaviourAction::NotifyHandler { peer_id, handler, event }),
            Poll::Ready(NetworkBehaviourAction::ReportObservedAddr { address, score }) =>
                return Poll::Ready(NetworkBehaviourAction::ReportObservedAddr { address, score }),
        };
        let outcome =
        match event {
            GenericProtoOut::CustomProtocolOpen { peer_id, received_handshake, notifications_sink, .. } => {
                log::warn!("---------------CustomProtocolOpen:{:?}",peer_id);
                GroupEventOut::None
            }
            GenericProtoOut::CustomProtocolReplaced { peer_id, notifications_sink, .. } => {
                GroupEventOut::None
            },
            GenericProtoOut::CustomProtocolClosed { peer_id } => {
                // self.on_peer_disconnected(peer_id)
                log::warn!("CustomProtocolClosed:{:?}",peer_id);
                GroupEventOut::None
            },
            GenericProtoOut::LegacyMessage { peer_id, message } =>
                // self.on_custom_message(peer_id, message)
                GroupEventOut::None
            ,
            GenericProtoOut::Notification { peer_id, protocol_name, message } =>{
                log::warn!("{:?}",protocol_name);

                GroupEventOut::Sending {
                    target:peer_id,
                    data: message.to_vec()
                }
            }
        };

        if let GroupEventOut::None = outcome {
            Poll::Pending
        } else {
            log::warn!("outcome---->{:?}",&outcome);
            Poll::Ready(NetworkBehaviourAction::GenerateEvent(outcome))
        }

    }
}


impl std::ops::Deref for GroupProto {
    type Target = GenericProto;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::ops::DerefMut for GroupProto {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
