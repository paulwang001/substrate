use crate::{PeerId };
use std::{collections::HashSet};
use std::vec;
use std::convert::TryInto;
const NUM_BUCKETS: usize = 32;
const ALPHA:   usize = 3;
type BucketPeerId=HashSet<PeerId>;
/// A distance between two keys in the DHT keyspace.


pub struct BucketTable {
    local_key: PeerId,
    pub buckets:Vec<BucketPeerId>,
    saturation:u8,
}
fn pop(barry: &[u8]) -> &[u8; 4] {
    barry.try_into().expect("slice with incorrect length")
}
impl BucketTable {
    fn bucket_index(&self,target:&PeerId)-> usize{
        //ignore first two bytes which is format and length description
        let a = u32::from_be_bytes(*pop(&self.local_key.as_bytes()[2..6]));
        let b = u32::from_be_bytes(*pop(&target.as_bytes()[2..6]));

        let bucketDistance = a ^ b ;
       
        let mut bucketIndex = bucketDistance.leading_zeros() as usize;
        if bucketIndex >= NUM_BUCKETS {
            bucketIndex = NUM_BUCKETS -1;
        } 
        bucketIndex
    }
    pub fn new(my_id:PeerId) ->Self {
        let table = BucketTable{
            local_key:my_id.clone(),
            buckets: (0 .. NUM_BUCKETS).map(|_| HashSet::new()).collect(),
            saturation:0,
        };
        table
    }
    pub fn PeerConnected(&mut self,peer:&PeerId) {
         let bucketIndex =  self.bucket_index(peer);
        if !self.buckets[bucketIndex].contains(peer) {
            self.buckets[bucketIndex].insert(peer.clone());
            self.calc_saturation();
        }
    }

    pub fn PeerDisconnected(&mut self,peer:&PeerId){
        let bucketIndex =  self.bucket_index(peer);
        if self.buckets[bucketIndex].contains(peer) {
            self.buckets[bucketIndex].remove(peer);
            self.calc_saturation();
        }
    }
    pub fn calc_saturation(&mut self){
        let mut  temp_sat :u8 = 0;
        for (i,tab) in self.buckets.iter().enumerate() {
            if tab.len() < ALPHA {
                break;
            }
            temp_sat =  i as u8;
        }
        let mut nodes_over_sat = 0;
        for i in temp_sat as usize..NUM_BUCKETS-1 {
            nodes_over_sat += self.buckets[i].len();
            if nodes_over_sat >= ALPHA {
                break;
            }
        }
        if nodes_over_sat < ALPHA  {
            if temp_sat > 0 {
                temp_sat -= 1;
            }
        }
        self.saturation = temp_sat;
    }

    pub fn get_closet_peers(&mut self,target:&PeerId,amax_peers:usize)->Vec<PeerId> {
        let mut max_peers = amax_peers;
        if max_peers == 0 {
            max_peers = ALPHA;
        }
        let mut bucketIndex = self.bucket_index(target);
        let mut result = vec![];
        let mut totalCount = 0;
        if self.buckets[bucketIndex].contains(target){
            result.push(target.clone());
            totalCount = 1;
        }
        if bucketIndex <= self.saturation as usize {
            
            for (_,item) in self.buckets[bucketIndex].iter().enumerate() {
                if item != target{
                    result.push(item.clone());
                    totalCount += 1;
                    if totalCount >= max_peers {
                        break;
                    }
                }                
            }
        }else{
            for index in  (self.saturation+1) as usize..NUM_BUCKETS-1 {
                for (_,item) in self.buckets[index].iter().enumerate() {
                    if item != target{
                        result.push(item.clone());
                        totalCount += 1;
                        if totalCount >= max_peers {
                            break;
                        }
                    }                
                }  
            }
        }
        result
    }
    pub fn get_saturation(&self)->u8{
        self.saturation
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
    use super::{BucketTable,NUM_BUCKETS};
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
                for _i in 0..100 {
                    let mut  str_val = PeerId::random().to_base58();
                    str_val.push_str("\n");
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
    #[test]
    fn bk_check_saturation()->io::Result<()>{
        env_logger::init();
        log::info!("-------------------");
        let peers  = buildPeerIds().expect("");
        let mut tab = BucketTable::new(peers[0].clone());
        for index in 1..peers.len()-1 {
            tab.PeerConnected(&peers[index]);
            log::info!("\n====================round:{:?} \t saturation:{:?}====================\n",index,tab.get_saturation());
            for bucket_index in 0..NUM_BUCKETS {
                log::info!("bucket index:{:?}, peers count:{:?}\n",bucket_index,tab.buckets[bucket_index].len());
            }
        }
        Ok(())
    }

    #[test]
    fn it_best_peers() {
        env_logger::init();
        log::info!("-------------------");
        let peers  = buildPeerIds().expect("");
        let mut tab = BucketTable::new(peers[0].clone());
        for index in 1..peers.len()-1 {
            tab.PeerConnected(&peers[index]);
        }
        let my_peers = tab.get_closet_peers(&peers[0].clone(),5);
        for p in my_peers {
            log::info!("{:?}",p);
        }
    }

}