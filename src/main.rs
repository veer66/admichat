extern crate websocket;
extern crate futures;
extern crate ex_futures;
extern crate tokio_core;
extern crate hyper;
#[macro_use] extern crate lazy_static;
extern crate regex;

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

lazy_static! {
    static ref CHAT_GUEST_PATH_RE: Regex = Regex::new("^/chat-guest/\\w+").unwrap();
    static ref CHAT_HOST_PATH_RE:  Regex = Regex::new("^/chat-host").unwrap();
    static ref ROOM_MSG_RE: Regex = Regex::new("([^:]+):(.+)").unwrap();
}

enum ChatMessage {
    ToAll(OwnedMessage,String),
    ToGuest(OwnedMessage,String),
    ToHost(OwnedMessage)
}

type ServerFuture = ClientNew<TcpStream>;
type Content = ChatMessage;
type Sender = UnboundedSender<Content>;
type Receiver = UnboundedReceiver<Content>;

fn build_guest_chat_future(f: ServerFuture, handle: &Handle, tx:
                           Rc<RefCell<Sender>>, rx: Receiver, room: &str) {
    let room_for_write = room.to_string();
    let room_for_read  = room.to_string();
    let f = f.and_then(|(s, _)| s.send(Message::text("INIT").into()))
        .and_then(move |s| {
            let (sink, stream) = s.split();
            let write_fut = stream
                .take_while(|msg| Ok(!msg.is_close()))
                .filter_map(|msg| match &msg {
                &OwnedMessage::Ping(_) => Some(msg),
                &OwnedMessage::Text(_) => Some(msg),
                    _ => None
                })
                .fold(tx, move |tx, msg| {
//                    println!("GET msg={:?} room={}", msg, room);
                    let res = match msg {                                             
                        OwnedMessage::Ping(p) => tx.borrow_mut().unbounded_send(
                            ChatMessage::ToGuest(OwnedMessage::Pong(p), room_for_write.clone())),
                        OwnedMessage::Text(t) => tx.borrow_mut().unbounded_send(
                            ChatMessage::ToAll(OwnedMessage::Text(t), room_for_write.clone())),
                        _ => Ok(())
                    };                
                    match res {
                        Ok(_) => future::ok::<Rc<RefCell<Sender>>,
                                              websocket::WebSocketError>(tx),
                        Err(_) => future::err::<Rc<RefCell<Sender>>,
                                                websocket::WebSocketError>(
                            websocket::WebSocketError::NoDataAvailable)
                    }
                });
            let read_fut = rx
                .map_err(|_| websocket::WebSocketError::NoDataAvailable)
                .filter_map(move |msg| {
                    let room = &room_for_read;
                    match msg.borrow() {                        
                        &ChatMessage::ToAll(ref m, ref m_room) => {
                            if room == m_room {
                                Some(m.clone())
                            } else {
                                None
                            }
                        },
                        &ChatMessage::ToGuest(ref m, ref m_room) => {
                            if room == m_room {
                                Some(m.clone())
                            } else {
                                None
                            }
                        },
                        _ => None
                    }
                })
                .fold(sink, move |sink, msg| {
                    sink.send(msg)
                });
            
            let mux = write_fut.map(|_| ()).select(read_fut.map(|_| ()));
            mux.then(|_| Ok(()))
        })
        .then(|_| Ok(()));
    handle.spawn(f);
}

fn build_host_chat_future(f: ServerFuture, handle: &Handle, tx:
                          Rc<RefCell<Sender>>, rx: Receiver) {

    let f = f.and_then(|(s, _)| s.send(Message::text("INIT").into()))
        .and_then(move |s| {
            let (sink, stream) = s.split();
            let write_fut = stream
                .take_while(|msg| Ok(!msg.is_close()))
                .filter_map(|msg| match &msg {
                &OwnedMessage::Ping(_) => Some(msg),
                &OwnedMessage::Text(_) => Some(msg),
                    _ => None
                })
                .fold(tx, move |tx, msg| {
                    let res = match msg {
                        OwnedMessage::Ping(p) => tx.borrow_mut().unbounded_send(
                            ChatMessage::ToHost(OwnedMessage::Pong(p))),
                        OwnedMessage::Text(t) => {
                            let cap = ROOM_MSG_RE.captures(&t[..]).unwrap();
                            tx.borrow_mut().unbounded_send(
                                ChatMessage::ToAll(
                                    OwnedMessage::Text(cap[1].to_string()),
                                    cap[0].to_string()))
                        },
   
                        _ => Ok(())
                    };                
                    match res {
                        Ok(_) => future::ok::<Rc<RefCell<Sender>>,
                                              websocket::WebSocketError>(tx),
                        Err(_) => future::err::<Rc<RefCell<Sender>>,
                                                websocket::WebSocketError>(
                            websocket::WebSocketError::NoDataAvailable)
                    }
                });
            let read_fut = rx
                .map_err(|_| websocket::WebSocketError::NoDataAvailable)
                .filter_map(move |msg| {
                    match msg.borrow() {
                        &ChatMessage::ToAll(ref m, ref m_room) => {
                            match m {
                                &OwnedMessage::Text(ref t) =>
                                    Some(OwnedMessage::Text(
                                        format!("{}:{}", m_room, t))),
                                _ => None
                            }
                        },
                        &ChatMessage::ToHost(ref m) => {
                            Some(m.clone())
                        },
                        _ => None
                    }
                })
                .fold(sink, move |sink, msg| {
                    sink.send(msg)
                });
            
            let mux = write_fut.map(|_| ()).select(read_fut.map(|_| ()));
            mux.then(|_| Ok(()))
        })
        .then(|_| Ok(()));
    handle.spawn(f);
}


fn run_server() -> Result<(), Box<Error>> {
    let mut core = Core::new()?;
    let handle = core.handle();
    let server = Server::bind("0.0.0.0:10000", &handle)?;
    let (tx, rx) = unbounded();
    let tx = Rc::new(RefCell::new(tx));
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
            let f = upgrade
                .use_protocol("rust-websocket")
                .accept();
            if let RequestUri::AbsolutePath(ref p) = path {
                if CHAT_GUEST_PATH_RE.is_match(&p[..]) {
                    let room = p.split('/').nth(2).unwrap().to_string();
                    build_guest_chat_future(f, &handle, tx.clone(), rx.clone(), &room[..]);
                } else if CHAT_HOST_PATH_RE.is_match(&p[..]) {
                    build_host_chat_future(f, &handle, tx.clone(), rx.clone());
                }
            }
            Ok(())
        });
    core.run(f)?;
    Ok(())
}

fn main() {
    run_server().unwrap_or_else(|e| {
        panic!("E = {:?}", e);
    })
}
