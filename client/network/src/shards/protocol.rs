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

use super::shard::Shard;
use libp2p::core::{InboundUpgrade, OutboundUpgrade, UpgradeInfo, PeerId, upgrade};
use prost::Message;
use std::{error, fmt, io, iter, pin::Pin};
use futures::{Future, io::{AsyncRead, AsyncWrite}};
use codec::{Encode, Decode, Input, Output, Error};
/// Implementation of `ConnectionUpgrade` for the shards protocol.
#[derive(Debug, Clone, Default)]
pub struct ShardsProtocol {}

impl ShardsProtocol {
    /// Builds a new `ShardsProtocol`.
    pub fn new() -> ShardsProtocol {
        ShardsProtocol {}
    }
}

impl UpgradeInfo for ShardsProtocol {
    type Info = &'static [u8];
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        iter::once(b"/mechain/shards/1.0")
    }
}

impl<TSocket> InboundUpgrade<TSocket> for ShardsProtocol
where
    TSocket: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = Rpc;
    type Error = ShardsDecodeError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_inbound(self, mut socket: TSocket, _: Self::Info) -> Self::Future {
        Box::pin(async move {
            let packet = upgrade::read_one(&mut socket, 2048).await?;
            let rpc = <Rpc as Decode>::decode(&mut &packet[..])?;
            Ok(rpc)
        })
    }
}

/// Reach attempt interrupt errors.
#[derive(Debug)]
pub enum ShardsDecodeError {
    /// Error when reading the packet from the socket.
    ReadError(upgrade::ReadOneError),
    /// Error when decoding the raw buffer into a protobuf.
    ProtobufError(codec::Error),
    /// Error when parsing the `PeerId` in the message.
    InvalidPeerId,
}

impl From<upgrade::ReadOneError> for ShardsDecodeError {
    fn from(err: upgrade::ReadOneError) -> Self {
        ShardsDecodeError::ReadError(err)
    }
}

impl From<codec::Error> for ShardsDecodeError {
    fn from(err: codec::Error) -> Self {
        ShardsDecodeError::ProtobufError(err)
    }
}

impl fmt::Display for ShardsDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ShardsDecodeError::ReadError(ref err) =>
                write!(f, "Error while reading from socket: {}", err),
            ShardsDecodeError::ProtobufError(ref err) =>
                write!(f, "Error while decoding protobuf: {}", err),
            ShardsDecodeError::InvalidPeerId =>
                write!(f, "Error while decoding PeerId from message"),
        }
    }
}

impl error::Error for ShardsDecodeError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            ShardsDecodeError::ReadError(ref err) => Some(err),
            ShardsDecodeError::ProtobufError(ref err) => Some(err),
            ShardsDecodeError::InvalidPeerId => None,
        }
    }
}


#[derive(Debug, Clone, PartialEq, Eq, Hash,Encode,Decode)]
pub struct  Rpc {
    /// List of messages that were part of this RPC query.
    pub messages: Vec<ShardsMessage>,
    /// List of joins.
    pub shards: Vec<ShardsJoin>,
    pub ttl:Option<u8>,
}

impl UpgradeInfo for Rpc {
    type Info = &'static [u8];
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        iter::once(b"/mechain/shards/1.0")
    }
}

impl <C> OutboundUpgrade<C> for Rpc
where C:AsyncWrite + AsyncRead + Send + Unpin + 'static,
{
    type Output = ();
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_outbound(self, mut socket: C, _: Self::Info) -> Self::Future {
        Box::pin(async move {
            let bytes = self.into_bytes();
            upgrade::write_one(&mut socket, bytes).await?;
            Ok(())
        })
    }
}

impl Rpc {
    /// Turns this `ShardsRpc` into a message that can be sent to a substream.
    fn into_bytes(self) -> Vec<u8> {
        <Rpc as Encode>::encode(&self)
    }
}

/// A message received by the shards system.
#[derive(Debug, Clone, PartialEq, Eq, Hash,Encode,Decode)]
pub struct ShardsMessage {
    /// Id of the peer that published this message.
    pub source: Vec<u8>,

    /// Content of the message. Its meaning is out of scope of this library.
    pub data: Vec<u8>,

    /// An incrementing sequence number.
    pub sequence_number: Vec<u8>,

    /// List of shards this message belongs to.
    ///
    /// Each message can belong to multiple topics at once.
    pub shards: Vec<Shard>,
}

/// A join received by the shards system.
#[derive(Debug, Clone, PartialEq, Eq, Hash,Encode,Decode)]
pub struct ShardsJoin {
    /// Action to perform.
    pub action: ShardsAction,
    /// The shard from which to join or leave.
    pub shard: Shard,
}

/// Action that a subscription wants to perform.
#[derive(Debug, Clone, PartialEq, Eq, Hash,Encode,Decode)]
pub enum ShardsAction {
    /// The remote wants to join to the given shard.
    Join,
    /// The remote wants to leave from the given shard.
    Leave,
}
