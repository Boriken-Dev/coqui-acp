// A real JavaScript peer for the bridge's test: coqui-peer's WebCrypto initiator over Node's WebSocket, then one ACP turn.
// A browser key cannot leave its process, so this prints its peer id first, and reads "<port> <expected peer>" from stdin
// once the test has started a bridge that allows it, or one that does not.
import {pathToFileURL} from 'node:url';
import readline from 'node:readline';

const {generateIdentity, createInitiator} = await import(pathToFileURL(process.argv[2]).href);
const identity = await generateIdentity();
console.log(identity.peerId);

const lines = readline.createInterface({input: process.stdin});
const [port, expected] = (await lines[Symbol.asyncIterator]().next()).value.trim().split(' ');
lines.close();

const socket = new WebSocket(`ws://127.0.0.1:${port}/`);
socket.binaryType = 'arraybuffer';
const inbox = [];
let waiter = null;
socket.onmessage = (event) => {
  if (waiter) {
    const take = waiter;
    waiter = null;
    take(event.data);
  } else {
    inbox.push(event.data);
  }
};
const fail = (why) => {
  console.error(why);
  process.exit(3);
};
socket.onclose = () => fail('the bridge closed the connection');
socket.onerror = () => fail('the connection failed');
const next = () => new Promise((resolve) => {
  if (inbox.length) resolve(inbox.shift());
  else waiter = resolve;
});
await new Promise((resolve) => socket.addEventListener('open', resolve));

const text = (data) => (typeof data === 'string' ? data : new TextDecoder().decode(data));
const proof = await createInitiator(identity, expected);
socket.send(proof.hello());
socket.send(await proof.answer(text(await next())));
const {session} = await proof.finish(text(await next()));

// Every message from here on is one sealed record.
const send = async (message) => socket.send(await session.seal(JSON.stringify(message)));
const receive = async () => JSON.parse(text(await session.open(new Uint8Array(await next()))));
const call = (id, method, params) => send({jsonrpc: '2.0', id, method, params});
const reply = async (id) => {
  for (;;) {
    const message = await receive();
    if (message.method === 'session/update') console.log('echoed:', message.params.update.content.text);
    if (message.id === id) return message.result;
  }
};
await call(1, 'initialize', {protocolVersion: 1, clientCapabilities: {}, clientInfo: {name: 'test', version: '0'}});
await reply(1);
await call(2, 'session/new', {cwd: '/w', mcpServers: []});
const {sessionId} = await reply(2);
await call(3, 'session/prompt', {sessionId, prompt: [{type: 'text', text: 'hello through the bridge'}]});
await reply(3);
socket.onclose = null;
socket.close();
process.exit(0);
