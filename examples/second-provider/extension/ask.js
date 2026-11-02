// The provider's own window. Everything from a page or an agent goes through textContent and is labelled as theirs.
import { loopbackUrl } from './provider.js';

const id = new URLSearchParams(location.search).get('id');
const content = document.getElementById('content');

function element(tag, properties = {}, ...children) {
  const node = Object.assign(document.createElement(tag), properties);
  node.append(...children);
  return node;
}

const send = (answer) => chrome.runtime.sendMessage({ op: 'answer', id, answer });

function checkbox(id_, text, checked = false) {
  const input = element('input', { type: 'checkbox', id: id_, checked });
  return { input, label: element('label', {}, input, ` ${text}`) };
}

function renderGrant({ origin, purpose, mcpOrigins, permissions, candidates }) {
  content.append(element('h1', { textContent: `${origin} asks for an agent` }));
  if (purpose !== undefined) content.append(element('p', { className: 'said', textContent: `The site says: ${purpose}` }));

  const list = element('fieldset', {}, element('legend', { textContent: 'Agents to give this site' }));
  const added = [];
  const addRow = (text, input) => list.append(element('label', {}, input, ` ${text}`));
  for (const candidate of candidates) {
    const key = element('input', { type: 'checkbox', className: 'candidate' });
    key.dataset.key = candidate.key;
    const fingerprint = candidate.peer ? `, key ${candidate.peer}, which the site will see` : ', no key';
    addRow(`${candidate.label} (${candidate.url}${fingerprint}; not proved)`, key);
  }
  content.append(list);

  const url = element('input', { type: 'text', id: 'add-url', placeholder: 'ws://127.0.0.1:8765/acp' });
  const label = element('input', { type: 'text', id: 'add-label', placeholder: 'what you call it' });
  const fingerprint = element('input', { type: 'text', id: 'add-fingerprint', placeholder: 'sha256:... (optional)' });
  const understood = checkbox('add-understood', 'I know nothing proves who answers at this address.');
  const error = element('p', { id: 'error' });
  const add = element('button', {
    type: 'button',
    id: 'add',
    textContent: 'Add',
    onclick: () => {
      if (loopbackUrl(url.value) === null) {
        error.textContent = 'This provider can only add a WebSocket on this machine, since it cannot prove anything else.';
        return;
      }
      if (!understood.input.checked) {
        error.textContent = 'Tick the box first: nothing here can prove who this is.';
        return;
      }
      error.textContent = '';
      const include = element('input', { type: 'checkbox', className: 'added', checked: true });
      added.push({ include, label: label.value, url: url.value, fingerprint: fingerprint.value });
      addRow(`${label.value || url.value} (added by you; not proved)`, include);
      url.value = '';
      label.value = '';
      fingerprint.value = '';
      understood.input.checked = false;
    },
  });
  content.append(
    element(
      'fieldset',
      {},
      element('legend', { textContent: 'Add one by hand' }),
      element('label', {}, 'Address ', url),
      element('label', {}, 'Name ', label),
      element('label', {}, 'Fingerprint ', fingerprint),
      understood.label,
      add,
      error,
    ),
  );

  const cwd = element('input', { type: 'text', id: 'cwd' });
  content.append(element('label', {}, 'Folder on the agent\'s machine it works in (without one, this site can start no session) ', cwd));

  const always = checkbox('always', 'Keep this until I revoke it (otherwise, until this tab closes)');
  content.append(always.label);

  const origins = mcpOrigins.map((origin) => {
    const option = checkbox(`mcp-${origin}`, `Let the agent connect to ${origin}, which the site asks for`);
    content.append(option.label);
    return { origin, input: option.input };
  });

  let mayApprove = null;
  if (permissions) {
    mayApprove = checkbox('permissions', 'this site can approve what the agent does for you');
    content.append(mayApprove.label);
  }

  content.append(
    element('button', {
      type: 'button',
      id: 'allow',
      textContent: 'Give access',
      onclick: () =>
        send({
          allow: [...document.querySelectorAll('input.candidate:checked')].map((input) => input.dataset.key),
          add: added.map((item) => ({
            label: item.label,
            url: item.url,
            fingerprint: item.fingerprint,
            understood: true,
            grant: item.include.checked,
          })),
          cwd: cwd.value,
          lifetime: always.input.checked ? 'always' : 'once',
          permissions: mayApprove?.input.checked === true,
          mcpOrigins: origins.filter(({ input }) => input.checked).map(({ origin }) => origin),
        }),
    }),
    ' ',
    element('button', { type: 'button', id: 'refuse', textContent: 'Refuse', onclick: () => send(null) }),
  );
}

function renderPermission({ origin, agent, title, options }) {
  content.append(
    element('h1', { textContent: `${agent} asks for your permission` }),
    element('p', { textContent: `On behalf of a conversation started by ${origin}. The site does not see this question.` }),
    element('p', { className: 'said', textContent: `The agent says: ${title}` }),
  );
  for (const option of options) {
    const button = element('button', { type: 'button', textContent: option.name, onclick: () => send(option.optionId) });
    button.dataset.option = option.optionId;
    content.append(button, ' ');
  }
  content.append(element('button', { type: 'button', id: 'refuse', textContent: 'Refuse', onclick: () => send(null) }));
}

const prompt = await chrome.runtime.sendMessage({ op: 'prompt', id });
if (prompt === null || prompt === undefined) {
  window.close();
} else if (prompt.kind === 'grant') {
  renderGrant(prompt.data);
} else {
  renderPermission(prompt.data);
}
document.body.dataset.ready = prompt?.kind ?? 'none';
