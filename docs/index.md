# coqui-acp

A session library for the [Agent Client Protocol](https://agentclientprotocol.com) in Rust and Dart, and a bridge that puts a stdio agent on the network.

!!! note "Alpha"
    Not published yet: the API may change in any release before 1.0.

## Versions

ACP version 1 is stable and upstream labels version 2 draft. The Rust library speaks version 1. The Dart client requests version 2 and accepts version 1, with one event
stream over both, since the two end a turn differently: version 1 by the stop reason of the `session/prompt` answer, version 2 by an idle `state_update`.

## Permissions

A client refuses every permission request until the application gives it a decider. The agent's own permissions stay the agent's: neither can override the other's refusal.

[The remote profile](profile.md) describes ACP across a network. [ACP in the browser](browser.md) is a draft contract for a web page to find and use a person's agents through any provider. [Agents as providers](provider.md) is the contract to write an agent to, in any language Guatiao hosts, and the facade that serves one over ACP.
