extern crate hyper;
extern crate futures;
extern crate serde_json;

use futures::future::Future;
use hyper::header::ContentLength;
use hyper::server::{Http, Request, Response, Service};
use futures::Stream;
use serde_json::Value;

struct ModMsg;

const NOT_FOUND_MSG: &'static str = "Not found";

impl Service for ModMsg {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;
    type Future = Box<Future<Item = Self::Response, Error = Self::Error>>;

    fn call(&self, req: Request) -> Self::Future {
        if req.method() == &hyper::Method::Post && req.path() == "/mod_msg" {
            let fut = req.body()
                .collect()
                .and_then(|chunk_vec| {
                    let mut req_body = vec![];
                    for chunk in chunk_vec {
                        req_body.extend(chunk.to_vec())
                    }
                    let v: Value = serde_json::from_slice(&req_body)
                         .map_err(|_| hyper::error::Error::Incomplete)?;
                    let v = v.as_object().ok_or(hyper::error::Error::Incomplete)?;
                    let orig = v.get("orig").ok_or(hyper::error::Error::Incomplete)?;
                    let res_body = format!("<<{}>>", orig);
                    let resp = Response::new()
                        .with_header(ContentLength(res_body.len() as u64))
                        .with_body(res_body);
                    Ok(resp)
                });
            Box::new(fut)
        } else {
            let resp = Response::new()
                .with_header(ContentLength(NOT_FOUND_MSG.len() as u64))
                .with_status(hyper::StatusCode::NotFound)
                .with_body(NOT_FOUND_MSG);
            let fut = futures::future::ok(resp);
            Box::new(fut)
        }
    }
}

fn main() {
    let addr = "127.0.0.1:10002".parse().unwrap();
    let server = Http::new().bind(&addr, || Ok(ModMsg)).unwrap();
    println!("Listen to {}", &addr);
    server.run().unwrap();
}
