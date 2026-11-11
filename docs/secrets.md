# Secret prompts

**Draft, version 1.** An extension to ACP for an agent to ask the person for a secret (a password, a key), or to show them one, without the model, a transcript or a log ever holding the value. The host that runs the agent talks to the frontend of the session's owner; the agent is only told that a value exists, by a handle the host keeps.

The key words MUST, MUST NOT, SHOULD and MAY are as in [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119). The reference implementation is `coqui_acp::secret` (Rust, feature `secret`) and `package:coqui_acp/secret.dart` (Dart); `vectors/secret.json` holds both to the same bytes.

## What it protects

It keeps a secret out of the model's context, the session history, exports and logs, and out of a relay between the person's frontend and wherever the value is used: the value is sealed to a key only its receiver holds, and the receiver opens it only if the identity it expects sent it. It does not stop an agent that may run a command from reading a value the person lets a command use. That boundary, the approvals and the output scrubbing belong to the host that serves the tools, not to this wire.

## Capability

A frontend that takes the extension says so in `clientCapabilities._meta` (`capabilities._meta` in ACP version 2) when it connects to the host:

```json
{"dev.boriken.coqui.acp/secrets": {"version": 1, "methods": ["request", "show"], "bindings": ["ecdsa-p256-sha256", "x25519-hmac-sha256"]}}
```

A host MUST serve secret tools only to a client that offers it, and a version it does not know is no offer. A host MUST send a request only to the frontend of the session's owner, never to every attached client.

## The form and what the person is asked

Every method carries a JSON Schema object as **JSON text** (a string), and optionally a guatiao-intake form document the same way. The text is hashed as it was sent, never re-serialized, so the same bytes are what the receiver bound and what the collector draws. The schema's `properties` are the questions; a property is secret when it has `writeOnly: true` or `x-sensitive: true`. The schema's own keywords say what a field is (`type`, `format`, `contentEncoding`, `contentMediaType`, `minLength`, `maximum`, ...); the agent sends no display hints and each frontend chooses how to prompt for or show a field. A frontend masks or hides a value from the field's secret mark alone, never from `format`, a title or a name.

**Flat, and nothing more.** A receiver MUST refuse a form that is not a flat object with at least one property, a name that is empty or holds a control character, a property whose type no control can show, a `required` naming a property the schema lacks, a `writeOnly` or `x-sensitive` that is not a boolean, a `format: "password"` without a secret mark, a whole-number bound beyond 2^53 - 1, and any limit it sets (by default 64 questions, 256 choices a question, 64 KiB of schema, 128 bytes a name). The schema's root has only `type` (`object`), `properties`, `required`, `title` and `description`: a root `default`, `allOf`, `if`, `dependentSchemas`, `$defs` or `additionalProperties` is refused. A **secret field** is text or a number and has only `type`, `title`, `description`, `minLength`, `maxLength`, `minimum`, `maximum`, `writeOnly`, `x-sensitive`, `contentEncoding`, `contentMediaType` and `format` (which can only be `password`); every other keyword (`default`, `const`, `enum`, `oneOf`, `anyOf`, `pattern`, `if`, `$ref`, `$dynamicRef`, `dependencies`, `contentSchema`, `multipleOf`, `readOnly`, `x-...`) is refused, because the schema is the agent's text and a value, a condition or a hint there would put the secret in it or make a frontend draw the field as something else.

**One reading of the text.** Every JSON text that is hashed (the schema, the layout, a show's values, an answer's plain values) is read strictly: an object that repeats a key is refused, because the digest covers both while a reader keeps one, and so is a string with a lone UTF-16 surrogate (a client whose strings can hold one MUST NOT hash it as U+FFFD: it refuses every hashed part that has one, the ids, the message and the names too).

**The layout** is guatiao-intake's form document over a flat form: `{"sections": [{"id", "title"?, "description"?}], "fields": {<question>: {"widget"?, "placeholder"?, "unit"?, "visibleWhen"?}}}`, and nothing else. A layout may place and label the questions; it MUST NOT change what a secret question is. A secret question has the `password` widget or none, and no other question has it (a frontend such as guatiao_flutter masks a field whose widget is `password` whatever the schema says); a question that is not secret has one of `text`, `textarea`, `number`, `slider`, `checkbox`, `toggle`, `select`, `radio` or `dialog`; a `visibleWhen` names a plain question of the form; a nested `form`, a question the form lacks and any other key are refused. The collector checks the layout it received (it is under the digest) before it draws anything.

A secret value is sent as its text: UTF-8 for text (canonical base64 text for bytes, when `contentEncoding` says `base64`; `base64url` is unpadded), the JSON number grammar for a number (`-?(0|[1-9][0-9]*)` for a whole number within 2^53 - 1; plus an optional fraction and exponent, finite, for any other number; no sign, space, leading zero, `.5` or `5.`). It is sealed as those bytes, so no JSON parser holds it. Unpadded base64url is canonical when it has no padding, only its own alphabet and zero trailing bits.

**The digest of what the person is asked** is SHA-256 over these parts, each with a 4-byte big-endian length: `dev.boriken.coqui.acp/secret/v1 context`, the method (`request` or `show`), the session id, the request id, the message, the schema text, one byte that is 1 when a layout was sent and else 0, the layout text (empty when none), one byte that is 1 when plain values were sent (a show) and else 0, and the plain values text (empty when none). The receiver's binding and every sealed field cover it, so a collector recomputes it from what it received before it draws anything: a schema with `writeOnly` removed, another message or session, or a changed layout is refused, because the binding no longer verifies. The request id is 1 to 128 bytes with no control character, and a host MUST NOT reuse one.

## `_coqui_acp/secret_request`

The host asks the person for values. The host is the receiver of the secret ones: it makes a key pair for this request and sends the public key with a binding made for the frontend.

```json
{
  "sessionId": "s1",
  "requestId": "6f1c8b6e-2d0e-4a52-9a43-0a6d3a8b8d11",
  "message": "The deploy needs your registry login",
  "schema": "{\"type\":\"object\",\"properties\":{\"user\":{\"type\":\"string\"},\"password\":{\"type\":\"string\",\"writeOnly\":true}},\"required\":[\"user\",\"password\"]}",
  "recipient": {
    "key": "<32 bytes, base64url>",
    "binding": {"method": "ecdsa-p256-sha256", "key": "<SPKI, base64url>", "expires": 1700000240, "audience": "sha256:<frontend>", "proof": "<64 bytes, base64url>"}
  }
}
```

The answer is `{"action": "accept", "plain": "...", "values": {...}, "auth": {...}}`, `{"action": "decline"}` or `{"action": "cancel"}`. `plain` is the plain fields as **JSON object text** (a string), hashed as it was sent, never re-serialized. `values` holds only the secret fields, each `{"enc": "<base64url>", "ct": "<base64url>"}`, and a plain field there is malformed. `auth` is the frontend's binding of the whole answer:

```json
{"action": "accept", "plain": "{\"user\":\"ana\"}", "values": {"password": {"enc": "...", "ct": "..."}}, "auth": {"method": "ecdsa-p256-sha256", "key": "...", "expires": 1700000240, "audience": "sha256:<host>", "proof": "..."}}
```

The agent is never sent the answer. The host verifies `auth` against the frontend it expects, opens the sealed fields, keeps each secret under a handle, and gives the agent the plain values and the handles.

## `_coqui_acp/secret_show_open` and `_coqui_acp/secret_show_deliver`

The host shows the person values. The frontend is the receiver of the secret ones, so showing takes two requests, one shape each, and the frontend makes its key pair after the first.

`secret_show_open`: the host sends the same fields as a request, without a recipient, and the plain values as JSON object text:

```json
{"sessionId": "s1", "requestId": "...", "message": "Your new API key", "schema": "{...}", "values": "{\"label\":\"ci\"}"}
```

The plain values are part of the digest of what is shown, so the frontend's binding covers them and a relay cannot change one.

The frontend answers `{"action": "ready", "recipient": {"key": "...", "binding": {...}}}`, or `decline` or `cancel`. The binding covers the show's digest and is made for the host.

`secret_show_deliver`: the host verifies the binding (below) and sends the secret values sealed to that key, with its binding of them:

```json
{"sessionId": "s1", "requestId": "...", "values": {"key": {"enc": "...", "ct": "..."}}, "auth": {...}}
```

The frontend refuses a deliver for a session and request id it has no open step pending for, verifies `auth` (over the open step's plain values text and the sealed values) against the host it expects, and answers `{"action": "shown"}` when the person has seen the values and closed them, or `cancel`. A frontend MUST keep what it opened only for the one request and clear it when the display closes.

## Sealing

Each secret field is one single-shot HPKE message ([RFC 9180](https://www.rfc-editor.org/rfc/rfc9180)): base mode, DHKEM(X25519, HKDF-SHA256) (KEM 0x0020), HKDF-SHA256 (KDF 0x0001), AES-128-GCM (AEAD 0x0001), empty `aad`. These are the primitives of Canoa's Noise method, and WebCrypto can run them. `enc` is the 32-byte encapsulated key and `ct` the ciphertext followed by its 16-byte tag. The `info` is the following, each part with a 4-byte big-endian length:

1. `dev.boriken.coqui.acp/secret/v1 field`
2. the method, `request` or `show`
3. the request id, as sent
4. the digest of what the person is asked
5. the field's name

A ciphertext therefore opens only for the field, request, method and form it was sealed for. Bytes are unpadded base64url everywhere.

## Binding

A key is useless to a collector unless it knows whose key it is: a key a pipe presents proves nothing, since any pipe could substitute its own. So every binding is made by an identity the checker already knows by pairing or pinning, and the checker MUST refuse any other, whatever else the message carries. The identity's id is the one the peer methods use: `sha256:` and the SHA-256 of its `SubjectPublicKeyInfo`.

```json
{"method": "...", "key": "<holder's SPKI>", "expires": 1700000240, "audience": "sha256:...", "proof": "..."}
```

There are two bindings per request. The **key binding** is the receiver's, over its per-request public key. The **values binding** is the sender's (`auth`), over the digest of the whole answer: SHA-256 of `dev.boriken.coqui.acp/secret/v1 values digest`, the plain values text (4-byte length, then the bytes as sent; for a show, the open step's `values` text), and then, for each sealed field in the byte order of the names, the name (4-byte length), `enc` and `ct` (4-byte length). The receiver verifies it before it reads or opens anything, so a plain value, a ciphertext or a name changed, swapped, added or dropped on the way is refused; only a sender it expects can make one.

What a binding proves is this statement (each part with a 4-byte length): `dev.boriken.coqui.acp/secret/v1 binding` or `... values`, the method, the request id, the digest of what is asked, the receiver's 32-byte HPKE public key, the payload (empty for a key binding, the answer's digest for a values binding), the `expires` as 8 bytes, and the audience's 32-byte id. A proof for one request cannot be moved to another request, form, key or checker.

**Audience.** A binding is made for one identity, and a checker MUST know its own id and refuse a binding made for anyone else. A checker with no id of its own cannot check an audience, so it takes no binding.

**Which identity.** A collector checking a key binding is told which identity the request must come from (the host it is paired with) and MUST refuse another, though it knows it. The receiver opening values is likewise told which sender it expects.

How an identity proves follows from what its key can do:

| Peer method | Identity key | Binding method | Proof |
| --- | --- | --- | --- |
| `webcrypto` | ECDSA P-256 (its handshake already signs with it) | `ecdsa-p256-sha256` | IEEE P1363 signature (`r` then `s`, 64 bytes, `s` low) over the statement; `key` is the 91-byte SPKI |
| `noise` | X25519 static key: it cannot sign | `x25519-hmac-sha256` | HMAC-SHA-256 of the statement under HKDF-SHA256 (no salt, one block) of the Diffie-Hellman of the holder's static key and the audience's, with the label `dev.boriken.coqui.acp/secret/v1 dh` and both public keys (holder first) as `info`; `key` is the 44-byte SPKI |
| `tls` | the certificate's key (ECDSA P-256 by default) | not yet | needs the certificate in the message and the trust store's chain checks; a later version adds a `chain` to the binding |

A signature with a high `s` is refused: one statement has one signature, and a proof's bytes are not an identifier of anything (an ECDSA signature can be re-encoded by someone who has seen it), so nothing may key a table or a replay set by them. A signer that does not normalize `s` (WebCrypto does not) is normalized by the one who makes the binding.

**The signer is a function, not a key.** The libraries take a signer (the message in, the 64-byte signature out) and, for X25519, a key agreement (a peer's public key in, the shared secret out). A product wires them to its platform key store or to WebCrypto, so an identity's private key never enters the library. Raw keys held in memory exist only as `coqui_acp::secret::unprotected` and `package:coqui_acp/secret_unprotected.dart`, for tests and tools.

**Limits of the Diffie-Hellman proof.** It is not transferable: only the checker can verify it, and it can only verify a holder it is paired with. Anyone who holds the checker's static key can also make a binding that verifies to the checker, so it is not evidence to a third party and is enough only between two paired peers. A remote runner that the person's frontend has never been paired with needs an ECDSA identity certified by a root the frontend trusts, with the audience bound; that is the `tls` row and the trust store, not this.

**Lifetime.** A binding ends at a time its maker sets (by default 240 seconds from now). A checker takes it for as long as it has not ended plus its skew (default 30 seconds) and refuses one that ends more than its maximum lifetime (default 5 minutes) plus the skew from now. The checker looks at the clock when it checks, so a receiver whose binding is checked after the person took long to answer must be asked again. A receiver opens one answer and then forgets its key; whoever owns receivers remembers the request ids for as long as a binding can verify (the maximum lifetime plus the skew), so one request is not answered twice. A receiver also refuses an answer that comes after its own binding ended plus the skew, whatever the sender proves.

**What a relay still sees.** The size of a ciphertext is the value's plus 48 bytes, so a relay learns how long each secret is; nothing is padded. It also sees which fields were answered and when.

## The host's tools

The agent never speaks these methods. The host serves it three MCP tools, `secret_request`, `secret_use` and `secret_show`, and keeps each secret value under a handle in memory for the session. `coqui-acp-secrets` is that host side: the store (cleared and zeroed when the session ends), the tools, the person's approval of every use, output scrubbing and the file rules.

- **Use.** `secret_use` writes a value to a file under the session's working roots, or runs a command with it in one environment variable. The person is shown the file or the command in full and must approve each use; an approval can be given for the rest of the session for that exact action and secret. The harness's permission mode (Full access, Bypass) never answers it.
- **Show.** `secret_show` takes the value from a handle, a file under the roots, a host environment variable or a command, and displays it through `secret_show_open` and `secret_show_deliver`. An environment variable or a command source needs the person's approval.
- **Scrubbing.** Everything a tool returns has the value, and its base64, hex, URL-encoded and JSON-escaped forms, replaced by the handle. This is a backstop for accidents: a reversed, split or otherwise transformed value passes it, and an agent that may run a command with the value can print it.
- **Routing.** A session has one frontend, the owner's, and a request goes to no other client; an answer is believed only from that identity.

## Refusals

A message that is not in the shapes above is `-32602` invalid params. A collector that refuses a key does not tell the agent more than that the person was not asked. Neither side puts a value, a key or a signature in an error message or a log.
