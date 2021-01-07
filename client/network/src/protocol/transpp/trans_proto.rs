use crate::protocol::generic_proto::{GenericProto,GenericProtoOut};
use futures::prelude::*;
use libp2p::{PeerId,Multiaddr};
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters, IntoProtocolsHandler, ProtocolsHandler, ProtocolsHandlerEvent, ProtocolsHandlerUpgrErr, SubstreamProtocol, KeepAlive, NegotiatedSubstream};
use futures::task::{Context, Poll};
use libp2p::core::connection::ConnectionId;
use libp2p::core::ConnectedPoint;
use smallvec::alloc::collections::VecDeque;
use void::Void;
use wasm_timer::Delay;
use bitflags::_core::time::Duration;
use futures::io::Error;

///group trans proto
pub struct TransProto {
    events:VecDeque<TransEvent>,
}

pub enum TransEventIn{
  Member
}

#[derive(Debug)]
pub enum TransEventOut{
   Member(u32)
}

pub struct  TransProtocolsHandler;

pub struct TransHandler {
    /// The timer used for the delay to the next ping as well as
    /// the ping timeout.
    timer: Delay,
}

impl TransHandler {
    pub fn  new() ->Self {
        TransHandler {
            timer:Delay::new(Duration::from_secs(5)),
        }
    }
}

impl ProtocolsHandler for TransHandler{
    type InEvent = TransEventIn;
    type OutEvent = TransEventOut;
    type Error = super::GroupFailure;
    type InboundProtocol = super::protocol::Group;
    type OutboundProtocol = super::protocol::Group;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(super::protocol::Group,())
    }

    fn inject_fully_negotiated_inbound(&mut self, stream: NegotiatedSubstream, (): ()) {
        // self.inbound = Some(protocol::recv_ping(stream).boxed());
    }

    fn inject_fully_negotiated_outbound(&mut self, stream: NegotiatedSubstream, (): ()) {
        // self.timer.reset(self.config.timeout);
        // self.outbound = Some(PingState::Ping(protocol::send_ping(stream).boxed()));
    }

    fn inject_event(&mut self, event: Self::InEvent) {

    }

    fn inject_dial_upgrade_error(&mut self, info: Self::OutboundOpenInfo, error: ProtocolsHandlerUpgrErr<Void>) {

    }

    fn connection_keep_alive(&self) -> KeepAlive {
        KeepAlive::No
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<ProtocolsHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::OutEvent, Self::Error>> {
        match self.timer.poll_unpin(cx) {
            Poll::Ready(_) => {
                let protocol = SubstreamProtocol::new(super::protocol::Group, ())
                    .with_timeout(Duration::from_secs(5));
                Poll::Ready(ProtocolsHandlerEvent::OutboundSubstreamRequest {protocol})
            }
            Poll::Pending => Poll::Pending
        }
    }
}

impl IntoProtocolsHandler for TransProtocolsHandler{
    type Handler = TransHandler;

    fn into_handler(self, remote_peer_id: &PeerId, connected_point: &ConnectedPoint) -> Self::Handler {
        TransHandler::new()
    }

    fn inbound_protocol(&self) -> <Self::Handler as ProtocolsHandler>::InboundProtocol {
        unimplemented!()
    }
}

pub enum TransEvent {
    GroupFindReq,
    GroupFindRes,
}

impl TransProto {
    pub fn new() -> Self {
        TransProto { events: Default::default() }
    }
    pub fn group_memeber_find(&mut self,group_id:impl Into<Vec<u8>>){
       self.events.push_front(TransEvent::GroupFindReq);
    }
}

impl NetworkBehaviour for TransProto {
    type ProtocolsHandler = TransHandler;
    type OutEvent = TransEvent;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        TransHandler::new()
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        Vec::new()
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {

    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {

    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        event: TransEventOut
    ) {
        log::info!("OutEvent---->{:?},{:?}",peer_id,event);
        self.events.push_front(TransEvent::GroupFindRes)
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        params: &mut impl PollParameters
    ) -> Poll<
           NetworkBehaviourAction<
               <<Self::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::InEvent,
               Self::OutEvent
           >
    > {
        if let Some(e) = self.events.pop_back() {
            Poll::Ready(NetworkBehaviourAction::GenerateEvent(e))
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
   fn hello_proto(){
       env_logger::init();
       log::trace!("....");
   }
}