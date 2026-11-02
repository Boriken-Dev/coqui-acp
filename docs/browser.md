# ACP in the browser

**Draft, version 1.** A contract for a web page to find a person's Agent Client Protocol agents and talk to them through whatever provider the person has: a browser extension today, perhaps the browser itself later. Any provider may implement it and any page may use it, with no library from this project. Nothing here is an upstream ACP or W3C standard, and it may change until a second provider passes its [conformance tests](#conformance).

The key words MUST, MUST NOT, SHOULD and MAY are as in [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119).

## What it builds on

| Layer | Used | Status |
| --- | --- | --- |
| Agent protocol | [ACP](https://agentclientprotocol.com), unchanged: no method here replaces one of its | v1 stable, v2 draft |
| Remote transport | [ACP's WebSocket profile](https://agentclientprotocol.com/rfds/streamable-http-websocket-transport), and this project's [WebTransport draft](profile.md) | Active RFD; WebTransport is ours |
| Network discovery | [DNS-SD](https://www.rfc-editor.org/rfc/rfc6763) over [mDNS](https://www.rfc-editor.org/rfc/rfc6762) | Standards; the service type below is unregistered |
| Page tools | [WebMCP](https://webmachinelearning.github.io/webmcp/), through [MCP over ACP](https://agentclientprotocol.com/rfds/mcp-over-acp) | Community Group draft; RFD draft |
| Provider discovery | The shape of [EIP-6963](https://eips.ethereum.org/EIPS/eip-6963): several providers, none overwriting another | A precedent, not a dependency |

ACP is the protocol once connected. This contract covers only what ACP does not: finding an agent, a person's consent, and what a page may ask of it.

## Why a page is not an ordinary ACP client

ACP was written for a client the person runs and trusts, such as an editor. [Its own text](https://agentclientprotocol.com/protocol/session-setup) says an MCP server configuration can launch a process. A page is a stranger the person visited, so passing its messages through unread would give any page with a grant:

- **a command on the agent's machine**, through a stdio entry in `session/new`'s `mcpServers`;
- **the person's history with that agent**, through `session/list` and `session/load`, which replays a whole conversation;
- **every tool the agent has, approved by the page itself**, by answering `session/request_permission`, or by `session/set_mode` to a mode that stops asking.

So a provider is a gateway with a [scope](#scope), not a pipe. The scope is small, mechanical and tested here so that nobody has to design it again.

## The page API

```webidl
partial interface Document {
  readonly attribute Acp? acp;           // absent when no provider is present
};

interface Acp {
  readonly attribute unsigned short version;                    // 1
  readonly attribute FrozenArray<AcpProviderInfo> providers;
  Promise<sequence<AcpAgent>> agents();                         // this origin's grants; never prompts
  Promise<sequence<AcpAgent>> request(optional AcpRequest options = {});
  Promise<AcpConnection> connect(AcpAgent agent, optional AcpConnectOptions options = {});
  Promise<undefined> revoke(AcpAgent agent);
  attribute EventHandler onagentschange;
};

dictionary AcpProviderInfo { required DOMString uuid; required DOMString name; required DOMString rdns; DOMString icon; };

dictionary AcpRequest {
  DOMString provider;        // a uuid, when the page lets the person choose among several; default the first registered
  DOMString purpose;         // the page's words, shown as the page's words
  sequence<unsigned short> acpVersions;
  sequence<DOMString> mcpOrigins;   // origins other than the page's it will name in mcpServers, shown to the person
  sequence<DOMString> peers;        // agents the page already knows, listed first
  boolean permissions;       // asks that the page answer the agent's permission requests; see Scope
};

dictionary AcpAgent {
  required DOMString id;     // opaque, stable, different for each origin
  required DOMString label;  // what the person calls it, or what it advertised
  required DOMString provider;
  required DOMString source; // "dns-sd", "manual", "paired", "local"
  required AcpIdentity identity;
  required AcpOffers offers;
  required DOMString status; // "available", "unreachable", "unknown"
  required AcpScopeInfo scope;
};

dictionary AcpIdentity { DOMString? peer; required boolean verified; DOMString? method; };   // method: any proof's name, not a fixed list
dictionary AcpOffers   { sequence<unsigned short> acpVersions; sequence<DOMString> transports; };
dictionary AcpScopeInfo { required boolean permissions; required DOMString lifetime; sequence<DOMString> mcpOrigins; }; // "once", "always"

dictionary AcpConnectOptions { AbortSignal signal; };

interface AcpConnection {
  readonly attribute AcpAgent agent;
  readonly attribute ReadableStream readable;   // of strings, one complete JSON-RPC message each
  readonly attribute WritableStream writable;   // of strings, one complete JSON-RPC message each
  readonly attribute Promise<AcpClosed> closed;
  undefined close();
};

dictionary AcpClosed { required DOMString reason; };   // "page", "agent", "navigation", "revoked", "failed"
```

Messages are strings, so a WebAssembly module or a Dart program crosses into JavaScript with the type it already has, and ACP's own libraries, which take a pair of streams, need only a parse and a stringify around them.

Errors are `DOMException`s: `NotAllowedError` for no grant, a refusal or no user activation; `NotFoundError` for an agent the provider no longer knows; `NetworkError` when it cannot be reached. An error never says which of "no such agent" and "not granted" applies, so a page cannot probe for agents.

### Rules for the page API

- `document.acp` exists only in a secure context's top-level document. A cross-origin frame gets nothing in this version.
- `agents()` returns what this origin was granted and nothing else. **A page learns about an agent only after the person grants it.** Never prompts, and is empty when there are no grants.
- `request()` needs [transient user activation](https://html.spec.whatwg.org/multipage/interaction.html#transient-activation). The provider asks in its own window, never inside the page, where a page could draw over it. That window lists the agents the provider knows and the ones it has discovered, and MUST let the person **grant any number of them, and add one by hand** there and then: by URL and fingerprint, or by pairing with a code. The candidates are shown there and not to the page. The page learns only what was granted, and refusing looks the same as having no agents.
- A provider MUST also let the person add, remove and revoke agents and grants outside any page's request.
- `connect()` never prompts. A connection belongs to its document: navigating, closing the tab, or `revoke` closes it at once, with the reason in `closed`.
- `id` differs per origin, so two sites cannot match their agents by it. `identity.peer` does not differ, and the person is told that the site will see it.

### Why `document.acp`

[WebMCP's draft](https://webmachinelearning.github.io/webmcp/) of 27 October 2026 defines `document.modelContext`, and [Chrome's guide](https://developer.chrome.com/docs/ai/webmcp/imperative-api) uses it. Older guides use `navigator.modelContext`. A page calling both should find them in the same place, and the document is the honest scope, since a grant's connections end with it.

A property on `document` can be shadowed by the page's own markup (`<img name="acp">`). That fails closed, as "no provider", and [finding a provider](#finding-a-provider) by event is not affected. A page that shows other people's HTML SHOULD find it by event.

### Finding a provider

`window.acp` was rejected for the reason EIP-6963 records of `window.ethereum`: whichever extension loaded last won. Here `document.acp` is a registry, defined the same way by whichever provider arrives first:

1. A provider MUST, if `document.acp` is not already a registry, define it, with the algorithm in `bindings/js/src/registry.mjs`. It then registers itself.
2. Registering dispatches `acp:ready` on `window`, with `detail.acp` holding the registry.
3. The registry answers an `acp:request` event on `window` with another `acp:ready`, for a page that started listening late.

A provider may arrive after the page has loaded, for instance when the person clicks an extension's button. A page should say what it can do without one, and listen.

Every provider reveals that it is installed, to every page it is injected into. So a provider SHOULD inject only where the person has allowed it (see [extensions](#a-provider-as-an-extension)).

## Scope

A provider MUST apply this to every message before it reaches the agent. It parses the JSON and forwards **its own serialization of what it parsed**, so a parser's disagreement over duplicate keys cannot reach the agent. A message that is not one JSON-RPC object is refused.

| From the page | Result |
| --- | --- |
| `initialize` | forwarded |
| `session/new` | forwarded with `cwd` replaced by the grant's, and `mcpServers` checked |
| `session/prompt`, `session/cancel`, `session/close` | forwarded for a session this origin opened with this agent |
| `session/load`, `session/resume` | the same, with `cwd` and `mcpServers` handled as for `session/new` |
| `session/set_model`, and `session/set_config_option` of category `model` or `thought_level` | forwarded for its own session |
| `session/set_mode`, and `session/set_config_option` of any other category, or none | forwarded for its own session only if the grant has `permissions`, since a mode can stop the agent asking |
| A response to a request the agent made of the page | forwarded |
| `authenticate`, `session/list`, anything else, any `_` extension | refused with `-32601`; a notification is dropped |

- **`cwd`** is the grant's, chosen by the person. Without one, `session/new` is refused.
- **`mcpServers`** may hold only `type` `http`, `sse` or `acp`. Anything else, above all an entry with a `command`, is refused whole. An `http` or `sse` URL MUST be `https` and have the page's origin or one in the grant's `mcpOrigins`, because the agent connects from its own network and sends the given headers there.
- **`_meta`** is removed from every message, since an agent may give it private meaning.

| From the agent | Result |
| --- | --- |
| A response | forwarded |
| `session/update` | forwarded for a session of this origin; dropped otherwise |
| `session/request_permission` | **answered by the person in the provider's window, and not shown to the page**; refused when nobody answers in time. Forwarded only if the grant has `permissions` |
| Any other request naming a session of another origin | refused |
| Any other request or notification (`fs/*`, `terminal/*`, `mcp/message`) | forwarded: it asks the page, and reaches nothing of the person's |

**Resuming.** A page that reloads, or opens again, may keep going: it remembers the agent's `id` and the ACP session id itself, finds the agent in `agents()`, connects, and sends `session/resume` or `session/load` with that session id, with no new prompt. This works as long as the grant lasts and the session still exists at the agent. An agent that no longer has it answers with its own error, and the page starts a new one.

- A provider MUST remember which sessions an origin opened with an agent for as long as the grant lasts, and MUST forget them when the grant ends. A session id another origin opened is refused, whether or not the page knows it: knowing an id is not permission to read a person's history.
- A grant of lifetime `once` lasts until the tab closes, across reloads. `always` lasts until revoked.
- A connection, by contrast, ends with its document, so **a turn in progress through a provider ends when the page does**. Whether its session can be resumed is up to the agent. Work that must outlive a tab belongs on a connection the page does not hold.

`permissions` is a grant the person gives only with the words "this site can approve what the agent does for you". It suits a person's own application, which has its own confirmation screens. It is never the default.

**Answering for the agent's reach.** A page that wants an agent to act with a person's authority on its service passes an `http` MCP server of its own origin in `mcpServers`, with a short-lived token for that conversation in `headers`. What the agent may do there is what the service allows that person. The token goes to the agent, so it must be worth no more than that conversation.

## Identity and trust

**Discovery is not trust, and neither is a label.** An mDNS answer is an unauthenticated hint that anyone on the link can send, so it can name a server that relays a proof to a real agent. A provider MUST prove the peer, and MUST tie later messages to the proof:

- `identity.verified` is true only when every message after the proof is protected by keys that came out of it, or by TLS the proof is bound to at this endpoint. A proof that only shows who holds a key does not count: whoever relayed it holds the channel. False is reported as false, including for a key nobody proved, and a page may refuse such an agent.
- A local agent may use no proof at all, or TLS that shows only a host name. A provider may connect to one only if the person added it knowing that, and reports `identity.verified` false, so a page can refuse it.
- The first use of an agent's key is the person's decision: pairing with a code, or accepting a fingerprint shown beside the label. It is never automatic.
- A key that changes is a new agent, whose grants do not carry over.
- Grants are keyed by the verified peer, the page's origin, and the provider's profile. A grant for one agent gives nothing on another.

**A provider's code must not come from the path it protects.** A proxy that serves a page's JavaScript can replace it, so an end-to-end session in that page protects nothing against that proxy. A provider ships its proof code with the extension or application.

**The person's key.** The provider's own identity proves the connection: the page holds no key and never sees one, and the agent's allow-list names the provider, not each site. A page that connects by itself, with no provider, proves with a key of its own. An agent may instead require a client certificate at TLS, which the browser offers from the person's store or smart card, as for any site. That proves the person to whatever ends the TLS, not to the agent behind a proxy, and whether a provider's background connection gets the browser's prompt is untested.

## WebMCP

These are two separate questions.

**A page's own tools, to an agent it has connected.** The page chooses, per session, and the default is none. A page opts in by putting an entry of `type: "acp"` in `session/new`'s `mcpServers`, answering the agent's `mcp/message` requests from `document.modelContext`. This needs no help from the provider and no new permission: a page that may prompt an agent can already say anything to it, and tools add no power over the agent that a prompt lacks. `exposeWebMcp` in `bindings/js` does this.

- It needs an agent advertising `mcpCapabilities.acp`, which upstream's [draft](https://agentclientprotocol.com/rfds/mcp-over-acp) makes optional. An agent without it gets no tools, and the page is told.
- The default is none because a tool's description is untrusted text in the agent's context, and a page may want an agent that has not read it.
- A tool marked `consequentialHint` runs only after a confirmation the page supplies; with none, it is refused. As WebMCP's draft says, a site's hints can be missing or wrong, so this protects a page from its own tools running unasked, not a person from a dishonest page.

**An agent reaching into pages.** A provider may also offer an agent the tools of pages the person chooses. That is the agent's access to the browser, a separate grant kept by the provider per agent, tab and capability, which this contract does not cover and no page can give. A page's permission to use an agent and an agent's permission to use a page never imply each other.

WebMCP does not replace this contract: it lets a page offer tools to an agent, and says nothing about a page finding one. Offering a person's agents to the browser's own assistant as WebMCP tools is out of scope for this version.

## Network discovery

### Service type

**`_acp-agent._tcp`**. `_acp._tcp` cannot be used, because [IANA's registry](https://www.iana.org/assignments/service-names-port-numbers/service-names-port-numbers.xhtml) assigns the service name `acp` to the Aeolon Core Protocol. `acp-agent` is not in the registry today and has not been requested, so this is provisional until it is registered.

A per-protocol type follows [`_a2a._tcp`](https://ftp.kaist.ac.kr/ietf/draft-zhao-a2a-dns-sd-00.xml). An advertiser MAY also publish `_agent._tcp` with `proto=acp`, as [an individual Internet-Draft](https://datatracker.ietf.org/doc/draft-jakab-dawn-agent-discovery-mdns/00/) proposes, but that draft has no standing yet and defines only A2A's keys.

### What is advertised

The instance name is the label. The SRV record gives host and port. TXT keys, none of them trusted:

| Key | Meaning |
| --- | --- |
| `txtvers` | `1`, required |
| `proto` | `acp`, required |
| `path` | the WebSocket path; default `/acp` |
| `tls` | `1` for `wss` |
| `wt` | a UDP port serving WebTransport on the same host, usually the SRV record's number |
| `wth` | `sha256:` and the hex hash of the certificate there, for `serverCertificateHashes`, when no certificate authority vouches for it |
| `peer` | the peer id, as a hint to match against one already paired |
| `acpv` | ACP versions, comma separated |

A client MUST treat all of it as hostile input. It MUST NOT offer a record that names a loopback address, which would point a person at a service on their own machine, or one that is plain beyond loopback.

Advertising tells everyone on the link that an agent exists, so it MUST be off unless asked for. A record carries nothing secret: a peer id is a public key's hash.

### On one machine

An agent bound to loopback is not reachable through an address mDNS could give others. It writes a descriptor instead, one JSON file per listener in a directory only its user can write, which is `$COQUI_ACP_DESCRIPTORS` if set, otherwise `coqui-acp/agents` under `%LOCALAPPDATA%`, `$XDG_RUNTIME_DIR` or `~/.local/state`, the first that is set:

```json
{ "txtvers": 1, "proto": "acp", "label": "My agent", "url": "ws://127.0.0.1:8765/acp", "peer": "sha256:…", "wt": null, "wth": null }
```

A descriptor MUST name loopback. A reader removes one nothing answers at, since a killed program leaves its file behind.

### The companion

A browser extension cannot query any of this itself: `chrome.mdns` is [documented only for Chrome Apps](https://developer.chrome.com/docs/apps/reference/mdns), a platform deprecated in 2020. It asks a native companion over [native messaging](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging), whose messages to the extension are limited to 1 MB. Discovery fits; relaying agent traffic through it means splitting larger messages.

The companion answers one request, `{ "op": "discover", "seconds": 3 }`, with `{ "candidates": [...] }` of at most 64, each `{ "label", "url", "peer", "source", "webtransport", "webtransportHash", "acpVersions" }`, where `source` is `dns-sd` or `local`. `coqui-acp-bridge` is one: `coqui-acp-bridge companion install --extension-id <id>` registers it, under the name `dev.boriken.coqui_acp_bridge`.

`wth` lets a browser open WebTransport to an agent with no certificate authority: browsers pin a hash only for a certificate valid for 14 days at most, so an agent's hash changes as it renews one. It can be a hint like the rest because TLS authenticates nothing here: a false hash only fails to open, and whichever transport opens has the same proof to pass. For the same reason a provider MAY try a WebSocket when WebTransport does not open. The same key at a new address or hash is the same agent, once the person chooses it, and its grants carry over.

A provider SHOULD search only when the person asks, since that starts a program and listens on the network, and MUST NOT show a page anything it found. Showing a candidate is not trusting it: it stays unchosen until the person chooses it, with its key beside its label, and choosing it is what accepts that key.

## A provider as an extension

Non-normative: what a Chromium Manifest V3 extension needs, as reference.

- **Getting into pages.** `externally_connectable` cannot serve this, since it [needs each site listed in the manifest](https://developer.chrome.com/docs/extensions/reference/manifest/externally-connectable). Use a small script in the page's main world that defines the registry, and a content script between it and the service worker. Check `sender.origin`, the tab and the document in the service worker: the page's world can forge any message, but not what the browser reports about the sender.
- **A strict page.** A page may run `script-src 'self'; connect-src 'self'`, and a provider MUST work under it: inject through `chrome.scripting` or a declared main-world content script, never an inline script tag.
- **Being asked, and asking for little.** Injecting everywhere needs permission for every site and tells every site the extension is there. A provider SHOULD start from the person's click on its button, and offer "let sites ask" as a choice that requests the host permission then.
- **Connecting.** The service worker holds the connection, so a page's `connect-src` does not limit it.

  | To | How |
  | --- | --- |
  | This machine | `ws://127.0.0.1`, or `localhost` |
  | The network, with a certificate the browser accepts | `wss://` |
  | The network, with a self-signed certificate | WebTransport with `serverCertificateHashes`, or through the companion |

- **Local network prompts.** Chrome [asks the person](https://developer.chrome.com/docs/modern-web-guidance/guides/security/local-network-access) before a public page reaches a local or loopback address, including over WebSocket and WebTransport. That guide does not mention extensions, so an extension's behaviour here has to be tested before it is relied on.

## Other languages

The contract is the page API and the DNS-SD record, so any language can take part:

- **WebAssembly** and **Dart** reach `document.acp` through JavaScript and exchange strings.
- **A native program** has no page: it browses DNS-SD itself, connects directly, and needs no scope, since it answers to the person who ran it.

## Conformance

`bindings/js/src/conformance.mjs` runs these against any provider, given a small driver that plays the person, a page's origin and an agent. A provider conforms when all pass.

| Id | A provider must |
| --- | --- |
| `registry` | define `document.acp` once, keep an earlier provider, and answer `acp:request` |
| `no-grant-no-agents` | show a page nothing without a grant, including an agent it has discovered |
| `no-grant-no-connection` | refuse to connect, with the same error as for an unknown agent |
| `activation` | refuse `request()` without user activation |
| `person-refuses` | return nothing and remember nothing when the person refuses |
| `person-chooses` | give a page exactly the agents the person chose, several or one added by hand during the prompt, and nothing about the others |
| `per-origin` | not let another origin use a grant, and give it a different `id` |
| `messages` | carry messages both ways, in order, unchanged in meaning |
| `revoke` | close at once on revocation, and refuse after it |
| `navigation` | close at once when the document goes |
| `no-stdio` | never let a `command` in `mcpServers` reach the agent |
| `mcp-origin` | never let a server of another origin reach the agent, unless granted |
| `cwd` | replace a page's `cwd` |
| `meta` | remove `_meta` |
| `sessions` | keep a page to sessions its origin opened, and refuse `session/list` |
| `unknown-methods` | refuse `authenticate` and any extension |
| `modes` | refuse a mode change without `permissions`, and an unknown configuration category, while allowing model and effort |
| `resume` | let a page that reloaded find its agent and resume its session with no prompt, and refuse a session id it did not open |
| `verified` | report `identity.verified` false for a key nobody proved |
| `strict-csp` | define `document.acp` and connect under `script-src 'self'; connect-src 'self'` (a browser check: the driver says how) |
| `permission-to-person` | answer a permission request in its own window, and keep it from the page |
| `duplicate-keys` | forward what it parsed, not what it received |
