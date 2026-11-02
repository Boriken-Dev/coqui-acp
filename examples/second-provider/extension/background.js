// The service worker: the browser's half of Provider's arguments. Listeners are registered as this file loads, since a
// stopped worker is woken only for listeners it has when it starts.
import { Provider } from './provider.js';

const EXTENSION_URL = chrome.runtime.getURL('');
const prompts = new Map();
const permissionQueue = [];
let showingPermission = null;

function openWindow(prompt) {
  chrome.windows
    .create({ type: 'popup', focused: true, width: 480, height: 680, url: `ask.html?id=${prompt.id}` })
    .then((created) => {
      if (!prompts.has(prompt.id)) chrome.windows.remove(created.id).catch(() => {});
      else prompt.windowId = created.id;
    })
    .catch(() => settle(prompt.id, null));
}

function settle(id, answer) {
  const prompt = prompts.get(id);
  if (prompt === undefined) return;
  prompts.delete(id);
  if (prompt.windowId !== undefined) chrome.windows.remove(prompt.windowId).catch(() => {});
  if (showingPermission === prompt) {
    showingPermission = null;
    showNextPermission();
  }
  prompt.resolve(answer);
}

// One window at a time, so an agent asking repeatedly cannot bury the person in windows.
function showNextPermission() {
  if (showingPermission !== null) return;
  const next = permissionQueue.shift();
  if (next === undefined) return;
  showingPermission = next;
  openWindow(next);
}

function ask(kind, data, signal, show) {
  return new Promise((resolve) => {
    const prompt = { id: crypto.randomUUID(), kind, data, resolve };
    prompts.set(prompt.id, prompt);
    signal.addEventListener(
      'abort',
      () => {
        const queued = permissionQueue.indexOf(prompt);
        if (queued !== -1) permissionQueue.splice(queued, 1);
        settle(prompt.id, null);
      },
      { once: true },
    );
    show(prompt);
  });
}

const ui = {
  askGrant: (data, signal) => ask('grant', data, signal, openWindow),
  askPermission: (data, signal) =>
    ask('permission', data, signal, (prompt) => {
      permissionQueue.push(prompt);
      showNextPermission();
    }),
};

// Socket traffic alone does not keep a service worker alive through a quiet minute, and its death would end a connection
// the page still holds. Any extension API call resets the idle timer, and this one has no effect of its own.
let keepAlive = null;
function busyChanged(busy) {
  clearInterval(keepAlive);
  keepAlive = busy ? setInterval(() => chrome.runtime.getPlatformInfo(), 20_000) : null;
}

const provider = new Provider({ local: chrome.storage.local, session: chrome.storage.session, ui, busyChanged });

chrome.runtime.onConnect.addListener((port) => {
  if (port.name === 'acp-page') provider.attachPage(port);
});
chrome.tabs.onRemoved.addListener((tabId) => provider.tabClosed(tabId));
chrome.windows.onRemoved.addListener((windowId) => {
  for (const prompt of prompts.values()) {
    if (prompt.windowId === windowId) settle(prompt.id, null);
  }
});
chrome.action.onClicked.addListener(() => chrome.runtime.openOptionsPage());

const OPERATIONS = {
  prompt: ({ id }) => {
    const prompt = prompts.get(id);
    return prompt === undefined ? null : { kind: prompt.kind, data: prompt.data };
  },
  answer: ({ id, answer }) => settle(id, answer ?? null),
  state: () => provider.state(),
  add: ({ record }) => provider.personAdds(record),
  remove: ({ key }) => provider.personRemoves(key),
  revoke: ({ uid }) => provider.personRevokes(uid),
};

chrome.runtime.onMessage.addListener((message, sender, respond) => {
  // A content script's sender URL is its page's, so only this extension's own pages get past here.
  if (!sender.url?.startsWith(EXTENSION_URL) || !Object.hasOwn(OPERATIONS, message?.op)) return false;
  Promise.resolve(OPERATIONS[message.op](message)).then(
    (value) => respond(value ?? null),
    () => respond(null),
  );
  return true;
});
