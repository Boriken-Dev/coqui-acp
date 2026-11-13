import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fixtures.dart';

AcpPermissionOption option(String id, String kind, [String? name]) => AcpPermissionOption(optionId: id, name: name ?? id, kind: kind);

AcpElicitation question(List<AcpField> fields, {String message = 'Which branch?'}) =>
    AcpElicitation(sessionId: 's1', message: message, fields: fields);

void main() {
  group('PermissionCard', () {
    final options = [
      option('allow', 'allow_once', 'Yes'),
      option('always', 'allow_always', 'Yes, and do not ask again'),
      option('no', 'reject_once', 'No'),
    ];

    testWidgets('lists the agent\'s answers refusals first, once before always, and answers with the one pressed', (tester) async {
      AcpPermissionOption? chosen;
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 'Edit calc.js', options: options, onChoose: (each) => chosen = each)));
      final names = [for (final each in tester.widgetList<PermissionChoice>(find.byType(PermissionChoice))) each.option.name];
      expect(names, ['No', 'Yes', 'Yes, and do not ask again']);
      expect(find.text('Claude wants to use a tool'), findsOneWidget);
      expect(find.text('Edit calc.js'), findsOneWidget);
      await tester.tap(find.widgetWithText(PermissionChoice, 'Yes'));
      expect(chosen?.optionId, 'allow');
    });

    testWidgets('names the host in what an always answer does, and a host may say it its own way', (tester) async {
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 't', options: options, onChoose: (_) {}, host: 'Notes')));
      expect(find.textContaining('remembered by Claude, not by Notes'), findsOneWidget);
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 't', options: options, onChoose: (_) {}, rememberedBy: 'Kept by the agent.')));
      expect(find.text('Kept by the agent.'), findsOneWidget);
    });

    testWidgets('slots: notes under the question and extra answers beside the agent\'s', (tester) async {
      var remembered = 0;
      await tester.pumpWidget(host(PermissionCard(
        agent: 'Claude',
        title: 't',
        options: options,
        onChoose: (_) {},
        notes: const [Text('Always allow here: a rule of the host')],
        extraAnswers: [FilledButton.tonal(key: const Key('always-allow-here'), onPressed: () => remembered++, child: const Text('Always allow here: work'))],
      )));
      expect(find.text('Always allow here: a rule of the host'), findsOneWidget);
      await tester.tap(find.byKey(const Key('always-allow-here')));
      expect(remembered, 1);
    });

    testWidgets('a request that offers nothing can be cancelled, and says what is waiting after it', (tester) async {
      var cancelled = false;
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 't', options: const [], onChoose: (_) {}, onCancel: () => cancelled = true, more: 2)));
      expect(find.text('2 more after this'), findsOneWidget);
      expect(find.textContaining('offered no answer'), findsOneWidget);
      await tester.tap(find.text('Cancel the request'));
      expect(cancelled, isTrue);
    });

    testWidgets('says why Bypass left it to the person', (tester) async {
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 't', options: options, onChoose: (_) {}, bypassPassed: true, forPerson: true)));
      expect(find.textContaining('leaves a question or a plan to you'), findsOneWidget);
    });

    testWidgets('a long detail is scrollable and says so', (tester) async {
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 't', options: options, onChoose: (_) {}, detail: List.generate(40, (i) => 'line $i').join('\n'))));
      expect(find.textContaining('This is long'), findsOneWidget);
    });

    testWidgets('the agent\'s own words cannot carry a control character into the card', (tester) async {
      await tester.pumpWidget(host(PermissionCard(agent: 'Cla\u202eude', title: 't', options: options, onChoose: (_) {})));
      expect(find.textContaining('\u202e'), findsNothing);
    });
  });

  testWidgets('BypassBanner offers the one way out', (tester) async {
    var left = false;
    await tester.pumpWidget(host(BypassBanner(onLeave: () => left = true)));
    await tester.tap(find.text('Leave Bypass'));
    expect(left, isTrue);
  });

  group('QuestionCard', () {
    testWidgets('builds a form from the schema, checks it, and answers', (tester) async {
      AcpElicitationAnswer? answer;
      final request = question([
        const AcpTextField('name', title: 'Name', required: true, minLength: 3),
        const AcpBooleanField('sure', title: 'Sure'),
      ]);
      await tester.pumpWidget(host(QuestionCard(request: request, agent: 'Claude', onAnswer: (each) => answer = each)));
      expect(find.text('Claude asks'), findsOneWidget);
      expect(find.text('Which branch?'), findsOneWidget);
      expect(tester.widget<FilledButton>(find.widgetWithText(FilledButton, 'Send answer')).onPressed, isNull);
      await tester.enterText(find.byKey(const ValueKey('question-name')), 'ab');
      await tester.pump();
      expect(find.text('Name needs at least 3 characters.'), findsOneWidget);
      await tester.enterText(find.byKey(const ValueKey('question-name')), 'main');
      await tester.pump();
      await tester.tap(find.widgetWithText(FilledButton, 'Send answer'));
      expect(answer, isA<AcpElicitAccept>());
      expect((answer as AcpElicitAccept).content, {'name': 'main'});
    });

    testWidgets('Decline and Cancel answer without a form', (tester) async {
      final answers = <AcpElicitationAnswer>[];
      await tester.pumpWidget(host(QuestionCard(request: question(const []), agent: 'Claude', onAnswer: answers.add)));
      await tester.tap(find.text('Decline'));
      await tester.tap(find.text('Cancel'));
      expect(answers.map((each) => each.runtimeType), [AcpElicitDecline, AcpElicitCancel]);
    });

    testWidgets('a new question starts a new form, and the same one keeps what was typed', (tester) async {
      final first = question([const AcpTextField('name', title: 'Name')]);
      Widget card(AcpElicitation request) => host(QuestionCard(request: request, agent: 'Claude', onAnswer: (_) {}));
      await tester.pumpWidget(card(first));
      await tester.enterText(find.byKey(const ValueKey('question-name')), 'typed');
      await tester.pumpWidget(card(first));
      expect(find.text('typed'), findsOneWidget);
      await tester.pumpWidget(card(question([const AcpTextField('name', title: 'Name')])));
      expect(find.text('typed'), findsNothing);
    });

    testWidgets('a choice list answers in the agent\'s order, and a kind it cannot show is said', (tester) async {
      AcpElicitationAnswer? answer;
      final request = question([
        const AcpChoicesField('pick', [AcpChoice('a', 'A'), AcpChoice('b', 'B')], title: 'Pick'),
        const AcpUnsupportedField('odd', 'tensor', title: 'Odd'),
      ]);
      await tester.pumpWidget(host(QuestionCard(request: request, agent: 'Claude', host: 'Notes', onAnswer: (each) => answer = each)));
      expect(find.textContaining('A question of a kind Notes cannot show (tensor)'), findsOneWidget);
      await tester.tap(find.byKey(const ValueKey('question-pick-b')));
      await tester.tap(find.byKey(const ValueKey('question-pick-a')));
      await tester.pump();
      await tester.tap(find.widgetWithText(FilledButton, 'Send answer'));
      expect((answer as AcpElicitAccept).content, {'pick': ['a', 'b']});
    });
  });

  group('QueuePanel', () {
    Widget panel({String? held, bool working = false, List<String>? log, VoidCallback? sendNext}) => host(QueuePanel(
      lines: const [QueuedLine('a', 'first'), QueuedLine('b', 'second', attachments: ['x.png'])],
      held: held,
      working: working,
      onEdit: (index, text) => log?.add('edit $index $text'),
      onRemove: (index) => log?.add('remove $index'),
      onClear: () => log?.add('clear'),
      onSendNext: sendNext,
    ));

    testWidgets('lists each message with what goes with it, and edits and removes by place', (tester) async {
      final log = <String>[];
      await tester.pumpWidget(panel(log: log));
      expect(find.text('2 queued: sent one at a time, as each turn ends'), findsOneWidget);
      expect(find.text('second (with x.png)'), findsOneWidget);
      await tester.tap(find.byTooltip('Remove queued message').last);
      await tester.tap(find.text('Clear queue'));
      await tester.tap(find.byTooltip('Edit queued message').first);
      await tester.pumpAndSettle();
      await tester.enterText(find.byKey(const Key('queued-text')), 'changed');
      await tester.tap(find.text('Save'));
      await tester.pumpAndSettle();
      expect(log, ['remove 1', 'clear', 'edit 0 changed']);
    });

    testWidgets('held, it says why and offers the next one once the agent is idle', (tester) async {
      var sent = false;
      await tester.pumpWidget(panel(held: 'a stopped turn', sendNext: () => sent = true));
      expect(find.text('Held, since a stopped turn: nothing more is sent until you say'), findsOneWidget);
      await tester.tap(find.text('Send next'));
      expect(sent, isTrue);
      await tester.pumpWidget(panel(held: 'a stopped turn', working: true, sendNext: () {}));
      expect(find.text('Send next'), findsNothing);
    });

    testWidgets('a host\'s own dialog shows the edit', (tester) async {
      var shown = 0;
      await tester.pumpWidget(host(QueuePanel(
        lines: const [QueuedLine('a', 'first')],
        onEdit: (_, _) {},
        onRemove: (_) {},
        onClear: () {},
        dialog: (context, builder) {
          shown++;
          return Future.value();
        },
      )));
      await tester.tap(find.byTooltip('Edit queued message'));
      expect(shown, 1);
    });
  });

  testWidgets('UsageFooter says what was used, and nothing when nothing was', (tester) async {
    await tester.pumpWidget(host(const UsageFooter(text: '32% of context')));
    expect(find.text('32% of context'), findsOneWidget);
    await tester.pumpWidget(host(const UsageFooter(text: null)));
    expect(find.byType(Text), findsNothing);
    expect(describeUsage(const SessionUsage(used: 320, size: 1000)), '32% of context');
    expect(describeUsage(const SessionUsage()), isNull);
  });
}
