# admichat

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