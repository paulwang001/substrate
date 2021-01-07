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
    // use super::{BucketTable,NUM_BUCKETS};
    use crate::protocol::transpp::routetab::{RouteItem, RouteTable};
    use std::time::SystemTime;
    use std::collections::HashMap;
    use crate::protocol::transpp::buckets::BucketTable;
    #[test]
    fn it_hello(){

    }

    //
    // fn readline_as_id(filename:&str) -> io::Result<Vec<PeerId>> {
    //     let f = File::open(filename)?;
    //     let f = BufReader::new(f);
    //
    //     let mut results:Vec<PeerId> = vec![];
    //     for line in f.lines() {
    //         if let Ok(line) = line {
    //             results.push(PeerId::from_str(&line).unwrap());
    //         }
    //     }
    //     Ok(results)
    // }
    // fn create_id_file(filename:&str)->io::Result<()>{
    //     let filename = filename;
    //     let file = OpenOptions::new()
    //                 .read(true)
    //                 .write(true)
    //                 .create(true)
    //                 //.create_new(true)
    //                 .append(true)
    //                 .open(filename);
    //
    //     match file {
    //         Ok(mut stream) => {
    //             for _i in 0..100 {
    //                 let mut  str_val = PeerId::random().to_base58();
    //                 str_val.push_str("\n");
    //                 stream.write_all(str_val.as_bytes()).unwrap();
    //             }
    //
    //         }
    //         Err(err) => {
    //             println!("{:?}", err);
    //         }
    //     }
    //     Ok(())
    // }
    // fn buildPeerIds()->io::Result<Vec<PeerId>>{
    //     // Create a path to the desired file
    //     let ids_file = String::from("peers.txt");
    //     let path = Path::new(&ids_file);
    //     if !path.exists() {
    //         create_id_file(&ids_file);
    //     }
    //
    //     readline_as_id(&ids_file)
    //
    //     // `file` goes out of scope, and the "hello.txt" file gets closed
    // }
    //
    // fn buildSortedPeerIds()->io::Result<Vec<PeerId>>{
    //     let mut result = buildPeerIds()?;
    //     result.sort_by(|a,b|{
    //         let av = u32::from_be_bytes(*pop(&a.as_bytes()[2..6]));
    //         let bv = u32::from_be_bytes(*pop(&b.as_bytes()[2..6]));
    //
    //         av.cmp(&bv)
    //     });
    //     Ok(result)
    //
    // }
    // fn same_value(v1: &Vec<u8>,v2:&Vec<u8>) -> bool{
    //     let mut ret = true;
    //     if v1.len() == v2.len() {
    //         for (k,_) in v1.iter().enumerate(){
    //             if v2[k] != v1[k] {
    //                 ret = false;
    //                 break;
    //             }
    //         }
    //     }else{
    //         ret = false;
    //     }
    //     ret
    // }
    // fn match_route_items(route_tab:&mut RouteTable,target:Vec<u8>,items:& HashMap<Vec<u8>,RouteItem>)->bool {
    //     let tab_item = route_tab.get(target.clone()).unwrap();
    //     if tab_item.len() != items.len(){
    //         return false;
    //     }
    //     for (_,item) in tab_item.iter().enumerate() {
    //         match items.get(item.0) {
    //             Some(item_data) =>{
    //                 if (item_data.update_time != item.1.update_time) || (item_data.ttl != item.1.ttl){
    //                     return false;
    //                 }
    //             }
    //             None =>{
    //                 return false;
    //             }
    //         }
    //     }
    //     return true;
    // }
    #[test]
    fn it_check_saturation()->io::Result<()>{
        use log::info;
        env_logger::init();
        //
        // let peers  = buildPeerIds()?;
        // let mut tab = BucketTable::new(peers[0].clone());
        // for index in 1..peers.len()-1 {
        //     tab.PeerConnected(&peers[index]);
        //     info!("\n====================round:{:?} \t saturation:{:?}====================\n",index,tab.get_saturation());
        //     for bucket_index in 0..NUM_BUCKETS {
        //         info!("bucket index:{:?}, peers count:{:?}\n",bucket_index,tab.buckets[bucket_index].len());
        //     }
        // }
        Ok(())
    }
    #[test]
    fn route_path_item_test(){
        // let peers = buildPeerIds().unwrap();
        // let mut route_tab = RouteTable::new();
        // let base_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        //对于同一target                
        //写入三条路由
        //  route_tab.add_route_items(peers[0].clone().into_bytes(), peers[1].clone().into_bytes(), base_time  , 3).unwrap();
        //  route_tab.add_route_items(peers[0].clone().into_bytes(), peers[2].clone().into_bytes(), base_time +10 , 3).unwrap();
        //  route_tab.add_route_items(peers[0].clone().into_bytes(), peers[3].clone().into_bytes(), base_time +20 , 3).unwrap();
        //  let mut test_map = HashMap::new();
         // test_map.insert(peers[1].clone().into_bytes(),RouteItem{update_time:base_time,ttl:3});
         // test_map.insert(peers[2].clone().into_bytes(),RouteItem{update_time:base_time+10,ttl:3});
         // test_map.insert(peers[3].clone().into_bytes(),RouteItem{update_time:base_time+20,ttl:3});
         // assert_eq!(match_route_items(&mut route_tab, peers[0].clone().into_bytes(), &test_map),true);
        
         //再写入第四条路由,第一条应该会被覆盖
        // route_tab.add_route_items(peers[0].clone().into_bytes(), peers[4].clone().into_bytes(), base_time +30 , 3).unwrap();
        // test_map.remove(&peers[1].clone().into_bytes());
        // test_map.insert(peers[4].clone().into_bytes(),RouteItem{update_time:base_time+30,ttl:3});
        // assert_eq!(match_route_items(&mut route_tab, peers[0].clone().into_bytes(), &test_map),true);

        //ttl更大，时间更新，不超过10分钟，这条应该不会覆盖 
        // route_tab.add_route_items(peers[0].clone().into_bytes(), peers[4].clone().into_bytes(), base_time +50 , 4).unwrap();
        // assert_eq!(match_route_items(&mut route_tab, peers[0].clone().into_bytes(), &test_map),true);

        //ttl更大，时间更新，超过10分钟，这条应该会覆盖 
        // route_tab.add_route_items(peers[0].clone().into_bytes(), peers[4].clone().into_bytes(), base_time +1050 , 4).unwrap();
        // test_map.insert(peers[4].clone().into_bytes(),RouteItem{update_time:base_time+1050,ttl:4});
        // assert_eq!(match_route_items(&mut route_tab, peers[0].clone().into_bytes(), &test_map),true);
        //ttl相同，时间更新，这条应该会覆盖
        // route_tab.add_route_items(peers[0].clone().into_bytes(), peers[4].clone().into_bytes(), base_time +1050 , 3).unwrap();
        // test_map.insert(peers[4].clone().into_bytes(),RouteItem{update_time:base_time+1050,ttl:3});
        // assert_eq!(match_route_items(&mut route_tab, peers[0].clone().into_bytes(), &test_map),true);

        //ttl更小，时间更新，这条应该会覆盖
        // route_tab.add_route_items(peers[0].clone().into_bytes(), peers[4].clone().into_bytes(), base_time +1000 , 2).unwrap();
        // test_map.insert(peers[4].clone().into_bytes(),RouteItem{update_time:base_time+1000,ttl:2});
        // assert_eq!(match_route_items(&mut route_tab, peers[0].clone().into_bytes(), &test_map),true);
        
        //再写入最新的


    }

}