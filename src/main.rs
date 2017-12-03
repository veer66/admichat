extern crate config;
extern crate websocket;
extern crate futures;
extern crate ex_futures;
extern crate tokio_core;
extern crate hyper;
#[macro_use]
extern crate lazy_static;
extern crate regex;
extern crate reqwest;

use std::error::Error;
use websocket::message::{Message, OwnedMessage};
use websocket::server::InvalidConnection;
use websocket::async::Server;
use regex::Regex;
use tokio_core::reactor::{Handle, Core};
use futures::{Future, Sink, Stream, future};
use ex_futures::unsync::pubsub::{unbounded, UnboundedSender, UnboundedReceiver};
use hyper::uri::RequestUri;
use tokio_core::net::TcpStream;
use websocket::client::async::ClientNew;
use std::rc::Rc;
use std::cell::RefCell;
use std::borrow::Borrow;
use reqwest::unstable::async::{Client, Decoder};
use std::mem;
use std::io::{Read, Cursor};
use std::collections::HashMap;

#[macro_use]
extern crate serde_derive;

lazy_static! {
    static ref CHAT_GUEST_PATH_RE: Regex = Regex::new("^/chat-guest/\\w+").unwrap();
    static ref CHAT_HOST_PATH_RE:  Regex = Regex::new("^/chat-host").unwrap();
    static ref ROOM_MSG_RE: Regex = Regex::new("([^:]+):(.+)").unwrap();
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub ws_addr: String,
    pub mod_addr: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    pub server: ServerConfig,
}

pub fn load_config() -> Settings {
    config::Config::new()
        .merge(config::File::with_name("config"))
        .expect("Cannot find config")
        .deserialize::<Settings>()
        .expect("Cannot deserialize config")
}

enum ChatMessage {
    ToAll(OwnedMessage, String),
    ToGuest(OwnedMessage, String),
    ToHost(OwnedMessage),
}

type ServerFuture = ClientNew<TcpStream>;
type Content = ChatMessage;
type Sender = UnboundedSender<Content>;
type Receiver = UnboundedReceiver<Content>;

struct ChatServer {
    handle: Handle,
    tx: Rc<RefCell<Sender>>,
    rx: Receiver,
    mod_addr: String,
}

impl ChatServer {
    fn guest_tx_msg(msg: OwnedMessage) -> Option<OwnedMessage> {
        match &msg {
            &OwnedMessage::Ping(_) => Some(msg),
            &OwnedMessage::Text(_) => Some(msg),
            _ => None,
        }
    }

    fn host_tx_msg(msg: OwnedMessage) -> Option<OwnedMessage> {
        match &msg {
            &OwnedMessage::Ping(_) => Some(msg),
            &OwnedMessage::Text(_) => Some(msg),
            _ => None,
        }
    }

    fn remote_mod_msg(msg: OwnedMessage,
                      handle: &Handle,
                      mod_addr: &str)
                      -> Box<Future<Item = OwnedMessage, Error = websocket::WebSocketError>> {
        let client = Client::new(handle);
        match &msg {
            &OwnedMessage::Text(ref orig) => {
                let orig = orig.clone();
                let mut params = HashMap::new();
                let url = &format!("http://{}/mod_msg", mod_addr)[..];
                params.insert("orig", orig.clone());
                Box::new(client.post(url)
                    .json(&params)
                    .send()
                    .map_err(|_| websocket::WebSocketError::NoDataAvailable)
                    .and_then(|mut res| {
                        let body = mem::replace(res.body_mut(), Decoder::empty());
                        body.concat2()
                            .map_err(|_| websocket::WebSocketError::NoDataAvailable)
                    })
                    .and_then(|body| {
                        let mut body = Cursor::new(body);
                        let mut buf = String::from("");
                        body.read_to_string(&mut buf)
                            .map_err(|_| websocket::WebSocketError::NoDataAvailable)
                            .map(|_| buf)
                    })
                    .map(move |text_msg| {
                        OwnedMessage::Text(format!("{} => {}", orig, text_msg))
                    })) as
                Box<Future<Item = OwnedMessage, Error = websocket::WebSocketError>>
            }
            _ => Box::new(future::ok(msg.clone())),
        }
    }

    fn guest_msg_to_tx
        (msg: OwnedMessage,
         tx: Rc<RefCell<Sender>>,
         room: &String)
         -> Box<Future<Item = Rc<RefCell<Sender>>, Error = websocket::WebSocketError>> {
        let res = match msg {
            OwnedMessage::Ping(p) => {
                tx.borrow_mut()
                    .unbounded_send(ChatMessage::ToGuest(OwnedMessage::Pong(p), room.clone()))
            }
            OwnedMessage::Text(t) => {
                tx.borrow_mut()
                    .unbounded_send(ChatMessage::ToAll(OwnedMessage::Text(t), room.clone()))
            }
            _ => Ok(()),
        };
        match res {
            Ok(_) => Box::new(future::ok(tx)),
            Err(_) => Box::new(future::err(websocket::WebSocketError::NoDataAvailable)),
        }
    }

    fn host_msg_to_tx
        (msg: OwnedMessage,
         tx: Rc<RefCell<Sender>>)
         -> Box<Future<Item = Rc<RefCell<Sender>>, Error = websocket::WebSocketError>> {

        let res = match msg {
            OwnedMessage::Ping(p) => {
                tx.borrow_mut()
                    .unbounded_send(ChatMessage::ToHost(OwnedMessage::Pong(p)))
            }
            OwnedMessage::Text(t) => {
                let cap = ROOM_MSG_RE.captures(&t[..]).unwrap();
                tx.borrow_mut()
                    .unbounded_send(ChatMessage::ToAll(OwnedMessage::Text(cap[2].to_string()),
                                                       cap[1].to_string()))
            }

            _ => Ok(()),
        };
        match res {
            Ok(_) => Box::new(future::ok(tx)),
            Err(_) => Box::new(future::err(websocket::WebSocketError::NoDataAvailable)),
        }
    }

    fn guest_rx_msg(msg: &ChatMessage, room: &String) -> Option<OwnedMessage> {
        match msg.borrow() {
            &ChatMessage::ToAll(ref m, ref m_room) if m_room == room => Some(m.clone()),
            &ChatMessage::ToGuest(ref m, ref m_room) if m_room == room => Some(m.clone()),
            _ => None,
        }
    }

    fn host_rx_msg(msg: Rc<ChatMessage>) -> Option<OwnedMessage> {
        match msg.borrow() {
            &ChatMessage::ToAll(ref m, ref m_room) => {
                match m {
                    &OwnedMessage::Text(ref t) => {
                        Some(OwnedMessage::Text(format!("{}:{}", m_room, t)))
                    }
                    _ => None,
                }
            }
            &ChatMessage::ToHost(ref m) => Some(m.clone()),
            _ => None,
        }
    }

    fn build_guest_chat_future(&self, f: ServerFuture, room: &str) {
        let room_for_write = room.to_string();
        let room_for_read = room.to_string();
        let tx = self.tx.clone();
        let rx = self.rx.clone();
        let handle = self.handle.clone();
        let handle_inner = handle.clone();
        let mod_addr = self.mod_addr.clone();

        let f = f.and_then(|(s, _)| s.send(Message::text("INIT").into()))
            .and_then(move |s| {
                let (sink, stream) = s.split();
                let write_fut = stream.take_while(|msg| Ok(!msg.is_close()))
                    .filter_map(Self::guest_tx_msg)
                    .and_then(move |msg| Self::remote_mod_msg(msg, &handle_inner, &mod_addr[..]))
                    .fold(tx,
                          move |tx, msg| Self::guest_msg_to_tx(msg, tx, &room_for_write));
                let read_fut = rx.map_err(|_| websocket::WebSocketError::NoDataAvailable)
                    .filter_map(move |msg| Self::guest_rx_msg(&msg, &room_for_read))
                    .fold(sink, move |sink, msg| sink.send(msg));
                write_fut.map(|_| ()).select(read_fut.map(|_| ())).then(|_| Ok(()))
            })
            .then(|_| Ok(()));
        handle.spawn(f);
    }

    fn build_host_chat_future(&self, f: ServerFuture) {
        let tx = self.tx.clone();
        let rx = self.rx.clone();
        let handle = self.handle.clone();
        let f = f.and_then(|(s, _)| s.send(Message::text("INIT").into()))
            .and_then(move |s| {
                let (sink, stream) = s.split();
                let write_fut = stream.take_while(|msg| Ok(!msg.is_close()))
                    .filter_map(Self::host_tx_msg)
                    .fold(tx, move |tx, msg| Self::host_msg_to_tx(msg, tx));
                let read_fut = rx.map_err(|_| websocket::WebSocketError::NoDataAvailable)
                    .filter_map(Self::host_rx_msg)
                    .fold(sink, move |sink, msg| sink.send(msg));

                let mux = write_fut.map(|_| ()).select(read_fut.map(|_| ()));
                mux.then(|_| Ok(()))
            })
            .then(|_| Ok(()));
        handle.spawn(f);
    }


    fn run_server(conf: Settings) -> Result<(), Box<Error>> {
        let ws_addr = conf.server.ws_addr.clone();
        let mut core = Core::new().unwrap();
        let handle = core.handle();
        let server = Server::bind(ws_addr.clone(), &handle)?;

        let (tx, rx) = unbounded();
        let tx = Rc::new(RefCell::new(tx));

        let chat_server = ChatServer {
            handle: core.handle(),
            tx: tx,
            rx: rx,
            mod_addr: conf.server.mod_addr.clone(),
        };

        let f = server.incoming()
            .map_err(|InvalidConnection { error, .. }| error)
            .for_each(|(upgrade, _addr)| {
                if !upgrade.protocols().iter().any(|s| s == "rust-websocket") {
                    handle.spawn(upgrade.reject()
                        .map_err(move |e| println!("REJECT: '{:?}'", e))
                        .map(move |_| println!("REJECT: Finished.")));
                    return Ok(());
                }
                let path = upgrade.request.subject.1.clone();
                let f = upgrade.use_protocol("rust-websocket")
                    .accept();
                if let RequestUri::AbsolutePath(ref p) = path {
                    if CHAT_GUEST_PATH_RE.is_match(&p[..]) {
                        let room = p.split('/').nth(2).unwrap().to_string();
                        chat_server.build_guest_chat_future(f, &room[..]);
                    } else if CHAT_HOST_PATH_RE.is_match(&p[..]) {
                        chat_server.build_host_chat_future(f);
                    }
                }
                Ok(())
            });
        println!("Listening {} ...", &ws_addr);
        core.run(f)?;
        Ok(())
    }
}


fn main() {
    let conf = load_config();
    ChatServer::run_server(conf).unwrap_or_else(|e| {
        panic!("E = {:?}", e);
    })
}
