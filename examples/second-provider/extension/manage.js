// Adding, removing and revoking, outside any page's request.
const byId = (id) => document.getElementById(id);
const call = (message) => chrome.runtime.sendMessage(message);

function row(text, buttonText, dataName, value, onclick) {
  const button = Object.assign(document.createElement('button'), { type: 'button', textContent: buttonText, onclick });
  button.dataset[dataName] = value;
  const item = document.createElement('li');
  item.append(`${text} `, button);
  return item;
}

async function refresh() {
  const { agents, grants } = await call({ op: 'state' });
  const name = (key) => agents.find((agent) => agent.key === key)?.label ?? key;
  byId('agents').replaceChildren(
    ...agents.map((agent) =>
      row(`${agent.label} (${agent.url}${agent.peer ? `, key ${agent.peer}` : ''}; not proved)`, 'Remove', 'remove', agent.key, async () => {
        await call({ op: 'remove', key: agent.key });
        await refresh();
      }),
    ),
  );
  byId('grants').replaceChildren(
    ...grants.map((grant) =>
      row(
        `${grant.origin} may use ${name(grant.agentKey)}, ${grant.lifetime === 'always' ? 'until revoked' : 'until its tab closes'}${
          grant.permissions ? ', and can approve what the agent does for you' : ''
        }`,
        'Revoke',
        'revoke',
        grant.uid,
        async () => {
          await call({ op: 'revoke', uid: grant.uid });
          await refresh();
        },
      ),
    ),
  );
  document.body.dataset.ready = 'yes';
}

byId('add').onclick = async () => {
  const added = await call({
    op: 'add',
    record: {
      url: byId('add-url').value,
      label: byId('add-label').value,
      fingerprint: byId('add-fingerprint').value,
      understood: byId('add-understood').checked,
    },
  });
  byId('error').textContent = added ? '' : 'Only a WebSocket on this machine, with the box ticked.';
  await refresh();
};

await refresh();
