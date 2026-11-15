// Child agents (subagents) built from the updates an agent marks with the child key. The updates are the wire shape
// `coqui-acp-harness` writes for a child report (a `tool_call_update` of the parent's call carrying only the marker) and
// for a child's own update (the same update with the marker added).
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

ChatSession fresh({ChatLimits limits = const ChatLimits()}) =>
    ChatSession(id: 'local-1', sessionId: 's1', limits: limits);

Map<String, Object?> parentCall(String id, String title) => {
  'sessionUpdate': 'tool_call_update',
  'toolCallId': id,
  'name': 'Agent',
  'title': title,
  'kind': 'think',
  'status': 'in_progress',
};

Map<String, Object?> report(String id, Map<String, Object?> fields) => {
  'sessionUpdate': 'tool_call_update',
  'toolCallId': id,
  '_meta': {
    childMetaKey: {'id': id, 'parentToolCallId': id, ...fields},
  },
};

Map<String, Object?> inside(String id, Map<String, Object?> update) => {
  ...update,
  '_meta': {
    childMetaKey: {'id': id, 'parentToolCallId': id},
  },
};

List<Map<String, Object?>> twoConcurrent() => [
  parentCall('call-a', 'Alpha count'),
  report('call-a', {
    'title': 'Alpha count',
    'task': 'say alpha',
    'state': 'starting',
  }),
  parentCall('call-b', 'Beta count'),
  report('call-b', {
    'title': 'Beta count',
    'task': 'say beta',
    'state': 'starting',
  }),
  report('call-a', {'state': 'working'}),
  report('call-b', {'state': 'working'}),
  report('call-b', {
    'state': 'completed',
    'result': 'beta',
    'usage': {'totalTokens': 18260, 'toolUses': 0, 'durationMs': 1066},
  }),
];

void main() {
  test(
    'two concurrent children are two named children, each with its own state',
    () {
      final session = fresh();
      twoConcurrent().forEach((update) => feed(session, update));
      expect(session.children.map((c) => (c.id, c.title, c.state)), [
        ('call-a', 'Alpha count', ChildState.working),
        ('call-b', 'Beta count', ChildState.completed),
      ]);
      final beta = session.child('call-b')!;
      expect(beta.result, 'beta');
      expect(beta.task, 'say beta');
      expect(beta.usage!.totalTokens, 18260);
      expect(beta.parentToolCallId, 'call-b');
      // The reports are not the parent's tool lines: each call shows once.
      expect(session.messages.where((m) => m.tool != null), hasLength(2));
    },
  );

  test(
    'a call and a result only shows its task and result, and says no progress was reported',
    () {
      final session = fresh();
      feed(session, parentCall('call-a', 'Alpha count'));
      feed(
        session,
        report('call-a', {'task': 'say alpha', 'state': 'starting'}),
      );
      feed(
        session,
        report('call-a', {'state': 'completed', 'result': 'alpha'}),
      );
      final child = session.child('call-a')!;
      expect(child.reportsProgress, isFalse);
      expect(
        (child.task, child.result, child.state),
        ('say alpha', 'alpha', ChildState.completed),
      );
    },
  );

  test(
    'a child that reports its progress keeps its own updates apart from the conversation',
    () {
      final session = fresh();
      feed(session, parentCall('call-a', 'Alpha count'));
      feed(session, report('call-a', {'state': 'working'}));
      feed(
        session,
        inside('call-a', text('agent_message_chunk', 'looking', id: 'm1')),
      );
      feed(
        session,
        inside('call-a', {
          'sessionUpdate': 'tool_call_update',
          'toolCallId': 'inner',
          'name': 'Glob',
        }),
      );
      final child = session.child('call-a')!;
      expect(child.reportsProgress, isTrue);
      expect(child.updates, hasLength(2));
      expect(session.messages.map((m) => m.role), [MessageRole.tool]);
      expect(session.messages.single.tool!.id, 'call-a');
    },
  );

  test(
    'replaying the same updates rebuilds the same children without a second copy',
    () {
      final updates = twoConcurrent();
      final live = fresh();
      for (final update in updates) {
        feed(live, update);
      }
      final replayed = fresh();
      for (final update in updates) {
        feed(replayed, update);
      }
      for (final update in updates) {
        feed(replayed, update);
      }
      expect(
        replayed.children.map((c) => (c.id, c.state, c.result)).toList(),
        live.children.map((c) => (c.id, c.state, c.result)).toList(),
      );
    },
  );

  test('the children and their updates are bounded', () {
    final session = fresh(
      limits: const ChatLimits(children: 2, childUpdates: 1),
    );
    for (final id in ['a', 'b', 'c']) {
      feed(session, report(id, {'state': 'completed'}));
    }
    expect(session.children.map((c) => c.id), ['b', 'c']);
    feed(session, inside('c', text('agent_message_chunk', 'one', id: 'm1')));
    feed(session, inside('c', text('agent_message_chunk', 'two', id: 'm2')));
    expect(session.child('c')!.updates, hasLength(1));
  });

  test(
    'an update with no marker, or a marker with no id, is the parent\'s',
    () {
      final session = fresh();
      feed(session, {
        ...parentCall('call-a', 'x'),
        '_meta': {
          childMetaKey: {'title': 'no id'},
        },
      });
      expect(session.children, isEmpty);
      expect(session.messages.where((m) => m.tool != null), hasLength(1));
    },
  );
}
