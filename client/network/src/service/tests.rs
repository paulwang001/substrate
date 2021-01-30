// This file is part of Substrate.

// Copyright (C) 2017-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use crate::{config, Event, NetworkService, NetworkWorker};

use libp2p::PeerId;
use futures::prelude::*;
use sp_runtime::traits::{Block as BlockT, Header as _,HashFor};
use std::{borrow::Cow, sync::Arc, time::Duration};
use substrate_test_runtime_client::{TestClientBuilder, TestClientBuilderExt as _};
use codec::{Encode, Decode};
use std::thread::Thread;
use crate::service::GroupRoles;
use futures::channel::mpsc::{unbounded,UnboundedSender,UnboundedReceiver};
use futures::task::Poll;
use rand::prelude::SliceRandom;
use rand::RngCore;

type TestNetworkService = NetworkService<
	substrate_test_runtime_client::runtime::Block,
	substrate_test_runtime_client::runtime::Hash,
>;

/// Builds a full node to be used for testing. Returns the node service and its associated events
/// stream.
///
/// > **Note**: We return the events stream in order to not possibly lose events between the
/// >			construction of the service and the moment the events stream is grabbed.
fn build_test_full_node(config: config::NetworkConfiguration,pool:futures::executor::ThreadPool)
	-> (Arc<TestNetworkService>, impl Stream<Item = Event>)
{
	let client = Arc::new(
		TestClientBuilder::with_default_backend()
			.build_pool_longest_chain(pool.clone())
			.0,
	);

	#[derive(Clone)]
	struct PassThroughVerifier(bool);
	impl<B: BlockT> sp_consensus::import_queue::Verifier<B> for PassThroughVerifier {
		fn verify(
			&mut self,
			origin: sp_consensus::BlockOrigin,
			header: B::Header,
			justification: Option<sp_runtime::Justification>,
			body: Option<Vec<B::Extrinsic>>,
		) -> Result<
			(
				sp_consensus::BlockImportParams<B, ()>,
				Option<Vec<(sp_blockchain::well_known_cache_keys::Id, Vec<u8>)>>,
			),
			String,
		> {
			let maybe_keys = header
				.digest()
				.log(|l| {
					l.try_as_raw(sp_runtime::generic::OpaqueDigestItemId::Consensus(b"aura"))
						.or_else(|| {
							l.try_as_raw(sp_runtime::generic::OpaqueDigestItemId::Consensus(b"babe"))
						})
				})
				.map(|blob| {
					vec![(
						sp_blockchain::well_known_cache_keys::AUTHORITIES,
						blob.to_vec(),
					)]
				});

			let mut import = sp_consensus::BlockImportParams::new(origin, header);
			import.body = body;
			import.finalized = self.0;
			import.justification = justification;
			import.fork_choice = Some(sp_consensus::ForkChoiceStrategy::LongestChain);
			Ok((import, maybe_keys))
		}
	}

	let import_queue = Box::new(sp_consensus::import_queue::BasicQueue::new(
		PassThroughVerifier(false),
		Box::new(client.clone()),
		None,
		&sp_core::testing::TaskExecutor::new_with_pool(pool.clone()),
		None,
	));

	let worker = NetworkWorker::new(config::Params {
		role: config::Role::Full,
		executor: None,
		network_config: config,
		chain: client.clone(),
		on_demand: None,
		transaction_pool: Arc::new(crate::config::EmptyTransactionPool),
		protocol_id: config::ProtocolId::from("shard"),
		import_queue,
		block_announce_validator: Box::new(
			sp_consensus::block_validation::DefaultBlockAnnounceValidator,
		),
		metrics_registry: Some(prometheus_endpoint::Registry::default()),
	})
	.unwrap();

	let service = worker.service().clone();
	let event_stream = service.event_stream("test");

	async_std::task::spawn(async move {
		futures::pin_mut!(worker);
		let _ = worker.await;
	});

	(service, event_stream)
}

fn build_pool()->futures::executor::ThreadPool{
	futures::executor::ThreadPoolBuilder::new().pool_size(48).create().expect("Failed to create thread pool!!!!")
}

const PROTOCOL_NAME: Cow<'static, str> = Cow::Borrowed("/foo");
const PROTOCOL_TEST: Cow<'static, str> = Cow::Borrowed("/group/1");

/// Builds two nodes and their associated events stream.
/// The nodes are connected together and have the `PROTOCOL_NAME` protocol registered.
fn build_nodes_one_proto()
	-> (Arc<TestNetworkService>, impl Stream<Item = Event>, Arc<TestNetworkService>, impl Stream<Item = Event>)
{
	let listen_addr = config::build_multiaddr![Memory(rand::random::<u64>())];
	let pool = build_pool();
	let (node1, events_stream1) = build_test_full_node(config::NetworkConfiguration {
		notifications_protocols: vec![PROTOCOL_NAME],
		listen_addresses: vec![listen_addr.clone()],
		transport: config::TransportConfig::MemoryOnly,
		.. config::NetworkConfiguration::new_local()
	},pool.clone());

	let (node2, events_stream2) = build_test_full_node(config::NetworkConfiguration {
		notifications_protocols: vec![PROTOCOL_NAME],
		listen_addresses: vec![],
		reserved_nodes: vec![config::MultiaddrWithPeerId {
			multiaddr: listen_addr,
			peer_id: node1.local_peer_id().clone(),
		}],
		transport: config::TransportConfig::MemoryOnly,
		.. config::NetworkConfiguration::new_local()
	},pool);

	(node1, events_stream1, node2, events_stream2)
}

fn build_group_nodes(
	group_count:u32,
	group_node_count:u32,
	boot_watch:Option<UnboundedSender<Vec<u8>>>,
	node_watch:Option<UnboundedSender<Vec<u8>>>,
)
	// -> Box<(dyn IntoIterator<Item=impl Stream<Item=Event>>, dyn IntoIterator<Item=(PeerId,Arc<TestNetworkService>)>)>
    -> Option<(Vec<(Arc<TestNetworkService>,impl Stream<Item=Event>)>,Vec<(PeerId,Arc<TestNetworkService>)>)>
{
	let mut groups = vec![];
	let pool = build_pool();
	let b_watch = boot_watch.unwrap();
	for g in 0..group_count {
		let watch = b_watch.clone();

		let listen_addr = config::build_multiaddr![Ip4([192, 168, 1, 27]), Tcp(4000_u16 + g as u16)];
		let (mut node1, events_stream1) = build_test_full_node(config::NetworkConfiguration {
			notifications_protocols: vec![PROTOCOL_TEST],
			listen_addresses: vec![listen_addr.clone()],
			in_peers:8,
			out_peers:8,
			transport: config::TransportConfig::Normal {
				enable_mdns:false,
				allow_private_ipv4:true,
				wasm_external_transport:None,
			},
			request_response_protocols:vec![],
			.. config::NetworkConfiguration::new_local()
		},pool.clone());
		node1.set_group_role(GroupRoles::Relay);
		// node1.set_watcher(Some(watch));
		groups.push((node1,events_stream1,listen_addr));
	}
	let mut nodes = vec![];
	let mut gx = 0_u16;
	let n_watch = node_watch.unwrap();
	for (node1,_,group_addr) in groups.iter() {
		let local = &node1.local_peer_id;
		let group_id = format!("shard{}0",gx);
		node1.join_group(group_id.clone()).unwrap();
		async_std::task::sleep(Duration::from_millis(20));

		for x in 0..group_node_count {
			let watch = n_watch.clone();
			// let listen1_addr = config::build_multiaddr![Ip4([0, 0, 0, 0]), Tcp(0_u16)];
			let listen1_addr = config::build_multiaddr![Ip4([192, 168, 1, 27]), Tcp(8000_u16 + (gx * 100) + x as u16)];
			let (mut node2, _events_stream2) = build_test_full_node(config::NetworkConfiguration {
				notifications_protocols: vec![PROTOCOL_TEST],
				listen_addresses: vec![listen1_addr.clone()],
				in_peers:8,
				out_peers:8,
				// boot_nodes:vec![config::MultiaddrWithPeerId {
				// 		multiaddr: group_addr.clone(),
				// 		peer_id: local.clone(),
				// 	}
				// ],
				reserved_nodes: vec![config::MultiaddrWithPeerId {
					multiaddr: group_addr.clone(),
					peer_id: local.clone(),
				}],
				transport: config::TransportConfig::Normal {
					enable_mdns:false,
					allow_private_ipv4:true,
					wasm_external_transport:None,
				},
				.. config::NetworkConfiguration::new_local()
			},pool.clone());
			node2.set_group_role(GroupRoles::Shard);
			async_std::task::sleep(Duration::from_millis(20));
			let group_id = format!("shard{}{}",gx,x % 4);
			node2.join_group(group_id).unwrap();
			// node2.set_watcher(Some(watch));
			nodes.push((local.clone(),node2));
		}
		gx +=1;
	}
    let groups = groups.into_iter().map(|(boot,stream,_)|(boot,stream)).collect::<Vec<_>>();

	Some((groups,nodes))
}

#[ignore]
#[test]
fn notifications_state_consistent() {
	env_logger::init();
	// Runs two nodes and ensures that events are propagated out of the API in a consistent
	// correct order, which means no notification received on a closed substream.

	let (node1, mut events_stream1, node2, mut events_stream2) = build_nodes_one_proto();

	// Write some initial notifications that shouldn't get through.
	for _ in 0..(rand::random::<u8>() % 5) {
		node1.write_notification(node2.local_peer_id().clone(), PROTOCOL_NAME, b"hello world".to_vec());
	}
	for _ in 0..(rand::random::<u8>() % 5) {
		node2.write_notification(node1.local_peer_id().clone(), PROTOCOL_NAME, b"hello world".to_vec());
	}

	async_std::task::block_on(async move {
		// True if we have an active substream from node1 to node2.
		let mut node1_to_node2_open = false;
		// True if we have an active substream from node2 to node1.
		let mut node2_to_node1_open = false;
		// We stop the test after a certain number of iterations.
		let mut iterations = 0;
		// Safe guard because we don't want the test to pass if no substream has been open.
		let mut something_happened = false;

		loop {
			iterations += 1;
			if iterations >= 1_000 {
				assert!(something_happened);
				break;
			}

			// Start by sending a notification from node1 to node2 and vice-versa. Part of the
			// test consists in ensuring that notifications get ignored if the stream isn't open.
			if rand::random::<u8>() % 5 >= 3 {
				node1.write_notification(node2.local_peer_id().clone(), PROTOCOL_NAME, b"hello world".to_vec());
			}
			if rand::random::<u8>() % 5 >= 3 {
				node2.write_notification(node1.local_peer_id().clone(), PROTOCOL_NAME, b"hello world".to_vec());
			}

			// Also randomly disconnect the two nodes from time to time.
			if rand::random::<u8>() % 20 == 0 {
				node1.disconnect_peer(node2.local_peer_id().clone());
			}
			if rand::random::<u8>() % 20 == 0 {
				node2.disconnect_peer(node1.local_peer_id().clone());
			}

			// Grab next event from either `events_stream1` or `events_stream2`.
			let next_event = {
				let next1 = events_stream1.next();
				let next2 = events_stream2.next();
				// We also await on a small timer, otherwise it is possible for the test to wait
				// forever while nothing at all happens on the network.
				let continue_test = futures_timer::Delay::new(Duration::from_millis(20));
				match future::select(future::select(next1, next2), continue_test).await {
					future::Either::Left((future::Either::Left((Some(ev), _)), _)) =>
						future::Either::Left(ev),
					future::Either::Left((future::Either::Right((Some(ev), _)), _)) =>
						future::Either::Right(ev),
					future::Either::Right(_) => continue,
					_ => break,
				}
			};

			match next_event {
				future::Either::Left(Event::NotificationStreamOpened { remote, protocol, .. }) => {
					something_happened = true;
					assert!(!node1_to_node2_open);
					node1_to_node2_open = true;
					assert_eq!(remote, *node2.local_peer_id());
					assert_eq!(protocol, PROTOCOL_NAME);
				}
				future::Either::Right(Event::NotificationStreamOpened { remote, protocol, .. }) => {
					something_happened = true;
					assert!(!node2_to_node1_open);
					node2_to_node1_open = true;
					assert_eq!(remote, *node1.local_peer_id());
					assert_eq!(protocol, PROTOCOL_NAME);
				}
				future::Either::Left(Event::NotificationStreamClosed { remote, protocol, .. }) => {
					assert!(node1_to_node2_open);
					node1_to_node2_open = false;
					assert_eq!(remote, *node2.local_peer_id());
					assert_eq!(protocol, PROTOCOL_NAME);
				}
				future::Either::Right(Event::NotificationStreamClosed { remote, protocol, .. }) => {
					assert!(node2_to_node1_open);
					node2_to_node1_open = false;
					assert_eq!(remote, *node1.local_peer_id());
					assert_eq!(protocol, PROTOCOL_NAME);
				}
				future::Either::Left(Event::NotificationsReceived { remote, .. }) => {
					assert!(node1_to_node2_open);
					assert_eq!(remote, *node2.local_peer_id());
					if rand::random::<u8>() % 5 >= 4 {
						node1.write_notification(
							node2.local_peer_id().clone(),
							PROTOCOL_NAME,
							b"hello world".to_vec()
						);
					}
				}
				future::Either::Right(Event::NotificationsReceived { remote, .. }) => {
					assert!(node2_to_node1_open);
					assert_eq!(remote, *node1.local_peer_id());
					if rand::random::<u8>() % 5 >= 4 {
						node2.write_notification(
							node1.local_peer_id().clone(),
							PROTOCOL_NAME,
							b"hello world".to_vec()
						);
					}
				}

				// Add new events here.
				future::Either::Left(Event::Dht(_)) => {}
				future::Either::Right(Event::Dht(_)) => {}
			};
		}
	});
}

#[test]
fn lots_of_incoming_peers_works() {
	let listen_addr = config::build_multiaddr![Memory(rand::random::<u64>())];
    let pool = build_pool();
	let (main_node, _) = build_test_full_node(config::NetworkConfiguration {
		notifications_protocols: vec![PROTOCOL_NAME],
		listen_addresses: vec![listen_addr.clone()],
		in_peers: u32::max_value(),
		transport: config::TransportConfig::MemoryOnly,
		.. config::NetworkConfiguration::new_local()
	},pool.clone());

	let main_node_peer_id = main_node.local_peer_id().clone();

	// We spawn background tasks and push them in this `Vec`. They will all be waited upon before
	// this test ends.
	let mut background_tasks_to_wait = Vec::new();

	for _ in 0..32 {
		let main_node_peer_id = main_node_peer_id.clone();

		let (_dialing_node, event_stream) = build_test_full_node(config::NetworkConfiguration {
			notifications_protocols: vec![PROTOCOL_NAME],
			listen_addresses: vec![],
			reserved_nodes: vec![config::MultiaddrWithPeerId {
				multiaddr: listen_addr.clone(),
				peer_id: main_node_peer_id.clone(),
			}],
			transport: config::TransportConfig::MemoryOnly,
			.. config::NetworkConfiguration::new_local()
		},pool.clone());

		background_tasks_to_wait.push(async_std::task::spawn(async move {
			// Create a dummy timer that will "never" fire, and that will be overwritten when we
			// actually need the timer. Using an Option would be technically cleaner, but it would
			// make the code below way more complicated.
			let mut timer = futures_timer::Delay::new(Duration::from_secs(3600 * 24 * 7)).fuse();

			let mut event_stream = event_stream.fuse();
			loop {
				futures::select! {
					_ = timer => {
						// Test succeeds when timer fires.
						return;
					}
					ev = event_stream.next() => {
						match ev.unwrap() {
							Event::NotificationStreamOpened { remote, .. } => {
								assert_eq!(remote, main_node_peer_id);
								// Test succeeds after 5 seconds. This timer is here in order to
								// detect a potential problem after opening.
								timer = futures_timer::Delay::new(Duration::from_secs(5)).fuse();
							}
							Event::NotificationStreamClosed { .. } => {
								// Test failed.
								panic!();
							}
							_ => {}
						}
					}
				}
			}
		}));
	}

	futures::executor::block_on(async move {
		future::join_all(background_tasks_to_wait).await
	});
}

#[test]
fn notifications_back_pressure() {
	// Node 1 floods node 2 with notifications. Random sleeps are done on node 2 to simulate the
	// node being busy. We make sure that all notifications are received.

	const TOTAL_NOTIFS: usize = 10_000;

	let (node1, mut events_stream1, node2, mut events_stream2) = build_nodes_one_proto();
	let node2_id = node2.local_peer_id();

	let receiver = async_std::task::spawn(async move {
		let mut received_notifications = 0;

		while received_notifications < TOTAL_NOTIFS {
			match events_stream2.next().await.unwrap() {
				Event::NotificationStreamClosed { .. } => panic!(),
				Event::NotificationsReceived { messages, .. } => {
					for message in messages {
						assert_eq!(message.0, PROTOCOL_NAME);
						assert_eq!(message.1, format!("hello #{}", received_notifications));
						received_notifications += 1;
					}
				}
				_ => {}
			};

			if rand::random::<u8>() < 2 {
				async_std::task::sleep(Duration::from_millis(rand::random::<u64>() % 750)).await;
			}
		}
	});

	async_std::task::block_on(async move {
		// Wait for the `NotificationStreamOpened`.
		loop {
			match events_stream1.next().await.unwrap() {
				Event::NotificationStreamOpened { .. } => break,
				_ => {}
			};
		}

		// Sending!
		for num in 0..TOTAL_NOTIFS {
			let notif = node1.notification_sender(node2_id.clone(), PROTOCOL_NAME).unwrap();
			notif.ready().await.unwrap().send(format!("hello #{}", num)).unwrap();
		}

		receiver.await;
	});
}

#[test]
#[should_panic(expected = "don't match the transport")]
fn ensure_listen_addresses_consistent_with_transport_memory() {
	let listen_addr = config::build_multiaddr![Ip4([127, 0, 0, 1]), Tcp(0_u16)];

	let _ = build_test_full_node(config::NetworkConfiguration {
		listen_addresses: vec![listen_addr.clone()],
		transport: config::TransportConfig::MemoryOnly,
		.. config::NetworkConfiguration::new("test-node", "test-client", Default::default(), None)
	},build_pool());
}

#[test]
#[should_panic(expected = "don't match the transport")]
fn ensure_listen_addresses_consistent_with_transport_not_memory() {
	let listen_addr = config::build_multiaddr![Memory(rand::random::<u64>())];

	let _ = build_test_full_node(config::NetworkConfiguration {
		listen_addresses: vec![listen_addr.clone()],
		.. config::NetworkConfiguration::new("test-node", "test-client", Default::default(), None)
	},build_pool());
}

#[test]
#[should_panic(expected = "don't match the transport")]
fn ensure_boot_node_addresses_consistent_with_transport_memory() {
	let listen_addr = config::build_multiaddr![Memory(rand::random::<u64>())];
	let boot_node = config::MultiaddrWithPeerId {
		multiaddr: config::build_multiaddr![Ip4([127, 0, 0, 1]), Tcp(0_u16)],
		peer_id: PeerId::random(),
	};

	let _ = build_test_full_node(config::NetworkConfiguration {
		listen_addresses: vec![listen_addr.clone()],
		transport: config::TransportConfig::MemoryOnly,
		boot_nodes: vec![boot_node],
		.. config::NetworkConfiguration::new("test-node", "test-client", Default::default(), None)
	},build_pool());
}

#[test]
#[should_panic(expected = "don't match the transport")]
fn ensure_boot_node_addresses_consistent_with_transport_not_memory() {
	let listen_addr = config::build_multiaddr![Ip4([127, 0, 0, 1]), Tcp(0_u16)];
	let boot_node = config::MultiaddrWithPeerId {
		multiaddr: config::build_multiaddr![Memory(rand::random::<u64>())],
		peer_id: PeerId::random(),
	};

	let _ = build_test_full_node(config::NetworkConfiguration {
		listen_addresses: vec![listen_addr.clone()],
		boot_nodes: vec![boot_node],
		.. config::NetworkConfiguration::new("test-node", "test-client", Default::default(), None)
	},build_pool());
}

#[test]
#[should_panic(expected = "don't match the transport")]
fn ensure_reserved_node_addresses_consistent_with_transport_memory() {
	let listen_addr = config::build_multiaddr![Memory(rand::random::<u64>())];
	let reserved_node = config::MultiaddrWithPeerId {
		multiaddr: config::build_multiaddr![Ip4([127, 0, 0, 1]), Tcp(0_u16)],
		peer_id: PeerId::random(),
	};

	let _ = build_test_full_node(config::NetworkConfiguration {
		listen_addresses: vec![listen_addr.clone()],
		transport: config::TransportConfig::MemoryOnly,
		reserved_nodes: vec![reserved_node],
		.. config::NetworkConfiguration::new("test-node", "test-client", Default::default(), None)
	},build_pool());
}

#[test]
#[should_panic(expected = "don't match the transport")]
fn ensure_reserved_node_addresses_consistent_with_transport_not_memory() {
	let listen_addr = config::build_multiaddr![Ip4([127, 0, 0, 1]), Tcp(0_u16)];
	let reserved_node = config::MultiaddrWithPeerId {
		multiaddr: config::build_multiaddr![Memory(rand::random::<u64>())],
		peer_id: PeerId::random(),
	};

	let _ = build_test_full_node(config::NetworkConfiguration {
		listen_addresses: vec![listen_addr.clone()],
		reserved_nodes: vec![reserved_node],
		.. config::NetworkConfiguration::new("test-node", "test-client", Default::default(), None)
	},build_pool());
}

#[test]
#[should_panic(expected = "don't match the transport")]
fn ensure_public_addresses_consistent_with_transport_memory() {
	let listen_addr = config::build_multiaddr![Memory(rand::random::<u64>())];
	let public_address = config::build_multiaddr![Ip4([127, 0, 0, 1]), Tcp(0_u16)];

	let _ = build_test_full_node(config::NetworkConfiguration {
		listen_addresses: vec![listen_addr.clone()],
		transport: config::TransportConfig::MemoryOnly,
		public_addresses: vec![public_address],
		.. config::NetworkConfiguration::new("test-node", "test-client", Default::default(), None)
	},build_pool());
}

#[test]
#[should_panic(expected = "don't match the transport")]
fn ensure_public_addresses_consistent_with_transport_not_memory() {
	let listen_addr = config::build_multiaddr![Ip4([127, 0, 0, 1]), Tcp(0_u16)];
	let public_address = config::build_multiaddr![Memory(rand::random::<u64>())];
    let pool = futures::executor::ThreadPoolBuilder::new().create().expect("");
	let _ = build_test_full_node(config::NetworkConfiguration {
		listen_addresses: vec![listen_addr.clone()],
		public_addresses: vec![public_address],
		.. config::NetworkConfiguration::new("test-node", "test-client", Default::default(), None)
	},pool);
}

#[test]
fn group_tests() {
	env_logger::init();
    let (tx,rx) = unbounded();
	let ret = build_group_nodes(2,90,Some(tx.clone()),Some(tx.clone()));
	let (mut events,nodes) = ret.unwrap();
	let mut boots = vec![];
	for (b,_) in events.iter() {
		boots.push(b.clone());
	}
	async_std::task::spawn(async move {
		loop {
			//group event
			let mut events = events.iter_mut();
			for (boot,evt) in events {
				if let Some(e) = evt.next().await {
					match e {
						Event::NotificationsReceived { remote, messages } =>{
							for (proto,data) in messages{
								let data = data.to_vec();
								log::info!("{:?}------------------>>>>>>{:?}==>{}",remote,proto,hex::encode(&data[..]));
							}
						},
						Event::Dht(e)=>{

						},
						_=> {
                           //log::warn!("=================={:?}",e);
						}
					}
				}

			}

		}
	});

	async_std::task::block_on(async move {
		let mut counter:usize = 0;
		let mut rng = rand::thread_rng();
		loop {
			log::info!("-------------pub-----------------");
			{
				async_std::task::sleep(Duration::from_millis( 1 * 1000)).await;
				let x = counter % nodes.len();
				let (_,n) = &nodes[x];
				let groups = n.local_groups.lock();

				for tg in groups.iter(){

					let mut nums=vec![0_u8;1024];
                    rng.fill_bytes(&mut nums);
					log::warn!("node pub....to:{}----{:?}",tg,n.local_peer_id.clone());
					n.publish_message(tg.clone(),nums);
				}


			}
			{
				async_std::task::sleep(Duration::from_millis( 1 * 1000)).await;
				let y = counter % boots.len();
				let boot = &boots[y];
				let groups = boot.local_groups.lock();
				for tg in groups.iter(){
					let mut nums=vec![0_u8;246];
					rng.fill_bytes(&mut nums);
					log::warn!("boot pub....to:{}----{:?}",tg,boot.local_peer_id.clone());
					boot.publish_message(tg.clone(),nums);
				}
				counter = counter + 1;
			}



		}
	});
	//
	// futures::future::poll_fn(move |cx|loop {
    //     match rx.poll_next(cx) {
	// 		Poll::Ready(None) => panic!(""),
	// 		Poll::Ready(Some(x)) => {
	// 			let hx = hex::encode(x);
	// 			log::info!("hashed:{}",hx);
	// 		}
	// 		Poll::Pending => {
	//
	// 		}
	// 	}
	//
	// });

}
