// The agent's own choices, a model, a mode or an effort: set when the agent can take them, held while a turn runs and sent before
// the next prompt, and refused in the agent's words.
import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

Map<String, Object?> _option(
  String id,
  String category,
  String current,
  List<String> values,
) => {
  'id': id,
  'name': id,
  'category': category,
  'currentValue': current,
  'options': [
    for (final v in values) {'value': v, 'name': v.toUpperCase()},
  ],
};

ChatSession _offering(ChatSession session) {
  session.applyOpened(
    AcpOpenedSession('s1', {
      'configOptions': [
        _option('model', 'model', 'a', ['a', 'b']),
        _option('effort', 'thought_level', 'low', ['low', 'high']),
      ],
    }),
  );
  return session;
}

void main() {
  test(
    'a choice goes to the agent, which answers with the options it has now',
    () async {
      final (:store, :session, :agent) = started();
      _offering(session);
      agent.onSetConfig = (id, value) async => {
        'configOptions': [
          _option('model', 'model', value, ['a', 'b']),
          _option('effort', 'thought_level', 'high', ['low', 'high']),
        ],
      };
      final refused = await session.choose(
        optionIn(session.options, OptionRole.model)!,
        'b',
      );
      expect(refused, isFalse);
      expect(agent.calls, ['config:model=b']);
      expect(
        session.options.map((o) => o.current),
        ['b', 'high'],
        reason: 'the agent changed the effort with the model',
      );
      store.dispose();
    },
  );

  test(
    'an answer that does not say what the options are leaves the chosen value as the current one',
    () async {
      final (:store, :session, :agent) = started();
      _offering(session);
      await session.choose(
        optionIn(session.options, OptionRole.effort)!,
        'high',
      );
      expect(optionIn(session.options, OptionRole.effort)!.current, 'high');
      store.dispose();
    },
  );

  test('a mode goes by session/set_mode', () async {
    final (:store, :session, :agent) = started();
    session.applyOpened(
      AcpOpenedSession('s1', {
        'modes': {
          'currentModeId': 'manual',
          'availableModes': [
            {'id': 'manual', 'name': 'Manual'},
            {'id': 'auto', 'name': 'Auto'},
          ],
        },
      }),
    );
    await session.choose(modeOptionIn(session.options)!, 'auto');
    expect(agent.calls, ['mode:auto']);
    expect(session.mode, 'auto');
    store.dispose();
  });

  test(
    "a refusal is said with the agent's reason, and the option stays as it was",
    () async {
      final (:store, :session, :agent) = started();
      _offering(session);
      agent.onSetConfig = (_, _) async => throw const AcpFailure(
        'Agent error: {code: -32602, message: Unknown model}',
      );
      final refused = await session.choose(
        optionIn(session.options, OptionRole.model)!,
        'b',
      );
      expect(refused, isTrue);
      expect(optionIn(session.options, OptionRole.model)!.current, 'a');
      expect(
        session.messages.last.text,
        'The agent did not change model to B: Unknown model',
      );
      store.dispose();
    },
  );

  test(
    'made while a turn runs waits, and goes before the next prompt; taken back it is not sent',
    () async {
      final (:store, :session, :agent) = started();
      _offering(session);
      await session.send('go');
      feed(session, state('running'));
      final model = optionIn(session.options, OptionRole.model)!;
      final effort = optionIn(session.options, OptionRole.effort)!;
      expect(await session.choose(model, 'b'), isFalse);
      await session.choose(effort, 'high');
      await session.choose(effort, null);
      expect(session.pending.keys, ['model']);
      expect(agent.calls, [
        'prompt:go',
      ], reason: 'nothing goes while a turn runs');
      await session.send('next');
      feed(session, state('idle', reason: 'end_turn'));
      await settle();
      expect(agent.calls, ['prompt:go', 'config:model=b', 'prompt:next']);
      expect(session.pending, isEmpty);
      store.dispose();
    },
  );

  test(
    'a choice the agent no longer offers is said, not sent, and a mode of custom is never chosen',
    () async {
      final (:store, :session, :agent) = started();
      _offering(session);
      session.pending['model'] = const PendingChoice(
        'model',
        OptionRole.model,
        'zz',
      );
      session.pending['mode'] = const PendingChoice(
        'mode',
        OptionRole.mode,
        'custom',
      );
      await session.send('hello');
      expect(agent.calls, ['prompt:hello']);
      expect(
        session.messages.map((m) => m.text),
        contains('The agent offers no model of zz now, so it was not set.'),
      );
      store.dispose();
    },
  );

  test(
    'a pending choice goes to the option of its role when its own id is gone, and a mode is sent even when it is the one shown',
    () async {
      final (:store, :session, :agent) = started();
      _offering(session);
      session.pending['llm'] = const PendingChoice(
        'llm',
        OptionRole.model,
        'b',
      );
      await session.send('hello');
      expect(agent.calls, ['config:model=b', 'prompt:hello']);
      session.applyOpened(
        AcpOpenedSession('s1', {
          'modes': {
            'currentModeId': 'manual',
            'availableModes': [
              {'id': 'manual', 'name': 'Manual'},
            ],
          },
        }),
      );
      session.pending['mode'] = const PendingChoice(
        'mode',
        OptionRole.mode,
        'manual',
      );
      await session.send('again');
      expect(agent.calls.sublist(2), ['mode:manual', 'prompt:again']);
      store.dispose();
    },
  );

  test('roles come from the ACP category, else from the id', () {
    ConfigOption option(String id, {String? category}) => ConfigOption(
      id: id,
      label: id,
      current: 'x',
      choices: const [Choice('x', 'x')],
      category: category,
    );
    expect(
      roleOf(option('anything', category: 'thought_level')),
      OptionRole.effort,
    );
    expect(roleOf(option('anything', category: 'something')), OptionRole.other);
    expect(roleOf(option('reasoning_effort')), OptionRole.effort);
    expect(roleOf(option('model')), OptionRole.model);
    expect(roleOf(option('mode')), OptionRole.mode);
    expect(isBypassMode('bypassPermissions'), isTrue);
    expect(isBypassMode('custom', 'Bypass'), isTrue);
    expect(isBypassMode('manual'), isFalse);
    expect(
      bypassAnswer(const [rejectOnce, allowAlways, allowOnce])!.optionId,
      'allow-once',
    );
    expect(bypassAnswer(const [rejectOnce]), isNull);
  });

  test(
    'options that are not ACP\'s shape are skipped, and what is kept is bounded',
    () {
      const limits = ChatLimits(configOptions: 1, choices: 2);
      final parsed = parseConfigOptions([
        'not a map',
        {'id': 'a', 'currentValue': 'x', 'options': 'nope'},
        _option('m', 'model', 'x', ['x', 'y', 'z']),
        _option('n', 'model', 'x', ['x']),
      ], limits);
      expect(
        parsed!.map((o) => o.id),
        isEmpty,
        reason: 'the limit counts what is looked at, not what is kept',
      );
      final more = parseConfigOptions([
        _option('m', 'model', 'x', ['x', 'y', 'z']),
      ], limits)!;
      expect(more.single.choices.map((c) => c.value), ['x', 'y']);
      expect(parseConfigOptions('nope', limits), isNull);
    },
  );
}
