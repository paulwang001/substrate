use libp2p::core::Multiaddr;
use libp2p::swarm::Swarm;
use crate::shards::{Shards, Shard, ShardsEvent};
use libp2p::core::identity;
use libp2p::core::multiaddr::Protocol;
use libp2p::core::transport::{MemoryTransport, upgrade};
use libp2p::tcp::TcpConfig;
use libp2p::{Transport, yamux,noise};
use rand::{random, RngCore};
use libp2p::noise::{X25519, Keypair,NoiseConfig};
use futures::{FutureExt, SinkExt, StreamExt};
use futures::channel::mpsc;
use libp2p::core::PeerId;
use bitflags::_core::time::Duration;
use std::sync::{Arc, Mutex};
use std::pin::Pin;

fn build_node(port:u64) -> (Multiaddr, Swarm<Shards>,PeerId) {
    let key = identity::Keypair::generate_ed25519();
    let public_key = key.public();
    let dh_keys = Keypair::<X25519>::new().into_authentic(&key).unwrap();
    let noise = NoiseConfig::xx(dh_keys).into_authenticated();

    let transport = TcpConfig::default()
        .upgrade(upgrade::Version::V1)
        .authenticate(noise)
        .multiplex(yamux::YamuxConfig::default()).boxed();


    let peer_id = public_key.clone().into_peer_id();

    // NOTE: The graph of created nodes can be disconnected from the mesh point of view as nodes
    // can reach their d_lo value and not add other nodes to their mesh. To speed up this test, we
    // reduce the default values of the heartbeat, so that all nodes will receive gossip in a
    // timely fashion.

    let behaviour = Shards::new(peer_id.clone());
    let mut swarm = Swarm::new(transport, behaviour, peer_id.clone());

    let mut addr: Multiaddr = format!("/ip4/127.0.0.1/tcp/{}",port).parse().unwrap();
    log::info!("{:?}",&addr);
    Swarm::listen_on(&mut swarm, addr.clone()).unwrap();

    // addr = addr.with(libp2p::core::multiaddr::Protocol::P2p(
    //     public_key.into_peer_id().into(),
    // ));
    (addr, swarm,peer_id)
}

#[test]
fn shard_test() {
    env_logger::init();
    log::warn!("============================");


    let (mut tx,mut rx) =mpsc::channel(1);
    let (addr_d,mut node_d,peer_id_d) = build_node(6001);
    let (addr_c,mut node_c,peer_id_c) = build_node(6002);
    let (addr_b,mut node_b,peer_id_b) = build_node(6003);
    let (addr_a,mut node_a,peer_id_a) = build_node(6004);

    log::warn!("d-peer:{:?}",peer_id_d.clone());
    log::warn!("c-peer:{:?}",peer_id_c.clone());
    log::warn!("b-peer:{:?}",peer_id_b.clone());
    log::warn!("a-peer:{:?}",peer_id_a.clone());
    log::warn!("=============================");

    node_d.join(Shard::new("G1"));
    node_d.add_known_peer(peer_id_a.clone(),vec![addr_a]);
    node_d.add_node_to_partial_view(peer_id_a.clone());

    node_c.join(Shard::new("G1"));
    node_c.add_known_peer(peer_id_d.clone(),vec![addr_d]);
    node_c.add_node_to_partial_view(peer_id_d);


    node_b.join(Shard::new("G1"));
    node_b.add_known_peer(peer_id_c.clone(),vec![addr_c]);
    node_b.add_node_to_partial_view(peer_id_c);



    node_a.join(Shard::new("G1"));
    node_a.add_known_peer(peer_id_b.clone(),vec![addr_b]);
    node_a.add_node_to_partial_view(peer_id_b.clone());


    async_std::task::spawn(async move{
        loop {
            match node_d.next().await {
                ShardsEvent::Message(msg) => {
                    log::warn!("d-->message-->{:?}",msg);
                }
                ShardsEvent::Joined { peer_id, shard } => {
                    log::info!("d-->joined--->peer:{:?},shard:{:?}",peer_id,shard);
                }
                ShardsEvent::Left { .. } => {

                }
            }
        }
    });

    async_std::task::spawn(async move{
        loop {
            match node_c.next().await {
                ShardsEvent::Message(msg) => {
                    log::warn!("c-->message-->{:?}",msg);
                }
                ShardsEvent::Joined { peer_id, shard } => {
                    log::info!("c-->joined--->peer:{:?},shard:{:?}",peer_id,shard);
                }
                ShardsEvent::Left { .. } => {

                }
            }
        }
    });

    async_std::task::spawn(async move{
        loop {
            match node_b.next().await {
                ShardsEvent::Message(msg) => {
                    log::warn!("b-->message-->{:?}",msg);
                }
                ShardsEvent::Joined { peer_id, shard } => {
                    log::info!("b-->joined--->peer:{:?},shard:{:?}",peer_id,shard);

                }
                ShardsEvent::Left { .. } => {

                }
            }
        }
    });

    async_std::task::spawn(async move{

        loop {
            if let Ok(Some(p)) = rx.try_next() {
                let mut rng = rand::thread_rng();
                log::warn!("------------------>publish to :{:?}",p);
                let mut data=vec![0_u8;128];
                rng.fill_bytes(&mut data);
                node_a.publish(Shard::new("G1"),data);
            }

            match node_a.next().await {
                ShardsEvent::Message(msg) => {
                    log::warn!("a-->message-->{:?}",msg);
                }
                ShardsEvent::Joined { peer_id, shard } => {
                    log::info!("a-->joined--->peer:{:?},shard:{:?}",peer_id,shard);

                }
                ShardsEvent::Left { .. } => {

                }
            }
        }
    });
    //
    // async_std::task::spawn(async move{
    //
    // });
    async_std::task::block_on(async move{
        loop {
            async_std::task::sleep(Duration::from_secs(10)).await;
            tx.send(peer_id_a.clone()).await.expect("");
        }

    });

}