// Copyright 2019-2020 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate.  If not, see <http://www.gnu.org/licenses/>.

//! Discovery mechanisms of Substrate.

use bytes::{Bytes, BytesMut};
use futures::prelude::*;
use libp2p::core::{ConnectedPoint, Multiaddr, connection::{ConnectionId, ListenerId}};
use libp2p::swarm::{ProtocolsHandler, IntoProtocolsHandler,NetworkBehaviour, NetworkBehaviourAction, PollParameters};
use std::task::{Context, Poll};
use std::sync::{Arc, Mutex,RwLock};
use sp_core::hexdisplay::HexDisplay;
use super::buckets::BucketTable;
use std::collections::{HashMap,HashSet};
use lru::LruCache;
use codec::{Encode, Decode};
use std::error::Error;
use std::result::Result;
use sp_runtime::{traits::{Block as BlockT, NumberFor}, Justification};
use super::generic_proto::{GenericProto,GenericProtoOut};
use crate::protocol::message::generic::Message as GenericMessage;
use std::time::{SystemTime,Duration,UNIX_EPOCH,SystemTimeError};
use crate::{config::ProtocolId};
use libp2p::{NetworkBehaviour, PeerId};
use futures::channel::mpsc;
use libp2p::identity::{PublicKey, ed25519};
use crate::protocol::transpp::routetab::{RouteItem, RouteTable, RoutePathItem};
use libp2p::multiaddr::Protocol;
use sp_core::Hasher;

const MAX_ROUTE_PENDING_ITEM:usize = 1024;
const MAX_DATA_CACHE_COUNT:usize  = 128;
const DEFAULT_ALPHA :usize = 3;
const DEFAULT_MAX_TTL: usize = 10;
const MAX_DIRECT_PACKET: usize = 256;
//每次路由发现请求在系统中的保留时间，3分钟
const MAX_ROUTE_REQ_TIME:Duration = Duration::from_secs(180);
//最长数据获取时间，3秒
const MAX_DATA_RETRIEVE:Duration = Duration::from_secs(3);

pub(crate) const MAINTENANCE_INTERVAL: Duration = Duration::from_millis(1000);
use log::{trace,info,debug,error};
use std::borrow::Cow;
use libp2p::futures::FutureExt;

#[derive(Debug, Eq, PartialEq,Clone,Encode,Decode)]
pub struct FindRouteReq{
    pub src:Vec<u8>,
    pub createTime:u64, //此路由的请求时间 
    pub dest:Vec<u8>,      //目标地址，此处为H
    pub alpha:u8,     //扩散值 
    pub pathes:Option<Vec<RoutePathItem>>,   //源路径为空
    pub ttl:u8,      //初始TTL值，
	pub sign:Vec<u8>,      // B的签名
}

#[derive(Debug,Eq, PartialEq,Clone,Encode,Decode)]
pub struct Tag(Vec<u8>);

#[derive(Debug, Eq, PartialEq,Clone,Encode,Decode)]
pub struct RelayDataReq{
    pub    routeInfo:FindRouteReq,
	pub    hash:Vec<u8>,
	pub    tags:Vec<Tag>,
    pub    data:Option<Vec<u8>>,
    pub    sign_packet:Vec<u8>, 
}
///路由发现协议的回应
#[derive(Debug, Eq, PartialEq,Clone,Encode,Decode)]
pub struct FindRouteResp{
	pub    dest:Vec<u8>,
    pub    route:Vec<RouteItem>,
    pub    sign:Vec<u8>,
    
}
#[derive(Debug,Encode,Decode)]
pub enum TransppEventOut {
	FindRouteEvt(FindRouteReq),
	FindRouteRespEvt(FindRouteResp),
	RelayDataEvt(RelayDataReq),
	PullDataEvt(PullDataReq),
	PullDataRespEvt(PullDataResp),
	None,
}
impl TransppEventOut {
	pub fn id(&self) -> &'static str{
		match self {
			PullDataEvt => "pull_data",
			PullDataRespEvt => "push_data",
			FindRouteEvt => "find_route_req",
			FindRouteRespEvt => "find_route_resp",
			RelayDataEvt => "relay_data_req",
		}
	}
}
pub enum  CustomEventOut<B:Hasher> {
	//transpp 发送出来的数据
	TransppData(PeerId,Vec<u8>),
	//peer的连接和断开
	PeerConnected(PeerId),
	PeerDisconnected(PeerId),
	//路由消息
	RouteUpdated(PeerId),
	//数据转发请求？
	DataRelay(FindRouteReq,B::Out,Vec<u8>),
	//数据请求，数据请求回应
	PullData(PeerId,B::Out),
	PullDataResp(PeerId,B::Out),
	None,
}
#[derive(Debug, Eq, PartialEq,Clone,Encode,Decode)]
pub struct PullDataReq {
	hash: Vec<u8>,
}
#[derive(Debug, Eq, PartialEq,Clone,Encode,Decode)]
pub struct PullDataResp {
	hash: Vec<u8>,
	data: Vec<u8>,
}
//由于整个系统是通过poll机制实现的，因此在一个线程里工作，不需要做同步等额外的工作
/// Implementation of `Transp2pBehaviour` that implements peer-to-peer data transfer with route
/// General behaviour of the network. Combines all protocols together.

pub struct Transp2pBehaviour<B:Hasher> {
	//自身地址
	local_id:PeerId,
	//计算饱和度的
	buckets:BucketTable,
	//路由表
	table:RouteTable,
	//收到的路由请求
	route_req_recv:LruCache<PeerId,HashSet<PeerId>>,
	//已经发送的路由请求，什么时候请求过的
	route_req_sent:LruCache<PeerId,u64>,
	//暂存的数据,哈希到数据
	data_cache:LruCache<B::Out,Arc<RwLock<Vec<u8>>>>,
	//已经发送请求的，但是还没有回应的哈希，值中的时间是第一次请求的时间
	pending_data_retrieve:LruCache<B::Out,(u64,Vec<PeerId>)>,
	//一组已经发达过的数据缓存(目标地址，哈希值)
	data_relay_sent:LruCache<PeerId,HashSet<B::Out>>,
	//一组等待数据的哈希对应的请求，需要等到数据收到后才能够转发
	pending_hash_for_data:LruCache<B::Out,Vec<RelayDataReq>>,
	//一组等待路由的数据，数据已经取到了存放在data_cache中，注意当路由得到的时候，数据有可能会被扔了，因此如果检查数据发现没有了，就简单丢弃该数据了
	pending_data_for_route:LruCache<B::Out,Vec<RelayDataReq>>,
	//当前已经连接的节点
	connected_peers:HashSet<PeerId>,
	//行为处理
	behaviour:GenericProto,
	//监控的tags
	tags:HashSet<Tag>,
	protocol: ProtocolId,
	/// Events to produce from `poll()`.
	//	events: SmallVec<[NetworkBehaviourAction<NotifsHandlerIn, GenericProtoOut>; 4]>,
	live_message_sinks: Vec<mpsc::UnboundedSender<CustomEventOut<B>>>,
	//定时检查，看看route_req_sent中是不是有超过3分钟的可以扔了
	periodic_maintenance_interval: futures_timer::Delay,
}

impl<B:Hasher>  Transp2pBehaviour<B>{
	/// Builds a new `Transp2pBehaviour`.
	///
	/// `user_defined` is a list of known address for nodes that never expire.
	pub  fn new(
		local_public_key: PublicKey,
		protocol: impl Into<ProtocolId> + Clone,
		versions: &[u8],
		peerset: sc_peerset::Peerset,
		notif_protocols: impl Iterator<Item = (Cow<'static, str>, Vec<u8>)>,
	) -> Self {

		let local_id = local_public_key.clone().into_peer_id();
		Transp2pBehaviour {
			local_id:PeerId::from(local_public_key),
			buckets:BucketTable::new(local_id.clone()),
			table:RouteTable::new(),
			route_req_recv:LruCache::new(MAX_ROUTE_PENDING_ITEM),
			route_req_sent:LruCache::new(MAX_ROUTE_PENDING_ITEM),
			data_cache:LruCache::new(MAX_DATA_CACHE_COUNT),
			pending_data_retrieve:LruCache::new(MAX_DATA_CACHE_COUNT),
			connected_peers:HashSet::new(),
			behaviour:GenericProto::new(local_id,protocol.clone(),versions,vec![],peerset,notif_protocols),
			pending_hash_for_data:LruCache::new(MAX_ROUTE_PENDING_ITEM),
			data_relay_sent:LruCache::new(2*MAX_ROUTE_PENDING_ITEM),
			pending_data_for_route:LruCache::new(2*MAX_ROUTE_PENDING_ITEM),
			live_message_sinks:vec![],
			tags:HashSet::new(),
			periodic_maintenance_interval: futures_timer::Delay::new(MAINTENANCE_INTERVAL),
			protocol:protocol.into(),
		}
	}

		/// Get data of valid, incoming messages for a topic (but might have expired meanwhile)
	pub fn get_event_stream(&mut self)
		-> mpsc::UnboundedReceiver<CustomEventOut<B>>
	{
		let (tx, rx) = mpsc::unbounded();


		self.live_message_sinks.push(tx);

		rx
	}

	pub fn get_closet_peers(&mut self,peer_id:PeerId)->Vec<PeerId>  {
		self.buckets.get_closet_peers(&peer_id,0)
	}
}
//数据收发部分
impl<B:Hasher>  Transp2pBehaviour<B>{

	//请求发送数据
	pub fn send_peers(&mut self,targets:Vec<PeerId>,alpha:usize,ttl:usize,tags:Vec<Tag>,data:Vec<u8>)->Result<(),SystemTimeError>{
		//创建哈希
		let hash = Hasher::hash(&data[..]);
		let mut alpha = alpha;
		if (alpha == 0) || (alpha > DEFAULT_ALPHA) {
			alpha = DEFAULT_ALPHA;
		}
		let mut ttl = ttl;
		if (ttl == 0) || (ttl > DEFAULT_MAX_TTL) {
			ttl = DEFAULT_MAX_TTL;
		}
		//创建请求
		let mut dataRelayReq = RelayDataReq::<B>{
			routeInfo:FindRouteReq{
				createTime:SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
				src:self.local_id.into_bytes(),
				dest:targets[0].into_bytes(),
				alpha:alpha as u8,
				ttl :ttl as u8,
				pathes:None,
				sign:vec![],
			},
			hash:hash,
			tags,
			data:Some(data.clone()),
			sign_packet:vec![],
		};
		if data.len() > MAX_DIRECT_PACKET {
			self.data_cache.put(hash, Arc::new(RwLock::new(data.clone())));
		}else{
			dataRelayReq.data = Some(data.clone());
		}

		let mut next_hops = vec![];
		targets.iter().for_each(|target|{
			for (next,_) in self.table.get(target.into_bytes()).unwrap_or_default().iter(){
				next_hops.push(next);
			}
		});
		next_hops.sort();
		next_hops.dedup();
		let data_packet = <RelayDataReq<B> as Encode>::encode(&dataRelayReq);
		next_hops.iter().for_each(|next_hop|{
            self.behaviour.write_notification(
				&PeerId::from_bytes(next_hop.to_vec()).unwrap(),
				self.protocol.as_ref().into(),
				 data_packet,
			);
		});
		Ok(())
	}


	
	//收到请求数据转发
	fn onDataRelay(&mut self,from:&PeerId,relayReq:RelayDataReq){
		//根据来源先更新路由
		let RelayDataReq {
			routeInfo, hash, tags, data, sign_packet
		} = relayReq;
		self.onFindRouteReq(from, routeInfo.clone());
		//如果该数据已经发送过了，就不再发送
		match self.data_relay_sent.get(&PeerId::from_bytes(routeInfo.dest).unwrap()){
			Some(sets) =>{
				if sets.contains(&hash) {
					//ignore if this target has been relayed
					return;
				}
			}
			None =>{}
		}
		let mut data_ok = false;
		//如果直接有数据，就直接转发
		match &data {
			Some(_data) =>{
				data_ok = true;
			}
			None=>{
				//读取数据
				match self.data_cache.get(&hash) {
					Some(_) => { //数据存在，可以转发
						data_ok = true;
					},
					None =>{ //数据不存在，请求获取数据
					
						match self.pending_data_retrieve.get_mut(&hash) {
							Some((_time,peersVec)) => {
									//如果数据已经在读取中了，就只是把潜在的节点放入
								peersVec.push(from.clone());
							},
							None =>{
								self.pending_data_retrieve.put(hash.clone(),(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),vec![from.clone()]));
							}

						}
						match self.pending_hash_for_data.get_mut(&hash){
							Some(targetItems) =>{
								targetItems.push(relayReq);
							},
							None =>{
								self.pending_hash_for_data.put(hash,vec![relayReq]);
							}
						}
					}
				}
			}
		}
		//数据存在的，可以中继和上报了
		if data_ok {
			let mut should_report = false;
			if relayReq.routeInfo.dest == self.local_id.clone().into_bytes() {
				should_report = true;
			}else {
				let tags = &relayReq.tags;
				for (_,tag) in tags.iter().enumerate(){
					if self.tags.contains(tag) {
						should_report = true;
						break;
					}
				}
				self.doDataRelay(from,relayReq);
			}

			if should_report {
				//TODO 发送给stream
			}
			
		}	
	}

	fn doDataRelay(&mut self,from:&PeerId,relayReq:RelayDataReq){
		//记录发送的信息
		let RelayDataReq {
			routeInfo, hash, tags, data, sign_packet
		}=relayReq;

		let FindRouteReq {
			src, createTime, dest, alpha, pathes, ttl, sign
		}=routeInfo;
		let key = PeerId::from_bytes(dest.clone()).unwrap();
		match self.data_relay_sent.get_mut(&key) {
			Some(sets) =>{
				sets.insert(hash.clone());
			},
			None =>{
				let mut sets = HashSet::with_capacity(MAX_DATA_CACHE_COUNT);
				sets.insert(hash.clone());
				self.data_relay_sent.put(key.clone(),sets);
			}
		}
		//如果TTL超了，就直接丢弃该数据
		//否转发到下一节点
		match self.table.get(key.clone().into_bytes()) {
			Some(next_items) =>{
				let send_data:Vec<u8>;
				if from != &self.local_id {
					let newPathes = RoutePathItem {
						next:from.into_bytes(),
						pathes,
						min_ttl: 0,
						sign: None
					};
					let newRouteInfo = FindRouteReq{
						src,
						dest,
						createTime,
						alpha,
						ttl,
						pathes:Some(vec![newPathes]),
						sign: vec![]
					};
					let newDataRelay = RelayDataReq{
						routeInfo:newRouteInfo,
						hash,
						tags: vec![],
						data,
						sign_packet,
					};
					send_data = <RelayDataReq<B> as Encode>::encode(&newDataRelay);
				}else {
					//说明这是第一个开始的请求
					send_data = <RelayDataReq<B> as Encode>::encode(&relayReq);
				}
				//选前alpha个
				let mut send_cnt = 0;
				next_items.iter().for_each(|(routeItem,_)|{
					if send_cnt < alpha {
						self.behaviour.write_notification(
							&PeerId::from_bytes(routeItem.to_vec()).unwrap(),
							 self.protocol.as_ref().into(),
							send_data
						);
						send_cnt += 1;
					}
					
				})
			},
			None =>{
				//路由里没有，生成路由发现请求，等到路由回应的时候再发送
				self.findRoute(&key, relayReq.routeInfo.alpha, relayReq.routeInfo.ttl);
			}
		}
	}

	fn doPullData(&mut self ,target:&PeerId,hash:Vec<u8>) {
		let req = PullDataReq{
			hash,
		};
		self.behaviour.write_notification(target,self.protocol.as_ref().into(),req.encode());
	}
	//收到请求数据消息,TODO 优化，直接传一个rwlock对象过去，到最底层发关的时候再处理
	fn onPullData(&mut self,from:&PeerId,hash:B::Out) {
		match self.data_cache.get(hash) {
			Some(data) =>{
				let resp = PullDataResp {
					hash:hash,
					data:*data.read().unwrap(),
				};
				// self.behaviour.send_packet(from,resp.encode());
				self.behaviour.write_notification(from,self.protocol.as_ref().into(),resp.encode());
			}
			None =>{
				//我这里没有，直接丢弃了，但其实在正常状态下是不会出现的。
			}
		}
	}
	//请求数据回应了
	fn onPullDataResp(&mut self,from:&PeerId,dataResp:PullDataResp){
		let  PullDataResp {
			hash, data
		} = dataResp;
		//验证并且填充
		if B::hash(&data) != hash {
			//错误信息
			return;
		}
		self.data_cache.put(hash.clone(),Arc::new(RwLock::new(data)));

		//确认是否可以上报或是转发了
		match self.pending_hash_for_data.get(&hash){
			Some(targets) =>{
				targets.to_vec().into_iter().for_each(|relay_req|{
                    let  RelayDataReq {
						routeInfo, hash, tags, data, sign_packet
					} = relay_req.clone();
					let FindRouteReq {
						src, createTime, dest, alpha, pathes, ttl, sign
					} = routeInfo;
					if dest == self.local_id.into_bytes() {
						//发组自己的，向上汇报
					}else {
						//否则转发
						self.doDataRelay(&PeerId::from_bytes(src.clone()).unwrap(), relay_req);
					}
					
				})
			}
			_ => {}
		}

	}

}

//以下是路由管理部分
impl<B:Hasher>  Transp2pBehaviour<B>{

	//请求发现路由
	pub fn findRoute(&mut self,dest: &PeerId,alpha: u8,ttl :u8)->Result<(),SystemTimeError> {
		//创建请求
		let findReq = FindRouteReq{
			src:self.local_id.into_bytes(),
			createTime:SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
			dest:dest.clone().into_bytes(),
			pathes:None,
			alpha,
			ttl,
			sign:vec![],

		} ;
	
		//查找需要发送的目标
		let peers = self.buckets.get_closet_peers(dest,alpha as usize);
		peers.iter().for_each(|peer|{
			//发送请求
			// self.behaviour.send_packet(peer,findReq.encode());
			self.behaviour.write_notification(peer,self.protocol.as_ref().into(),findReq.encode());
			//记录发送的请求
			self.route_req_sent.put(*peer.clone(), findReq.createTime);
		});
		Ok(())

	 }
	 //某个路由已经更新
	 fn onRouteUpdated(&mut self,target:&PeerId) {
		match self.table.get(target.clone().into_bytes()) {
			Some(values) =>{
				//检查是否有需要发给该节点的数据，有的话，发出去
				match self.pending_data_for_route.get(target) {
					Some(data_items)=>{
						//data_items是一个待发送给各个节点的数据组
						data_items.to_vec().into_iter().for_each(|data_item|{
							match self.data_cache.get(&data_item.hash) {
								Some(_) =>{
									self.doDataRelay(&self.local_id, data_item);
								},
								None =>{
									//没数据，直接丢弃吧
									self.data_cache.pop(data_item.hash);
								}
							}
						});
					},
					None=>{}
				};
			},
			None=>{}
		} 
		
	 }
	//收到路由发现请求
	fn onFindRouteReq(&mut self,from: &PeerId,findReq: FindRouteReq){
		//avoid rewound request
		if self.route_req_sent.contains(from){
			return;
		}
		let FindRouteReq {
			src,
			createTime,
			dest,
			alpha,
			pathes,
			ttl,
			sign,
		} = findReq.clone();
		//验证签名,暂略过
		//更新路由表（如果需要）
		let pathItem = RoutePathItem {
			next:from.into_bytes(),
			pathes:pathes.clone(),
			min_ttl: 0,
			sign: None
		};


		//如果需要更新等待中的route_req
		// self.table.add(src.clone(), createTime, from, pathes, sign);

		self.onRouteUpdated(&PeerId::from_bytes(src.clone()).unwrap());
		//根据req.src来更新路由表，并且响应正在等待此路由的节点
		match self.table.get(src.clone()) {
			Some(values) =>{
				// let route_items = values.iter().filter(|(peerId,_time)|{
				// 	self.connected_peers.contains(peerId)
				// }).map(|(peer_id,time)|{
                //
				// })
				// 	.collect();
				// if route_items.len() > 0 {
				// 	//直接执行response
				// 	self.doRouteResponse(PeerId::from_bytes(src).unwrap(),route_items);
				// }
			},
			None =>{},
		}
		let peer_dest = PeerId::from_bytes(dest.clone()).unwrap();
		let mut need_forward = true;

		if self.local_id == peer_dest {
			//要找的是自己,直接回应
			need_forward = false;
		}
		match self.route_req_sent.get(&peer_dest) {
			Some(routes) =>{
				//有记录，最近一段时间已经发送过，不再发送
				need_forward = false;
			}
			None =>{
			}
		}
		match self.table.get(dest) {
			Some(values) =>{
				let route_items = values.iter().filter(|(peerId,item)|{
					self.connected_peers.contains((&PeerId::from_bytes(peerId.to_vec()).unwrap()).as_ref())
				})
				.map(|(peer_id,item)|{
				   item.values().next().unwrap().clone()
				})
				.collect::<Vec<_>>();
				if route_items.len() > 0 {
					//直接执行response
					self.doRouteResponse(&peer_dest,route_items);
					need_forward = false;
				}
			},
			None =>{}
		}

		if need_forward {

		}
	}
	//中继发送路由发现请求
	fn doFindReqRelay(&mut self,from:PeerId,findReq:&FindRouteReq){
		let FindRouteReq {
			src, createTime, dest, alpha, pathes, ttl, sign
		} = findReq.clone();
		//寻找需要发送的目标
		let peer_dest = PeerId::from_bytes(dest.clone()).unwrap();
		let targets =  self.buckets.get_closet_peers(&peer_dest,5);
		//记录本次转发时间
		self.route_req_sent.put(peer_dest.clone(),SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
		let newPathes = RoutePathItem {
			next:from.into_bytes(),
			pathes,
			min_ttl: 0,
			sign: None
		};
		//TODO sign should be added
		let newReq = FindRouteReq {
			src,
			dest,
			createTime:findReq.createTime,
			pathes:Some(vec![newPathes]),
			alpha,
			ttl,
			sign:vec![]
		}.encode();
		targets.iter().for_each(|peer_id|{
			//转发请求
			self.behaviour.write_notification(peer_id,self.protocol.into(),newReq);

		});

	}
	//收到路由回应消息
	fn onFindRouteResp(&mut self, from: &PeerId, find_resp: FindRouteResp){
		//avoid rewound request
		// if self.pending_req_sent.contains(from){
		// 	return;
		// }

        let FindRouteResp {
			dest, route, sign
		} = find_resp;
		let mut pathes = vec![];
		for r in route {
           pathes.extend(r.pathes.into_iter());
		}
		//验证签名,暂略过
		//更新路由表（如果需要）
		let pathItem = RoutePathItem {
			next:from.into_bytes(),
			pathes: Some(pathes),
			min_ttl: 0,
			sign: None
		};
		//如果需要更新等待中的route_req
		// self.table.add(src, createTime, from, sign);

		//根据req.src来更新路由表，并且响应正在等待此路由的节点
		match self.table.get(dest.clone()) {
			Some(values) =>{
			let route_items = values.iter().filter(|(peer_id,item)|{
				self.connected_peers.contains((&PeerId::from_bytes(peer_id.to_vec()).unwrap()).as_ref())
			})
				.map(|(_,item)|{
					item.values().into_iter().next().unwrap().clone()
				})
				.collect::<Vec<_>>();
			if route_items.len() > 0 {
				//直接执行response
				self.doRouteResponse(&PeerId::from_bytes(dest.to_vec()).unwrap(), route_items);
			}
			},
			None =>{}
		}

	}
	 //响应路由发现请求
	fn doRouteResponse(&mut self,target:&PeerId,route_items:Vec<RouteItem>) {
		match self.route_req_recv.get(&target) {
			Some(value_set) =>{
				let findResp = FindRouteResp {
					dest:target.into_bytes(),
					route: route_items,
					sign: vec![]
				};
				value_set.iter().for_each(|peerId|{
					self.behaviour.write_notification(
						peerId,
						self.protocol.as_ref().into(),
						findResp.encode()
					);
					// self.behaviour.send_packet(peerId,<TransppEventOut<B> as Encode>::encode(TransppEventOut(findResp)));
				})
			},
			None=>{},
		}
	}
	

	fn on_tick(&mut self){
		let mut to_delete :Vec<PeerId> = vec![];
		let current_time = SystemTime::now();
		//删除所有find_route_req发出的超过3分钟的
		self.route_req_sent.iter().for_each(|(id,time)|{
			if current_time.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() - time > MAX_ROUTE_REQ_TIME.as_secs() {
				to_delete.push(id.clone());
			}
		});
		to_delete.iter().for_each(|id|{
			self.route_req_sent.pop(id);
		});
		let mut to_delete_hash:Vec<B::Out> = vec![];
		//删除读取数据超时的，并且在剩余的节点中重新找一个
		self.pending_data_retrieve.iter_mut().for_each(|(hash,(time,peers))|{
			if current_time.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() - time > MAX_DATA_RETRIEVE.as_secs() {
				peers.pop();
				if peers.len() > 0{
					//向第一个节点发送一次数据请求
					match self.data_cache.get(hash) {
						Some(_)=>{},
						None => {
							self.doPullData(peers.first().unwrap(),hash.into());
							*time=SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
						},
					}
				}else{
					to_delete_hash.push(hash);
				}
			}
		});
		to_delete_hash.iter().for_each(|hash|{
			self.pending_data_retrieve.pop(hash);
		});


	}
	pub fn on_custom_message(
		&mut self,
		who: PeerId,
		data: BytesMut,
	)  {

		let message = match <TransppEventOut as Decode>::decode(&mut &data[..]) {
			Ok(message) => message,
			Err(err) => {
				debug!(target: "sync", "Couldn't decode packet sent by {}: {:?}: {}", who, data, err.what());
			//	self.peerset_handle.report_peer(who.clone(), rep::BAD_MESSAGE);
			// 	return TransppEventOut::None;
			}
		};
		//
		// let mut stats = self.context_data.stats.entry(message.id()).or_default();
		// stats.bytes_in += data.len() as u64;
		// stats.count_in += 1;

		match message {
		TransppEventOut::FindRouteEvt(s) => self.onFindRouteReq(&who, s),
		TransppEventOut::FindRouteRespEvt(s) => self.onFindRouteResp(&who, s),
		TransppEventOut::RelayDataEvt(s) => self.onDataRelay(&who, s),
		TransppEventOut::PullDataEvt(s) => self.onPullData(&who, s),
		TransppEventOut::PullDataRespEvt(s) => self.onPullDataResp(&who, s),
			_ =>{},
		}
	}
}

impl <B:Hasher + 'static> NetworkBehaviour for Transp2pBehaviour<B> {
	type ProtocolsHandler = <GenericProto as NetworkBehaviour>::ProtocolsHandler;
	type OutEvent = CustomEventOut<B>;

	fn new_handler(&mut self) -> Self::ProtocolsHandler {
		NetworkBehaviour::new_handler(&mut self.behaviour)
	}

	fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
		self.behaviour.addresses_of_peer(peer_id)
	}

	fn inject_connected(&mut self, peer_id: &PeerId) {
		self.buckets.PeerConnected(&peer_id);
		self.behaviour.inject_connected(&peer_id)
	}

	fn inject_disconnected(&mut self, peer_id: &PeerId) {
		self.buckets.PeerDisconnected(&peer_id);
		self.behaviour.inject_disconnected( &peer_id)
	}

	fn inject_event(
		&mut self,
		peer_id: PeerId,
		connection: ConnectionId,
		event: <<Self::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::OutEvent,

	) {
		unimplemented!()
	}

	fn inject_addr_reach_failure(
		&mut self,
		peer_id: Option<&PeerId>,
		addr: &Multiaddr,
		error: &dyn std::error::Error
	) {
		self.behaviour.inject_addr_reach_failure( peer_id, addr, error)
	}

	fn inject_dial_failure(&mut self, peer_id: &PeerId) {
		self.behaviour.inject_dial_failure(peer_id)
	}

	fn inject_new_listen_addr(&mut self, addr: &Multiaddr) {
		self.behaviour.inject_new_listen_addr( addr)
	}

	fn inject_expired_listen_addr(&mut self, addr: &Multiaddr) {
		info!(target: "sub-libp2p", "No longer listening on {}", addr);
		self.behaviour.inject_expired_listen_addr( addr)
	}

	fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
		let new_addr = addr.clone()
			.with(Protocol::P2p(self.local_id.clone().into()));
		info!(target: "sub-libp2p", "Discovered new external address for our node: {}", new_addr);
		self.behaviour.inject_new_external_addr( addr)
	}

	fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static)) {
		error!(target: "sub-libp2p", "Error on libp2p listener {:?}: {}", id, err);
		self.behaviour.inject_listener_error( id, err);
	}


	fn inject_listener_closed(&mut self, id: ListenerId, reason: Result<(), &std::io::Error>) {
		error!(target: "sub-libp2p", "Libp2p listener {:?} closed", id);
		self.behaviour.inject_listener_closed( id,reason);
	}


	/**
	 * 处理behaviour的GenerateEvent，再处理成事件
	 * 
	 * 如果是SendEvent直接返回
	 * 
	 * 定时器消息处理
	 * 
	 * 
	 */
	fn poll(
		&mut self,
		cx: &mut std::task::Context,
		params: &mut impl PollParameters,
	) -> Poll<
		NetworkBehaviourAction<
			<<Self::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::InEvent,
			Self::OutEvent
		>
	> {
		while let Poll::Ready(()) = self.periodic_maintenance_interval.poll_unpin(cx) {
			self.periodic_maintenance_interval.reset(MAINTENANCE_INTERVAL);
			self.on_tick();
		}
		let event = match self.behaviour.poll(cx, params) {
			Poll::Pending => return Poll::Pending,
			Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev)) => ev,
			Poll::Ready(NetworkBehaviourAction::DialAddress { address }) =>
				return Poll::Ready(NetworkBehaviourAction::DialAddress { address }),
			Poll::Ready(NetworkBehaviourAction::DialPeer { peer_id, condition }) =>
				return Poll::Ready(NetworkBehaviourAction::DialPeer { peer_id, condition}),
			Poll::Ready(NetworkBehaviourAction::SendEvent { peer_id, event }) =>
				return Poll::Ready(NetworkBehaviourAction::SendEvent { peer_id, event }),
			Poll::Ready(NetworkBehaviourAction::ReportObservedAddr { address, score }) =>
				return Poll::Ready(NetworkBehaviourAction::ReportObservedAddr { address,score }),
		};

		//只需要对GenerateEvent作再一次的处理
		let outcome = match event {
			GenericProtoOut::CustomProtocolOpen { peer_id, .. } => {
				// self.on_peer_connected(peer_id.clone());
				CustomEventOut::None
			}
			GenericProtoOut::CustomProtocolClosed { peer_id, .. } => {
				// self.on_peer_disconnected(peer_id.clone());
				CustomEventOut::None
			},
			GenericProtoOut::LegacyMessage { peer_id, message } =>{
					self.on_custom_message(peer_id, message);
					CustomEventOut::None
				},
			GenericProtoOut::Notification { peer_id, protocol_name, message } => {
				self.on_custom_message(peer_id, message);
				CustomEventOut::None
			}
			_ => CustomEventOut::None
		};

		if CustomEventOut::None == outcome {
			return Poll::Pending
		} else {
			return Poll::Ready(NetworkBehaviourAction::GenerateEvent(outcome))
		}
	}

}
/*
#[cfg(test)]
mod tests {
	use futures::prelude::*;
	use libp2p::identity::Keypair;
	use libp2p::Multiaddr;
	use libp2p::core::upgrade;
	use libp2p::core::transport::{Transport, MemoryTransport};
	use libp2p::core::upgrade::{InboundUpgradeExt, OutboundUpgradeExt};
	use libp2p::swarm::Swarm;
	use std::{collections::HashSet, task::Poll};
	use crate::protocol::transpp::transpp::Transp2pBehaviour;


	#[test]
	fn discovery_working() {
		let mut user_defined = Vec::new();

		// Build swarms whose behaviour is `DiscoveryBehaviour`.
		let mut swarms = (0..25).map(|_| {
			let keypair = Keypair::generate_ed25519();
			let keypair2 = keypair.clone();

			let transport = MemoryTransport
				.and_then(move |out, endpoint| {
					let secio = libp2p::secio::SecioConfig::new(keypair2);
					libp2p::core::upgrade::apply(
						out,
						secio,
						endpoint,
						upgrade::Version::V1
					)
				})
				.and_then(move |(peer_id, stream), endpoint| {
					let peer_id2 = peer_id.clone();
					let upgrade = libp2p::yamux::Config::default()
						.map_inbound(move |muxer| (peer_id, muxer))
						.map_outbound(move |muxer| (peer_id2, muxer));
					upgrade::apply(stream, upgrade, endpoint, upgrade::Version::V1)
				});

			let behaviour = futures::executor::block_on({
				let user_defined = user_defined.clone();
				let keypair_public = keypair.public();
				async move {
					Transp2pBehaviour::new(keypair_public,"/transpp/1",vec![],)
					//DiscoveryBehaviour::new(keypair_public, user_defined, false, true, 50).await
				}
			});
			let mut swarm = Swarm::new(transport, behaviour, keypair.public().into_peer_id());
			let listen_addr: Multiaddr = format!("/memory/{}", rand::random::<u64>()).parse().unwrap();

			if user_defined.is_empty() {
				user_defined.push((keypair.public().into_peer_id(), listen_addr.clone()));
			}

			Swarm::listen_on(&mut swarm, listen_addr.clone()).unwrap();
			(swarm, listen_addr)
		}).collect::<Vec<_>>();

		// Build a `Vec<HashSet<PeerId>>` with the list of nodes remaining to be discovered.
		let mut to_discover = (0..swarms.len()).map(|n| {
			(0..swarms.len()).filter(|p| *p != n)
				.map(|p| Swarm::local_peer_id(&swarms[p].0).clone())
				.collect::<HashSet<_>>()
		}).collect::<Vec<_>>();

		let fut = futures::future::poll_fn(move |cx| {
			'polling: loop {
				for swarm_n in 0..swarms.len() {
					match swarms[swarm_n].0.poll_next_unpin(cx) {
						Poll::Ready(Some(e)) => {
							match e {
								DiscoveryOut::UnroutablePeer(other) => {
									// Call `add_self_reported_address` to simulate identify happening.
									let addr = swarms.iter().find_map(|(s, a)|
										if s.local_peer_id == other {
											Some(a.clone())
										} else {
											None
										})
										.unwrap();
									swarms[swarm_n].0.add_self_reported_address(&other, addr);
								},
								DiscoveryOut::Discovered(other) => {
									to_discover[swarm_n].remove(&other);
								}
								_ => {}
							}
							continue 'polling
						}
						_ => {}
					}
				}
				break
			}

			if to_discover.iter().all(|l| l.is_empty()) {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		});

		futures::executor::block_on(fut);
	}
}
*/