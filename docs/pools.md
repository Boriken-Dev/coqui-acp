# Pools: which agent, account and model serves

A pool is an ordered list of members, each an agent with an account, a model and whatever else a host starts a session by. The
`coqui-acp-pool` crate decides which member takes a new session or task, and moves a running session on when its usage runs out. It
is pure policy: no I/O, no files, no secrets, no clock, and no names of harnesses, accounts or standards. A host builds the pools
from its own configuration and tells the library what it knows through a few traits.

## What a host supplies

| Seam | Answers |
| --- | --- |
| `Availability` | whether a member can serve at all: installed, signed in, model listed, offered to this connection, able to take a running session's history |
| `UsageSource` | the member's usage buckets (`Known`), none (`Unknown`), or a report still to come (`Pending`); never waited for |
| `Occupancy` | the runs on a member, and when it last took a turn |
| `Holds` | what else holds a member back, such as a spending limit, scoped however the host chooses |
| `Starter` | start a session on a member; move a running one (export, clean, import); send the continuation |
| `Ledger` | where the record of each served turn goes |

## Choosing

A member passes over, with a reason each, when it is unavailable, at its cap of runs (`max_concurrent`), refused by a limit that has not
lifted, over a usage bucket that covers the model (exhausted, or at its `stop_permille`, with the window still open) or held. Usage
that is not reported counts as available and is marked so. Among the members that can serve, the strategy orders them:

- `priority` (the default): the highest priority, equal ones in the listed order.
- `headroom`: the least used, use not reported counting as 500 permille (`unknown_permille`), then fewest runs, least recently used and
  priority.
- `round_robin`: the member after the last one that took a turn, kept while the session's prompt cache is warm.

A running session stays on its member while it can serve, even if an earlier member has refilled; a later new session takes the
earlier one. Every bound is a setting with a default and no floor, and a setting that is not its default can be listed.

## Moving on

When a turn ends, the host calls the switchboard with what ended it: a limit, a clean finish, or something else (which moves nothing).
A session is never moved in the middle of a turn. When its member cannot serve, the pool decides again with that member out: the
members of the same group (the harness) first, in the strategy's order, then the others. The host's `carry` exports the session,
cleans it and imports it on the new member. A member whose history cannot be carried is left out for this session.

How a turn's end is read is the harness's to say. A harness provider that knows how its harness reports a limit states it in the standard
form, the `LimitHit` in the refused turn's `_meta`: the kind (usage quota, rate limit, context window, credit, other), when it lifts,
whether the work can go on with another member, and which of the turn's messages are the limit's own words. The pool uses it when it is
given: a limit that cannot be continued elsewhere (a context window) moves nothing, the cleaner drops the named messages, and phrase
matching is only the fallback for a harness that reports nothing.

The cleaner works on the exported history in the standard format and only on the last turn, the one the limit ended. It drops the
closing error or refusal, the text that names the limit and what follows it (the phrases are the host's), tool calls left open, and every
line the host marked as its own notice. Then the continuation goes to the new member: the role's text, else "The previous agent's usage
ran out; continue this session where it stopped.", or the person's own prompt again when the interrupted turn did no work.

## What is recorded

Each start and each move reaches the ledger as a `TurnRecord` from `coqui-acp-history`: harness, agent, account, model, effort, role,
pool and member, profile, how the member was chosen, tokens (input, output, cached), cost, duration and outcome, and an open map of tags
named `namespace:name` by whoever set them (`acp:` for `_meta` the agent returned, or a host's own), so usage can be summed by model,
role or effort later. Storing and querying the records is the host's.
