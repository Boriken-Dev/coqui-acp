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
    const diff = ToolDiff(path: 'src/calc.js', oldText: 'return a - b;', newText: 'return a + b;');

    Future<void> openMenu(WidgetTester tester) async {
      await tester.tap(find.byKey(const Key('permission-more')));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
    }

    testWidgets('a chip says it needs the person, above a question made from the tool\'s title', (tester) async {
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 'Edit calc.js', options: options, onChoose: (_) {})));
      expect(find.descendant(of: find.byKey(const Key('needs-input')), matching: find.text('Needs your input')), findsOneWidget);
      final question = find.text('Allow Claude to edit calc.js?');
      expect(question, findsOneWidget);
      expect(tester.getTopLeft(find.byKey(const Key('needs-input'))).dy, lessThan(tester.getTopLeft(question).dy));
    });

    testWidgets('a title that is not a request is the tool\'s name', (tester) async {
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 'Bash', options: options, onChoose: (_) {})));
      expect(find.text('Allow Claude to use Bash?'), findsOneWidget);
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: '', options: options, onChoose: (_) {})));
      expect(find.text('Allow Claude to use a tool?'), findsOneWidget);
    });

    testWidgets('the agent\'s reason is the line under the question, and without one there is no line', (tester) async {
      await tester.pumpWidget(host(PermissionCard(
        agent: 'Claude',
        title: 'Edit calc.js',
        reason: 'Path is outside allowed working directories',
        options: options,
        onChoose: (_) {},
      )));
      final reason = find.text('Path is outside allowed working directories');
      expect(tester.getTopLeft(reason).dy, greaterThan(tester.getBottomLeft(find.text('Allow Claude to edit calc.js?')).dy - 1));
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 'Edit calc.js', options: options, onChoose: (_) {})));
      expect(reason, findsNothing);
    });

    testWidgets('an edit shows its diff inline, across the card\'s width', (tester) async {
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 'Edit calc.js', diff: diff, options: options, onChoose: (_) {})));
      final shown = find.byType(DiffView);
      expect(find.descendant(of: shown, matching: find.textContaining('return a + b;')), findsOneWidget);
      expect(find.descendant(of: shown, matching: find.textContaining('return a - b;')), findsOneWidget);
      expect(find.text('src/calc.js'), findsOneWidget);
      expect(tester.getSize(shown).width, greaterThan(tester.getSize(find.byKey(const Key('permission-card'))).width - 60));
    });

    testWidgets('a command shows inline, across the card\'s width, and a diff takes its place when there is one', (tester) async {
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 'Run npm test', detail: 'npm test -- --run', options: options, onChoose: (_) {})));
      final command = find.byKey(const Key('permission-detail'));
      expect(find.descendant(of: command, matching: find.text('npm test -- --run')), findsOneWidget);
      expect(tester.getSize(command).width, greaterThan(tester.getSize(find.byKey(const Key('permission-card'))).width - 40));
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 'Edit', detail: 'src/calc.js', diff: diff, options: options, onChoose: (_) {})));
      expect(find.byType(DiffView), findsOneWidget);
      expect(find.text('src/calc.js'), findsOneWidget, reason: 'the path above the diff, not the text as well');
    });

    testWidgets('Deny and a split primary: the agent\'s always, with the one-time answer in its menu, the one pressed answering', (tester) async {
      final chosen = <String>[];
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 'Edit calc.js', options: options, onChoose: (each) => chosen.add(each.optionId))));
      expect(find.byKey(const Key('permission-deny')), findsOneWidget);
      expect(find.descendant(of: find.byKey(const Key('permission-primary')), matching: find.text('Yes, and do not ask again')), findsOneWidget);
      expect(find.text('Yes'), findsNothing, reason: 'in the menu until that is open');
      await openMenu(tester);
      await tester.tap(find.text('Yes'));
      await tester.pump();
      await tester.tap(find.byKey(const Key('permission-primary')));
      await tester.tap(find.byKey(const Key('permission-deny')));
      expect(chosen, ['allow', 'always', 'no']);
    });

    testWidgets('the menu holds the one-time answer first, what the card does not know, then the refusals for good', (tester) async {
      await tester.pumpWidget(host(PermissionCard(
        agent: 'Claude',
        title: 'Edit calc.js',
        options: [
          option('never', 'reject_always', 'No, never'),
          option('v', '_vendor', 'Keep planning'),
          ...options,
        ],
        onChoose: (_) {},
      )));
      await openMenu(tester);
      final order = [for (final kind in ['allow_once', '_vendor', 'reject_always']) tester.getTopLeft(find.byKey(Key('answer-$kind'))).dy];
      expect(order, [...order]..sort());
      expect(find.text('Keep planning'), findsOneWidget);
    });

    testWidgets('with no always on offer the primary allows once, and there is no menu or sentence about always', (tester) async {
      AcpPermissionOption? chosen;
      await tester.pumpWidget(host(PermissionCard(
        agent: 'Claude',
        title: 'Edit calc.js',
        options: [options[0], options[2]],
        onChoose: (each) => chosen = each,
      )));
      expect(find.byKey(const Key('permission-more')), findsNothing);
      expect(find.textContaining('An "always" answer'), findsNothing);
      await tester.tap(find.descendant(of: find.byKey(const Key('permission-primary')), matching: find.text('Yes')));
      expect(chosen?.optionId, 'allow');
    });

    testWidgets('an option with no name is named by its kind, and one of a kind the card does not know is a button of its own', (tester) async {
      await tester.pumpWidget(host(PermissionCard(
        agent: 'Claude',
        title: 'Edit calc.js',
        options: const [
          AcpPermissionOption(optionId: 'a', name: '', kind: 'allow_once'),
          AcpPermissionOption(optionId: 'r', name: '', kind: 'reject_once'),
        ],
        onChoose: (_) {},
      )));
      expect(find.descendant(of: find.byKey(const Key('permission-primary')), matching: find.text('Allow once')), findsOneWidget);
      expect(find.descendant(of: find.byKey(const Key('permission-deny')), matching: find.text('Deny')), findsOneWidget);
      await tester.pumpWidget(host(PermissionCard(
        agent: 'Claude',
        title: 'Edit calc.js',
        options: [option('v', '_vendor', 'Keep planning'), options[2]],
        onChoose: (_) {},
      )));
      expect(find.byKey(const Key('permission-primary')), findsNothing);
      expect(find.widgetWithText(OutlinedButton, 'Keep planning'), findsOneWidget);
    });

    testWidgets('the key hints a host gives show on Deny and in the menu, on a card wide enough for a keyboard', (tester) async {
      const hints = {'reject_once': 'Ctrl+Shift+R', 'allow_once': 'Ctrl+Shift+A'};
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 'Edit calc.js', options: options, onChoose: (_) {}, answerHints: hints)));
      expect(find.descendant(of: find.byKey(const Key('permission-deny')), matching: find.text('Ctrl+Shift+R')), findsOneWidget);
      await openMenu(tester);
      expect(find.descendant(of: find.byKey(const Key('answer-allow_once')), matching: find.text('Ctrl+Shift+A')), findsOneWidget);
      await tester.tapAt(const Offset(2, 2));
      await tester.pump();
      tester.view.physicalSize = const Size(390, 800);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      await tester.pumpAndSettle();
      expect(find.text('Ctrl+Shift+R'), findsNothing, reason: 'a phone has no such keys');
      await tester.pumpWidget(host(PermissionCard(agent: 'Claude', title: 'Edit calc.js', options: options, onChoose: (_) {})));
      expect(find.textContaining('Ctrl'), findsNothing, reason: 'none given, none said');
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
