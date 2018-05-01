use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::io;

use packet;
use packet::{Handshake};
use ntt;

use wallet_crypto::cbor;

/// Light ID create by the server or by the client
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone)]
pub struct LightId(pub u32);
impl LightId {
    /// create a `LightId` from the given number
    ///
    /// identifier from 0 to 1023 are reserved.
    ///
    /// # Example
    ///
    /// ```
    /// use protocol::{LightId};
    /// let id = LightId::new(0x400);
    /// ```
    pub fn new(id: u32) -> Self {
        assert!(id >= 1024);
        LightId(id)
    }
    pub fn next(self) -> Self {
        LightId(self.0 + 1)
    }
}

/// A client light connection will hold pending message to send or
/// awaiting to be read data
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct LightConnection {
    id: LightId,
    node_id: ntt::protocol::NodeId,
    received: Option<Vec<u8>>
}
impl LightConnection {
    pub fn new_with_nodeid(id: LightId, nonce: u64) -> Self {
        LightConnection {
            id: id,
            node_id: ntt::protocol::NodeId::make_syn(nonce),
            received: None
        }
    }
    pub fn new_expecting_nodeid(id: LightId, node: &ntt::protocol::NodeId) -> Self {
        LightConnection {
            id: id,
            node_id: node.clone(),
            received: None
        }
    }

    pub fn get_id(&self) -> LightId { self.id }

    /// tell if the `LightConnection` has some pending message to read
    pub fn pending_received(&self) -> bool {
        self.received.is_some()
    }

    /// consume the eventual data to read
    /// 
    /// to call only if you are ready to process the data
    pub fn get_received(&mut self) -> Option<Vec<u8>> {
        let mut v = None;
        ::std::mem::swap(&mut self.received, &mut v);
        v
    }

    /// add data to the received bucket
    fn receive(&mut self, bytes: &[u8]) {
        self.received = Some(match self.get_received() {
            None => bytes.iter().cloned().collect(),
            Some(mut v) => { v.extend_from_slice(bytes); v }
        });
    }
}

#[derive(Clone)]
pub enum ServerLightConnection {
    Establishing,
    Established(ntt::protocol::NodeId),
}

pub struct Connection<T> {
    ntt: ntt::Connection<T>,
    // this is a line of active connections open by the server/client
    // that have not been closed yet.
    server_cons: BTreeMap<LightId, ServerLightConnection>,
    client_cons: BTreeMap<LightId, LightConnection>,
    // this is for the server to map from its own nodeid to the client lightid
    map_to_client: BTreeMap<ntt::protocol::NodeId, LightId>,
    // potentialy the server close its connection before we have time
    // to process it on the client, so keep the buffer alive here
    //server_dones: BTreeMap<LightId, LightConnection>,
    //await_reply: BTreeMap<ntt::protocol::NodeId, >

    next_light_id: LightId
}

impl<T: Write+Read> Connection<T> {

    // search for the next free LIGHT ID in the client connection map
    fn find_next_connection_id(&self) -> LightId {
        let mut x = LightId(ntt::LIGHT_ID_MIN);
        while self.client_cons.contains_key(&x) {
            x = x.next();
        }
        return x;
    }

    fn get_free_light_id(&mut self) -> LightId {
        let id = self.next_light_id;
        self.next_light_id = id.next();
        id
    }

    pub fn new(ntt: ntt::Connection<T>) -> Self {
        Connection {
            ntt: ntt,
            server_cons: BTreeMap::new(),
            client_cons: BTreeMap::new(),
            map_to_client: BTreeMap::new(),
            //server_dones: BTreeMap::new(),
            next_light_id: LightId::new(0x401)
        }
    }

    pub fn handshake(&mut self, hs: &packet::Handshake) -> io::Result<()> {
        use ntt::protocol::{ControlHeader, Command};
        let lcid = self.find_next_connection_id();
        let lc = LightConnection::new_with_nodeid(lcid, self.ntt.get_nonce());

        /* create a connection, then send the handshake data, followed by the node id associated with this connection */
        self.ntt.create_light(lcid.0);
        self.send_bytes(lcid, &packet::send_handshake(hs));
        self.send_nodeid(lcid, &lc.node_id);

        self.client_cons.insert(lcid, lc);

        /* wait answer from server, which should a new light connection creation,
         * followed by the handshake data and then the node id
         */
        let siv = match self.ntt.recv().unwrap() {
            Command::Control(ControlHeader::CreatedNewConnection, cid) => { LightId::new(cid) },
            _ => { unimplemented!() }
        };

        fn data_recv_on<T: Read+Write>(con: &mut Connection<T>, expected_id: LightId) -> io::Result<Vec<u8>> {
            match con.ntt.recv().unwrap() {
                ntt::protocol::Command::Data(cid, len) => {
                    if cid == expected_id.0 {
                        let bytes = con.ntt.recv_len(len).unwrap();
                        Ok(bytes)
                    } else {
                        unimplemented!()
                    }
                }
                _ => { unimplemented!() }
            }
        };

        let server_bytes_hs = data_recv_on(self, siv)?;
        let _server_handshake : Handshake = cbor::decode_from_cbor(&server_bytes_hs).unwrap();

        let server_bytes_nodeid = data_recv_on(self, siv)?;
        let server_nodeid = match ntt::protocol::NodeId::from_slice(&server_bytes_nodeid[..]) {
            None   => unimplemented!(),
            Some(nodeid) => nodeid,
        };

        // TODO compare server_nodeid and client_id

        let scon = LightConnection::new_expecting_nodeid(siv, &server_nodeid);
        self.server_cons.insert(siv, ServerLightConnection::Established(server_nodeid));

        Ok(())
    }

    pub fn new_light_connection(&mut self, id: LightId) {
        self.ntt.create_light(id.0).unwrap();

        let lc = LightConnection::new_with_nodeid(id, self.ntt.get_nonce());
        self.send_nodeid(id, &lc.node_id);
        self.client_cons.insert(id, lc);
    }

    pub fn close_light_connection(&mut self, id: LightId) {
        self.client_cons.remove(&id);
        // TODO: this signal needs to be sent:
        // self.ntt.close_light(id.0);
    }

    pub fn has_bytes_to_read(&self, id: LightId) -> bool {
        match self.client_cons.get(&id) {
            None => false,
            Some(con) => {
                match &con.received {
                    None => false,
                    Some(v) => v.len() > 0,
                }
            }
        }
    }

    pub fn wait_msg(&mut self, id: LightId) -> io::Result<Vec<u8>> {
        while !self.has_bytes_to_read(id) {
            self.broadcast()
        }

        match self.client_cons.get(&id) {
            None => panic!("oops"),
            Some(con) => {
                match &con.received {
                    None => panic!("oops2"),
                    Some(v) => Ok(v.clone())
                }
            }
        }
    }

    /// get a mutable reference to a LightConnection so one can read its received data
    ///
    //pub fn poll<'a>(&'a mut self) -> Option<&'a mut LightConnection> {
    //    self.server_cons.iter_mut().find(|t| t.1.pending_received()).map(|t| t.1)
    //}

    //pub fn poll_id<'a>(&'a mut self, id: LightId) -> Option<&'a mut LightConnection> {
    //    self.server_cons.iter_mut().find(|t| t.0 == &id && t.1.pending_received()).map(|t| t.1)
    //}

    pub fn send_bytes(&mut self, id: LightId, bytes: &[u8]) {
        self.ntt.light_send_data(id.0, bytes).unwrap()
    }

    pub fn send_nodeid(&mut self, id: LightId, nodeid: &ntt::protocol::NodeId) {
        self.ntt.light_send_data(id.0, nodeid.as_ref()).unwrap()
    }

    // TODO return some kind of opaque token
    pub fn send_bytes_ack(&mut self, id: LightId, bytes: &[u8]) -> ntt::protocol::NodeId {
        match self.client_cons.get(&id) {
            None => panic!("send bytes ack ERROR. connection doesn't exist"),
            Some(con) => {
                self.ntt.light_send_data(id.0, bytes).unwrap();
                con.node_id.clone()
            }
        }
    }

    pub fn broadcast(&mut self) {
        use ntt::protocol::{ControlHeader, Command};
        match self.ntt.recv().unwrap() {
            Command::Control(ControlHeader::CloseConnection, cid) => {
                let id = LightId::new(cid);
                match self.server_cons.remove(&id) {
                    Some(ServerLightConnection::Establishing) => {},
                    Some(ServerLightConnection::Established(v)) => {
                        /*
                        if let Some(_) = v.received {
                            self.server_dones.insert(id, v);
                        }
                        */
                    },
                    Some(v) => {
                    },
                    None    =>
                        // BUG, server asked to close connection but connection doesn't exists in tree
                        {},
                }
            },
            Command::Control(ControlHeader::CreatedNewConnection, cid) => {
                let id = LightId::new(cid);
                if let Some(_) = self.server_cons.get(&id) {
                    panic!("light id created twice")
                } else {
                    //let con = LightConnection::new_expecting_nodeid(id);
                    self.server_cons.insert(id, ServerLightConnection::Establishing);
                }
            },
            Command::Control(ch, cid) => {
                println!("{}:{}: LightId({}) Unsupported control `{:?}`", file!(), line!(), cid, ch);
            },
            ntt::protocol::Command::Data(server_id, len) => {
                let id = LightId::new(server_id);
                match self.server_cons.get(&id) {
                    Some(slc) => {
                        match slc.clone() {
                            ServerLightConnection::Established(nodeid) => {
                                match self.map_to_client.get(&nodeid) {
                                    None => println!("ERROR bug cannot find node in client map"),
                                    Some(client_id) => {
                                        match self.client_cons.get_mut(client_id) {
                                            None => println!("ERROR bug cannot find client connection for receiving"),
                                            Some(con) => {
                                                let bytes = self.ntt.recv_len(len).unwrap();
                                                con.receive(&bytes);
                                            }
                                        }
                                    },
                                }
                            },
                            ServerLightConnection::Establishing => {
                                let bytes = self.ntt.recv_len(len).unwrap();
                                let nodeid = match ntt::protocol::NodeId::from_slice(&bytes[..]) {
                                    None         => panic!("ERROR: expecting nodeid but receive stuff"),
                                    Some(nodeid) => nodeid,
                                };

                                let scon = LightConnection::new_expecting_nodeid(id, &nodeid);
                                self.server_cons.remove(&id);
                                self.server_cons.insert(id, ServerLightConnection::Established(nodeid));

                                match self.client_cons.iter().find(|(k,v)| v.node_id.match_ack(nodeid)) {
                                    None => {},
                                    Some((z,_)) => {
                                        self.map_to_client.insert(nodeid, *z);
                                    }
                                }
                            },
                        }
                    },
                    None => {
                        println!("{}:{}: LightId({}) does not exists but received data", file!(), line!(), server_id)
                    },
                }
            },
        }
    }
}

pub mod command {
    use std::io::{Read, Write};
    use super::{LightId, Connection};
    use wallet_crypto::cbor;
    use block;
    use packet;

    pub trait Command<W: Read+Write> {
        type Output;
        fn cmd(&self, connection: &mut Connection<W>, id: LightId) -> Result<Self::Output, &'static str>;

        fn execute(&self, connection: &mut Connection<W>) -> Result<Self::Output, &'static str> {
            let id = connection.get_free_light_id();

            connection.new_light_connection(id);
            connection.broadcast(); // expect ack of connection creation

            let ret = self.cmd(connection, id)?;

            connection.close_light_connection(id);

            Ok(ret)
        }
    }

    #[derive(Debug)]
    pub struct GetBlockHeader(Option<block::HeaderHash>);
    impl GetBlockHeader {
        pub fn first() -> Self { GetBlockHeader(None) }
        pub fn some(hh: block::HeaderHash) -> Self { GetBlockHeader(Some(hh)) }
    }

    impl<W> Command<W> for GetBlockHeader where W: Read+Write {
        type Output = block::MainBlockHeader;
        fn cmd(&self, connection: &mut Connection<W>, id: LightId) -> Result<Self::Output, &'static str> {
            // require the initial header
            let (get_header_id, get_header_dat) = packet::send_msg_getheaders(&[], &self.0);
            connection.send_bytes(id, &[get_header_id]);
            connection.send_bytes(id, &get_header_dat[..]);
            let dat = connection.wait_msg(id).unwrap();
            let mut l : packet::BlockHeaderResponse = cbor::decode_from_cbor(&dat).unwrap();
            println!("{}", l);
    
            match l {
                packet::BlockHeaderResponse::Ok(mut ll) => {
                    match ll.pop_front() {
                        Some(block::BlockHeader::MainBlockHeader(bh)) => Ok(bh),
                        None => panic!("pop front")
                    }
                },
                _  => Err("No first main block header")
            }
        }
    }

    #[derive(Debug)]
    pub struct GetBlock {
        from: block::HeaderHash,
        to:   block::HeaderHash
    }
    impl GetBlock {
        pub fn only(hh: block::HeaderHash) -> Self { GetBlock::from(hh.clone(), hh) }
        pub fn from(from: block::HeaderHash, to: block::HeaderHash) -> Self { GetBlock { from: from, to: to } }
    }

    impl<W> Command<W> for GetBlock where W: Read+Write {
        type Output = Vec<u8>; // packet::block::Block;
        fn cmd(&self, connection: &mut Connection<W>, id: LightId) -> Result<Self::Output, &'static str> {
            // require the initial header
            let (get_header_id, get_header_dat) = packet::send_msg_getblocks(&self.from, &self.to);
            connection.send_bytes(id, &[get_header_id]);
            connection.send_bytes(id, &get_header_dat[..]);
            Ok(connection.wait_msg(id).unwrap())
        }
    }

}
