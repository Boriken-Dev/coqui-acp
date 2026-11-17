# coqui-acp-hub

One live Agent Client Protocol session, held independent of any client and served to any number of clients that attach and detach, like a session kept in `tmux`.

A `Hub` does no I/O: give it what arrives from the agent and from each client, and send the messages it returns over your own links. It keeps an append-only, bounded log of updates so a client can attach with `session/load` and replay from the start or from a cursor, fans live updates out to everyone attached, queues prompts from every client in order (each stamped with who sent it), puts a permission request to the clients your policy lets answer and takes the first valid answer, and leaves the session running when everyone detaches.

```rust
use coqui_acp_hub::{Client, Hub, Output};

let mut hub = Hub::builder("session-id").build();
hub.connect(Client::new("phone").label("Pixel").role("operator"))?;
let outputs: Vec<Output> = hub.from_client(&"phone".into(), &message)?;
```

The message shapes, the client identity and the cursor are in [`AGENTS.md`](AGENTS.md).
