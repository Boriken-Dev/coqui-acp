# Secret prompts

**Draft, version 1.** An extension to ACP for an agent to ask the person for a secret (a password, a key), or to show them one, without the model, a transcript or a log ever holding the value. The host that runs the agent talks to the frontend of the session's owner; the agent is only told that a value exists, by a handle the host keeps.

The key words MUST, MUST NOT, SHOULD and MAY are as in [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119). The reference implementation is `coqui_acp::secret` (Rust, feature `secret`) and `package:coqui_acp`'s `AcpSecret*` types (Dart); `vectors/secret.json` holds both to the same bytes.

## What it protects

It keeps a secret out of the model's context, the session history, exports and logs, and out of a relay between the person's frontend and wherever the value is used: the value is sealed to a key only its receiver holds. It does not stop an agent that may run a command from reading a value the person lets a command use. That boundary, the approvals and the output scrubbing belong to the host that serves the tools, not to this wire.

## Capability

A frontend that takes the extension says so in `clientCapabilities._meta` (`capabilities._meta` in ACP version 2) when it connects to the host:

```json
{"dev.boriken.coqui.acp/secrets": {"version": 1, "methods": ["request", "show"], "bindings": ["ecdsa-p256-sha256", "x25519-hmac-sha256"]}}
```

A host MUST serve secret tools only to a client that offers it, and a version it does not know is no offer. A host MUST send a request only to the frontend of the session's owner, never to every attached client.

## The form

Both methods carry a JSON Schema object. Its `properties` are the questions; a property is secret when it has `writeOnly: true` or `x-sensitive: true`. The schema's own keywords say what a field is (`type`, `format`, `contentEncoding`, `contentMediaType`, `minLength`, `maximum`, ...); the agent sends no display hints and each frontend chooses how to prompt for or show a field. An optional `form` is a guatiao-intake form document that lays the questions out; this extension does not read it.

A receiver MUST refuse a form that is not a flat object with at least one property, a name that is empty or holds a control character, a property whose type no control can show, a `required` naming a property the schema lacks, and any limit it sets (by default 64 questions, 256 choices a question, 64 KiB of schema, 128 bytes a name). A secret field MUST be text or a number, and MUST NOT carry `default`, `const`, `enum`, `oneOf` or `examples`: the schema is the agent's text, and a value there would put the secret in it.

A secret value is sent as its text: UTF-8 for text (base64 text for bytes, when `contentEncoding` says `base64`), decimal text for a number. It is sealed as those bytes, so no JSON parser holds it.

## `_coqui/secret/request`

The host asks the person for values. The host is the receiver of the secret ones: it makes a key pair for this request and sends the public key with a binding.

```json
{
  "sessionId": "s1",
  "requestId": "6f1c8b6e-2d0e-4a52-9a43-0a6d3a8b8d11",
  "message": "The deploy needs your registry login",
  "schema": {
    "type": "object",
    "properties": {
      "user": {"type": "string", "title": "User"},
      "password": {"type": "string", "title": "Password", "writeOnly": true}
    },
    "required": ["user", "password"]
  },
  "recipient": {
    "key": "<32 bytes, base64url>",
    "binding": {"method": "ecdsa-p256-sha256", "key": "<SPKI, base64url>", "expires": 1700000300, "proof": "<64 bytes, base64url>"}
  }
}
```

The answer is `{"action": "accept", "values": {...}}`, `{"action": "decline"}` or `{"action": "cancel"}`. In `values` a plain field holds its value and a secret field holds `{"enc": "<base64url>", "ct": "<base64url>"}`:

```json
{"action": "accept", "values": {"user": "ana", "password": {"enc": "...", "ct": "..."}}}
```

The agent is never sent the answer. The host opens the sealed fields, keeps each secret under a handle, and gives the agent the plain values and the handles.

## `_coqui/secret/show`

The host shows the person values. The frontend is the receiver of the secret ones, so the method has two steps, told apart by `step`, and the frontend makes its key pair after the first.

`open`: the host sends the schema and the plain values, without the secret ones, and asks for the key:

```json
{"step": "open", "sessionId": "s1", "requestId": "...", "message": "Your new API key", "schema": {...}, "values": {"label": "ci"}}
```

The frontend answers `{"action": "ready", "recipient": {"key": "...", "binding": {...}}}`, or `decline` or `cancel`.

`deliver`: the host verifies the binding (below) and sends the secret values sealed to that key:

```json
{"step": "deliver", "sessionId": "s1", "requestId": "...", "values": {"key": {"enc": "...", "ct": "..."}}}
```

The frontend answers `{"action": "shown"}` when the person has seen the values and closed them, or `cancel`. A frontend MUST keep what it opened only for the one request and clear it when the display closes.

## Sealing

Each secret field is one single-shot HPKE message ([RFC 9180](https://www.rfc-editor.org/rfc/rfc9180)): base mode, DHKEM(X25519, HKDF-SHA256) (KEM 0x0020), HKDF-SHA256 (KDF 0x0001), AES-128-GCM (AEAD 0x0001), empty `aad`. These are the primitives of Canoa's Noise method, and WebCrypto can run them. `enc` is the 32-byte encapsulated key and `ct` the ciphertext followed by its 16-byte tag. The `info` is the following, each part with a 4-byte big-endian length:

1. `dev.boriken.coqui.acp/secret/v1 field`
2. the method, `request` or `show`
3. the request id, as sent
4. the field's name

A ciphertext therefore opens only for the field of the request and method it was sealed for; moved to another field or request, or opened by another key, it fails. Bytes are unpadded base64url everywhere. The request id is 1 to 128 bytes with no control character, and a host MUST NOT reuse one.

## Binding the key

The receiver's key is useless to a collector unless it knows whose key it is: a key a pipe presents proves nothing, since any pipe could substitute its own. So the receiver sends a **binding**, and the collector MUST refuse the key unless the binding verifies against an identity it already knows by pairing or pinning, whatever else the message carries. The identity's id is the one the peer methods use: `sha256:` and the SHA-256 of its `SubjectPublicKeyInfo`.

```json
{"method": "...", "key": "<holder's SPKI>", "expires": 1700000300, "audience": "sha256:...", "proof": "..."}
```

What the binding proves is this statement (each part with a 4-byte length): `dev.boriken.coqui.acp/secret/v1 binding`, the method, the request id, the 32-byte HPKE public key, the `expires` as 8 bytes, and the audience's 32-byte id or nothing. A proof for one request cannot be moved to another, to another key, or to a collector it was not made for.

How an identity proves it follows from what the key can do:

| Peer method | Identity key | Binding method | Proof |
| --- | --- | --- | --- |
| `webcrypto` | ECDSA P-256 (its handshake already signs with it) | `ecdsa-p256-sha256` | IEEE P1363 signature (`r` then `s`, 64 bytes) over the statement; `key` is the 91-byte SPKI |
| `noise` | X25519 static key: it cannot sign | `x25519-hmac-sha256` | HMAC-SHA-256 of the statement under HKDF-SHA256 of the Diffie-Hellman of the holder's static key and the collector's, with the label `dev.boriken.coqui.acp/secret/v1 dh` and both public keys (holder first) as `info`; `audience` is REQUIRED and is the collector's id; `key` is the 44-byte SPKI |
| `tls` | the certificate's key (ECDSA P-256 by default) | not yet | needs the certificate in the message and the trust store's chain checks; a later version adds a `chain` to the binding |

The Diffie-Hellman proof is not transferable: only the collector can check it, and it can check it only for a holder it knows. That is enough for a person's own frontend and host. A receiver that a relay stands between and that the person's frontend has never paired with needs a chain to a root the collector trusts, which is what the `tls` row is waiting for.

A binding `expires` at a time the receiver sets, and a collector refuses one that ended or that lasts longer than its own limit (5 minutes from now by default). The collector checks it when the form opens; the person's time to fill the form is the form's own timeout.

## Refusals

A message that is not in the shapes above is `-32602` invalid params. A collector that refuses a key does not tell the agent more than that the person was not asked. Neither side puts a value, a key or a signature in an error message or a log.
