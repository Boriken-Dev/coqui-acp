// The package's own scenes, drawn with the test font (every glyph a square), so they show layout, colour and what folds, not words.
// Regenerate with `flutter test --update-goldens test/goldens_test.dart` and look at the pictures before committing them.
import 'dart:typed_data';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fixtures.dart';

const _plan = [
  PlanEntry('Read addNumbers in src/calc.js', PlanStatus.done),
  PlanEntry('Change subtraction to addition', PlanStatus.running),
  PlanEntry('Check the result', PlanStatus.pending),
];

Widget _scene(Widget child, {ChatLook? look, Brightness brightness = Brightness.dark}) => MaterialApp(
  debugShowCheckedModeBanner: false,
  theme: theme(look: look, brightness: brightness),
  home: Scaffold(body: Center(child: SizedBox(width: 900, height: 640, child: child))),
);

Future<void> _size(WidgetTester tester) async {
  tester.view.physicalSize = const Size(900, 640);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
}

void main() {
  for (final density in ChatDensity.values) {
    testWidgets('the transcript at ${density.label}', (tester) async {
      await _size(tester);
      await tester.pumpWidget(_scene(ChatTranscript(entries: worked(), density: density, plan: _plan)));
      await expectLater(find.byType(MaterialApp), matchesGoldenFile('goldens/transcript_${density.name}.png'));
    });
  }

  testWidgets('the Worked for line opened, and its burst opened', (tester) async {
    await _size(tester);
    await tester.pumpWidget(_scene(ChatTranscript(entries: worked())));
    await tester.tap(find.textContaining('Worked for'));
    await tester.pump();
    await tester.tap(find.descendant(of: find.byType(ToolBurst), matching: find.byType(FoldLine)));
    await tester.pump();
    await expectLater(find.byType(MaterialApp), matchesGoldenFile('goldens/transcript_opened.png'));
  });

  testWidgets('a failed call open among folded ones, light', (tester) async {
    await _size(tester);
    final failed = tool('f', kind: 'execute', title: 'npm test', status: 'failed', content: [
      {'type': 'content', 'content': {'type': 'text', 'text': 'FAIL calc.test.js\n  expected 5, got -1'}},
    ]);
    final entries = [user('Run the tests', 0), toolEntry(tool('a', kind: 'read', locations: [{'path': 'src/calc.js'}]), 1), toolEntry(failed, 2), agent('One test fails.', 3)];
    await tester.pumpWidget(_scene(ChatTranscript(entries: entries), brightness: Brightness.light));
    await expectLater(find.byType(MaterialApp), matchesGoldenFile('goldens/transcript_failed_light.png'));
  });

  testWidgets('a permission card and a question beside the queue', (tester) async {
    await _size(tester);
    await tester.pumpWidget(_scene(Column(mainAxisAlignment: MainAxisAlignment.end, children: [
      const QueuePanel(lines: [QueuedLine('a', 'and then run the tests', attachments: ['log.txt'])], onEdit: _no2, onRemove: _no1, onClear: _no0),
      PermissionCard(
        agent: 'Claude',
        title: 'Edit src/calc.js',
        detail: 'return a + b;',
        options: const [
          AcpPermissionOption(optionId: 'y', name: 'Yes', kind: 'allow_once'),
          AcpPermissionOption(optionId: 'n', name: 'No', kind: 'reject_once'),
        ],
        onChoose: (_) {},
      ),
      QuestionCard(
        request: const AcpElicitation(sessionId: 's', message: 'Which branch?', fields: [
          AcpChoiceField('branch', [AcpChoice('main', 'main'), AcpChoice('dev', 'dev')], title: 'Branch', required: true),
        ]),
        agent: 'Claude',
        onAnswer: _noAnswer,
      ),
    ])));
    await expectLater(find.byType(MaterialApp), matchesGoldenFile('goldens/cards.png'));
  });

  testWidgets('the composer with slots, chips and suggestions', (tester) async {
    await _size(tester);
    final controller = TextEditingController(text: '/co');
    addTearDown(controller.dispose);
    final attachments = _Chips();
    await tester.pumpWidget(_scene(Align(
      alignment: Alignment.bottomCenter,
      child: Composer(
        controller: controller,
        onSend: () {},
        commands: const [AgentCommandInfo('compact', 'Make room in the context'), AgentCommandInfo('cost', 'Show what this cost')],
        attachments: attachments,
        before: const [ComposerPill(label: 'Manual', icon: Icons.tune)],
        trailing: const [ComposerPill(label: 'Opus 5.5', icon: Icons.memory), ComposerPill(label: 'High', icon: Icons.speed)],
      ),
    )));
    await expectLater(find.byType(MaterialApp), matchesGoldenFile('goldens/composer.png'));
  });
}

void _no0() {}
void _no1(int _) {}
void _no2(int _, String _) {}
void _noAnswer(AcpElicitationAnswer _) {}

class _Chips implements ComposerAttachments {
  @override
  bool get canAttach => true;
  @override
  List<ComposerChip> get chips => const [
    ComposerChip(key: 'a', name: 'calc.js', detail: '1.2 KB'),
    ComposerChip(key: 'b', name: 'src', detail: 'Folder', kind: ChipKind.folder),
  ];
  @override
  Future<List<String>> addFiles(List<PickedFile> files) async => const [];
  @override
  Future<List<String>> addPaths(List<String> paths) async => const [];
  @override
  String? addImage(Uint8List bytes) => null;
  @override
  String? addText(String text) => null;
  @override
  String? addFolder(String path) => null;
  @override
  void remove(Object key) {}
}
