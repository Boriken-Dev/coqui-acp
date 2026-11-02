# coqui-acp-page

A person's Agent Client Protocol agents, from a Rust page compiled to WebAssembly: a thin layer over `document.acp`, the [page contract](https://boriken-dev.github.io/coqui-acp/browser/). A browser provider, usually an extension, holds the connection and proves the agent. The page holds only what the person granted it.

## Features

- `find`: a provider by event, so markup in the page cannot shadow it.
- `Acp::agents`, `request`, `connect`, `revoke`, as the contract has them.
- `Connection::send`, `receive`, `closed`: one complete JSON-RPC message per string, so any ACP library fits on top.

## Installation

Not on crates.io yet:

```sh
cargo add --git https://github.com/Boriken-Dev/coqui-acp coqui-acp-page
```

## Quick start

```rust,ignore
let acp = coqui_acp_page::find(1000).await?.expect("no provider");
let agents = acp.request(&coqui_acp_page::Request::with_purpose("write this page's tests")).await?;  // from a user action
let connection = acp.connect(&agents[0]).await?;
connection.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}"#).await?;
let answer = connection.receive().await?;
```

`examples/page.rs` is a whole page. Build it with `cargo build -p coqui-acp-page --example page --target wasm32-unknown-unknown`, then `wasm-bindgen --target web`. A page with a content security policy needs `script-src 'wasm-unsafe-eval'`.

## License

[MPL-2.0](../../LICENSE).
