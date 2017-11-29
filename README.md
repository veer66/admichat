# admichat

A simple web chat for talking web admin

The server was written in Rust. Client-side code is written in plain HTML and JS.

## Prerequisite

* Rust and Cargo
* Ruby 2.4+
* sh

## How to run

### static file

````
ruby -run -ehttpd . -p10001
````

### websocket

In another window:
````
cargo run
````

## URL

* http://localhost:10001/chat-guest.html, for client/guest/customer
* http://localhost:10001/chat-admin.html, for admin


## TODO

* SSL
* Admin page should require authentication
