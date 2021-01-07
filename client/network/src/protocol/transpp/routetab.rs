
use crate::{PeerId};
use std::time::{Duration, SystemTime,SystemTimeError,UNIX_EPOCH};
use lru::LruCache;
use std::collections::{HashMap, HashSet, hash_map::Entry};
use codec::{Encode, Decode};
use std::vec::Vec;
use std::error::Error;
use std::{fmt,str,io::Write};
const MaxRouteItemCount:usize = 10240;
const RouteExpireTime:u64  =  600;
const MaxItemEachNext:usize = 3;
const InvalidIndex:u32 = 65536;
const InvalidTTL:u8   =255;
//因为SystemTime/PeerId不支持Encode/Decode，所以只能用这个方法了
#[derive(Debug, Eq, PartialEq,Clone,Encode,Decode)]
pub struct RoutePathItem{
    pub next:Vec<u8>,
    pub pathes:Option<Vec<RoutePathItem>>,
    pub min_ttl:u8,
    pub sign:Option<Vec<u8>>,
}

impl RoutePathItem {
    pub fn getMinTTL(& self)->u8{
         self.min_ttl
    }
    pub fn update_min_ttl(& mut self) {
        let mut min_ttl:u8 = InvalidTTL;
        match &mut self.pathes{
           Some(pathes) =>{
            pathes.iter_mut().for_each(|sub_item|{
                sub_item.update_min_ttl()
            });
            pathes.iter().for_each( |sub_item|{
                if min_ttl > sub_item.min_ttl {
                    min_ttl = sub_item.min_ttl;
                }  
            });
           },
           None =>{
            min_ttl = 1
           }
        }

        self.min_ttl = min_ttl;

    }

    /** 
     * 测试本地的pathes里是否包含RoutePathItem这一项
     * 这其实是本地分支里是否包含目标分支,
     * 包含的意思是：目标树里的每一条分支，在本地都能找到
     * **/
    fn contains(&self,pathItem:&RoutePathItem) -> bool {
        if self.next != pathItem.next {
            false
        }else{
            let mut  contained = false;
            match &pathItem.pathes {
                Some(incoming_pathes) =>{
                    //如果新来的每一个分支，在自身的里面都能够找到，那么我们就认为这个是包含的
                    let  results:Vec<bool> =  incoming_pathes.iter().map(|one_path|{
                        let mut contained = false;
                        match &self.pathes {
                            Some(my_path) =>{
                                for (_i,a_path) in my_path.iter().enumerate(){
                                    if a_path.contains(one_path) {
                                        contained = true;
                                        break;
                                    }
                                }
                            },
                            None =>{},
                        }
                        contained
                    }).collect();
                    for (i,found) in results.iter().enumerate(){
                        if !found { return false; }
                    }
                    return true;
                },
                None =>{
                    match &self.pathes {
                        None =>{ return true;},
                        _ =>{return false;}
                    }
                }
            }    
        }
    }
    fn print(&self) ->String{

        let mut string = String::from("{\"next\":\"");
        let peer_str = PeerId::from_bytes(self.next.clone()).unwrap().to_base58();

        string.push_str(&peer_str);
        string.push_str("\",");
        match &self.pathes {
            Some(pathes) =>{
                string.push_str("\"pathes\":[");
                pathes.iter().for_each(|path|{
                    string.push_str(&path.print());
                    string.push_str(",");
                });
                string.push_str("]");
            },
            None =>{
                string.push_str("\"pathes\":\"None\"");
            }
        }
        string.push_str("}"); 
        string
    
    }
    
}

/// item for 
#[derive(Debug, Eq, PartialEq,Clone,Encode,Decode)]
pub struct RouteItem {
    //计算得到的所有路径中最小的ttl
    min_ttl:u8,
    //这个分支下所有的路径
    pub(crate) pathes:Vec<RoutePathItem>,
    //这个分支的签名
    sign:Vec<u8>,
}

#[derive(Debug, Eq, PartialEq,Clone,Encode,Decode)]
pub struct RouteItemsWithTime {
    create_time:u64,
    item:RouteItem,
}
impl RouteItemsWithTime{
    ///! ### 合并同一时间点的一组路由信息
    ///! 合并规则
    ///! 1. 检查new_items的每一个分支， 忽略被包含的分支
    ///! 2. 然后看剩余的每一个分支，与现有的分支之和是否小于3，如果小于3，直接合并
    ///! 3. 如果大于3，从新的中找一个ttl最小的，然后在新的+老的中找出另外两个ttl最小的
    pub fn merge(&mut self,new_items:& RoutePathItem){
        let mut contained=Vec::new();
        match & new_items.pathes {
            Some(pathes) =>{
                for (i,val) in pathes.iter().enumerate(){
                    let mut item_contained = false;
                    self.item.pathes.iter().for_each(|path|{
                        if !item_contained {
                            item_contained = path.contains(val);
                        }
                    });
             
                    if item_contained {
                        contained.push(i);
                    }
                }
                //不是完全包含的，需要合并
                if contained.len() < pathes.len() {
                    if pathes.len() - contained.len() + self.item.pathes.len() <= MaxItemEachNext {
                        //合并后不超过MaxItemEachNext的
                        for i in 0..pathes.len() {
                            if !contained.contains(&i) {
                                self.item.pathes.push(pathes[i].clone());
                            }
                        }
                    }else{
                        //需要删除一些ttl最长的，为了避免数据的多次克隆，我们尽量保留self.item.pathes的位置
                        let mut in_ttls:Vec<(u8,usize)> = Vec::new();
                        for(index,path) in pathes.iter().enumerate(){
                            in_ttls.push((path.getMinTTL(),index));
                        };
                        in_ttls.sort();
                        //in_ttls是按照ttl从小到大排列了
                        let mut no_new_item = false;
                        let mut self_ttl_info:Vec<(u8,usize)> = Vec::new();
                        for (index,path) in self.item.pathes.iter().enumerate(){
                            self_ttl_info.push((path.getMinTTL(),index));
                        }
                        self_ttl_info.sort();
                        let mut cnt_of_self_pathes = self_ttl_info.len();
                        let mut cur_to_replace = cnt_of_self_pathes;
                        for (_,(ttl,index)) in in_ttls.iter().enumerate(){
                            if !contained.contains(&index) {
                                if cnt_of_self_pathes < MaxItemEachNext {
                                    //不满，直接放在后面
                                    self.item.pathes.push(pathes[*index].clone());
                                    cnt_of_self_pathes += 1;
                                    no_new_item = false;
                                }else{
                                    //总共已经超过了，在里面替换一个最旧的
                                    if cur_to_replace>0 {
                                        //强制替换或者小的TTL才替换
                                        if no_new_item || (self.item.pathes[cur_to_replace].getMinTTL() <= pathes[*index].getMinTTL()){
                                            no_new_item = false;
                                            self.item.pathes[cur_to_replace] = pathes[*index].clone();
                                            cur_to_replace -= 1;
                                
                                            if cur_to_replace == 0 {
                                                break;
                                            }
                                        }
                                       
                                    } 
                                }
                            }
                        }   
                    }
                }
            },
            None=>{},
        }

    }


}

type RouteItemsWithTimeVec = Vec<RouteItemsWithTime>;

type RouteItemMapByTime=HashMap<u64,RouteItem>;

fn from_map(items: &'static RouteItemMapByTime) -> RouteItemsWithTimeVec{
    let  result = items.iter().map(|(key,item)|{
        RouteItemsWithTime{
            create_time:*key,
            item:item.clone(),
        }
    }).collect();
   result
}
fn to_map(item_vec:&RouteItemsWithTimeVec)->RouteItemMapByTime {
    let mut result = HashMap::new();
    item_vec.iter().for_each(|item|{
        result.insert(item.create_time,item.item.clone());
    });
    result
}
///
///  RouteItem 合并过程
/// 
/// 
impl RouteItem {
    pub fn buildMinTTL(&mut self){
        let mut min_ttl:u8 = 255;
        self.pathes.iter().for_each(move |path_item|{
            let ttl = path_item.getMinTTL();
            if ttl < min_ttl {
                min_ttl = ttl;
            }
        });

        self.min_ttl = min_ttl+1;
    }
    fn contains(&self, route_item:&RouteItem)->bool {
        let mut  not_contained = false;
        //只要生成了routeItem，就不会有空数组
        route_item.pathes.iter().for_each(|pathItem|{
            if !not_contained {
                let mut branch_contained = false;
                self.pathes.iter().for_each(|selfPathBranch|{
                    if !branch_contained {
                        branch_contained = selfPathBranch.contains(pathItem);
                    }
                });
                if !branch_contained { //某个分支没有包含，整个就不包含了
                    not_contained = true;
                }
            }
        });
        !not_contained
    }
    //合并与自己的下一跳地址一致的表项，注意这里并不验证其有效性
     fn merge_route_item(&mut self,route_item:&RouteItem){
        let mut  ttl:u8 = 255;
        if !self.contains(route_item) {
            self.pathes.append(&mut route_item.pathes.clone());
        }
      
        if self.min_ttl > route_item.min_ttl {
            self.min_ttl = route_item.min_ttl;
        }
        //TODO 合并签名   
    }

}




pub struct RouteTable{
    routeItems : LruCache<Vec<u8>,HashMap<Vec<u8>,RouteItemMapByTime>>,
}
impl  RouteTable {
    pub fn new() -> Self{
        RouteTable{
            routeItems : LruCache::new(MaxRouteItemCount),
        }
    }

    ///！ 这个是从上一节点过来的一组路由信息，需要注意的是
    ///！ 1. 传入时，items中每一项的min_ttl应该是正确的
    ///！ 2. items中，每一个time对应的分支，都是有签名的，可以单独合并
    pub fn add_route_items(&mut self,target:Vec<u8>,next:Vec<u8>,items:&RouteItemsWithTimeVec)->Result<(),SystemTimeError>{
        match self.routeItems.get_mut(&target) {
            Some(route_of_target) =>{

                //有此target的路由，寻找next的项
                match route_of_target.get_mut(&next) {
                    Some(route_of_next) =>{
                        let mut insert_new_time = false;
                        items.iter().for_each(|item|{
                            match route_of_next.get_mut(&item.create_time) {
                                //有这一项就合并
                                Some(org_item) =>{
                                    org_item.merge_route_item(&item.item);
                                },
                                None =>{
                                    //没有这个时间点的信息
                                    route_of_next.insert(item.create_time,item.item.clone());
                                    insert_new_time = true;
                                }
                            }
                        });
                        if insert_new_time {
                            let mut oldest = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                            let mut max_ttl_key = oldest;
                            let mut max_ttl = 0;
                            //看看要不要删除最久的，或是最长的
                            if route_of_next.len() > MaxItemEachNext {
                                route_of_next.iter().for_each(|(create_time,item)| {
                                    if *create_time < oldest {
                                        oldest = *create_time;
                                    }
                                    if item.min_ttl > max_ttl {
                                        max_ttl = item.min_ttl;
                                        max_ttl_key = *create_time;
                                    }
                                });
                            }
                            //最久的超过10分钟才删除，否则
                            if (SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()- oldest) > RouteExpireTime {
                                route_of_next.remove(&oldest);    
                            }else{
                                route_of_next.remove(&max_ttl_key);
                            }
                                          
                        }
                    },
                    None =>{
                        let mut route_time = HashMap::new();
                        items.iter().for_each(|item|{
                        //没有这向这个peer的路由？直接加上
                            route_time.insert(item.create_time, item.item.clone());                          
                        });
                        route_of_target.insert(next,route_time);
                    }
                    
                }

                if route_of_target.len() > MaxItemEachNext {
                    //怎么删除法？
                }
            },
            None =>{
                let mut route_time = HashMap::new();
                items.iter().for_each(|item|{
                //没有这向这个peer的路由？直接加上
                    route_time.insert(item.create_time, item.item.clone());
                    
                });
                let mut route_of_next = HashMap::new();
                route_of_next.insert(next,route_time);
                self.routeItems.put(target,route_of_next);
            }
        }
        Ok(())
    }
  
    //读取有效的路由表项
    pub fn get(&self,target: Vec<u8>)->Option<HashMap<Vec<u8>,RouteItemMapByTime>>{
        // Some(self.routeItems.get(&target)?)
        None
    }

}

#[cfg(test)] 
mod tests{
    use std::error::Error;
    use std::fs::{OpenOptions,File};
    use std::io::prelude::*;
    use std::path::Path;
    use std::io::{self, BufReader};
    use std::str::FromStr;
    use std::convert::From;
    use crate::{PeerId};
    use super::{*};
    fn readline_as_id(filename:&str) -> io::Result<Vec<PeerId>> {
        let f = File::open(filename)?;
        let f = BufReader::new(f);
    
        let mut results:Vec<PeerId> = vec![];
        for line in f.lines() {
            if let Ok(line) = line {
                results.push(PeerId::from_str(&line).unwrap());
            }
        }
        Ok(results)
    }
    fn create_id_file(filename:&str)->io::Result<()>{
        let filename = filename;
        let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    //.create_new(true)
                    .append(true)
                    .open(filename);

        match file {
            Ok(mut stream) => {
                for _i in 1..100 {
                    let str_val = PeerId::random().to_base58() +"\n";
                    stream.write_all(str_val.as_bytes()).unwrap();
                }

            }
            Err(err) => {
                println!("{:?}", err);
            }
        }
        Ok(())
    }
    fn buildPeerIds()->io::Result<Vec<PeerId>>{
        // Create a path to the desired file
        let ids_file = String::from("peers.txt");
        let path = Path::new(&ids_file);
        if !path.exists() {
            create_id_file(&ids_file);
        }
        
        readline_as_id(&ids_file)

        // `file` goes out of scope, and the "hello.txt" file gets closed
    }
    fn extend_path_item(item:&mut RoutePathItem,level:u8,max_branch:u8,peers:&Vec<PeerId>) {
       
        if level == 0 {
            item.pathes = None;
            return;
        }
        let x = rand::random::<u8>()  ;
        let to_branch = x % 4;
        println!("level:{:?}, branch:{:?}",level,to_branch);
        if to_branch == 0 {
            item.pathes = None;
        }else {
            let mut pathes = vec![];
            
            for _i in 0..to_branch as usize {
                let mut sub_item = RoutePathItem {
                    next: peers[x as usize % 99].clone().into_bytes(),
                    pathes:None,
                    min_ttl:1,
                    sign:None,
                };
                extend_path_item(&mut sub_item,level-1,max_branch,peers);
                
                pathes.push(sub_item);
            }
            item.pathes = Some(pathes);
            item.update_min_ttl();
        }
    }
    #[test]
    fn route_path_item_get_ttl(){
        let peers  = buildPeerIds().unwrap();
        let mut one_hop = RoutePathItem{
            next:peers[0].clone().into_bytes(),
            pathes:None,
            min_ttl:1,
            sign:None,
        };
        assert_eq!(one_hop.getMinTTL(),1);

        //多级的测试,测试最多20层
        extend_path_item(&mut one_hop,12,3,&peers);
        println!("==================== pathes with min_ttl {:?}=====================",one_hop.getMinTTL());
        println!("pathes:{:?}",one_hop.print());

        //添加一个不同的分支
        //添加一个相同的分支
        //添加一个相同的树
        //添加一个不同的树
        //添加一个新时间的分支
        //添加三个新时间的分支
        
        //添加一个超时的分支
    }

}