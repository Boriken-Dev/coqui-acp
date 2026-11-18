import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart' show MarkdownElementBuilder;
import 'package:flutter_test/flutter_test.dart';
import 'package:markdown/markdown.dart' as md;

import 'fixtures.dart';

// What an app that keeps its own conversation needs of the widgets: a column it sets, entries that take the whole width, a
// line round a bubble, a `/` list the keyboard moves, a composer with no queue, and Markdown it adds meaning to.

class _Wide implements ChatEntryData {
  const _Wide(this.text, {required this.at});
  @override
  final String text;
  @override
  final DateTime at;
  @override
  MessageRole get role => MessageRole.notice;
  @override
  ToolCall? get tool => null;
  @override
  Compaction? get compaction => null;
}

Widget _transcript(List<ChatEntryData> entries, {ChatLook? look, ChatRenderers renderers = ChatRenderers.none}) => host(
  SizedBox(width: 900, height: 700, child: ChatTranscript(entries: entries, renderers: renderers)),
  look: look,
);

ChatLook _look({bool centred = false, Color? border, double width = 400}) {
  final base = ChatLook.fromScheme(theme().colorScheme);
  return base.copyWith(messageWidth: width, centred: centred, bubbleBorder: border, agentInBubble: true);
}

void main() {
  group('the column', () {
    testWidgets('an entry that is wide takes the whole width, the others keep to the look\'s message width', (tester) async {
      final entries = [user('Hello', 0), _Wide('a wide table', at: t0.add(const Duration(seconds: 1)))];
      await tester.pumpWidget(
        _transcript(
          entries,
          look: _look(),
          renderers: ChatRenderers(
            entry: [(context, entry) => entry is _Wide ? Container(key: const Key('table'), height: 20, color: Colors.red) : null],
            wide: (entry) => entry is _Wide,
          ),
        ),
      );
      expect(tester.getSize(find.byKey(const Key('table'))).width, greaterThan(700));
      final bubble = find.ancestor(of: find.text('Hello'), matching: find.byType(Container)).first;
      expect(tester.getSize(bubble).width, lessThan(400));
    });

    testWidgets('a centred look puts the column in the middle, and an agent\'s words at its left edge', (tester) async {
      await tester.pumpWidget(_transcript([agent('Hello there', 0)], look: _look(centred: true, width: 400)));
      final box = tester.getRect(find.byType(MessageBox).first);
      expect(box.left, greaterThan(150), reason: 'a 400 px column in the 720 px between the paddings starts near 200, not at 40');
      expect(box.left, lessThan(250));
    });

    testWidgets('without it the column is at the left', (tester) async {
      await tester.pumpWidget(_transcript([agent('Hello there', 0)], look: _look(width: 400)));
      expect(tester.getRect(find.byType(MessageBox).first).left, lessThan(60));
    });

    testWidgets('a bubble has a line round it when the look has one', (tester) async {
      await tester.pumpWidget(_transcript([agent('Hello there', 0)], look: _look(border: const Color(0xFF00FF00))));
      final decorated = tester.widgetList<Container>(find.descendant(of: find.byType(MessageBox), matching: find.byType(Container)));
      final lines = [for (final c in decorated) if (c.decoration case final BoxDecoration d when d.border != null) d.border];
      expect(lines, isNotEmpty);
    });
  });

  group('Markdown', () {
    testWidgets('a link goes to the host when it says what links do, and is copied otherwise', (tester) async {
      final followed = <(String, String)>[];
      await tester.pumpWidget(host(AgentMarkdown(text: '[open it](app:/missions)', onTapLink: (href, label) => followed.add((href, label)))));
      await tester.tap(find.text('open it'), warnIfMissed: false);
      await tester.pump();
      expect(followed, [('app:/missions', 'open it')]);
    });

    testWidgets('a host\'s syntax and builder give its own words a meaning', (tester) async {
      await tester.pumpWidget(
        host(AgentMarkdown(text: 'see M-12 now', inlineSyntaxes: [_Reference()], builders: {'ref': _RefBuilder()})),
      );
      expect(find.byKey(const Key('ref M-12')), findsOneWidget);
    });

    testWidgets('soft line breaks stay breaks when asked for, and join otherwise', (tester) async {
      String shown() => tester.widgetList<RichText>(find.descendant(of: find.byType(AgentMarkdown), matching: find.byType(RichText))).map((w) => w.text.toPlainText()).join('|');
      await tester.pumpWidget(host(const AgentMarkdown(text: 'one\ntwo')));
      expect(shown(), 'one two');
      await tester.pumpWidget(host(const AgentMarkdown(key: Key('kept'), text: 'one\ntwo', softLineBreak: true)));
      expect(shown(), 'one\ntwo');
    });
  });

  group('the / list', () {
    Future<(TextEditingController, List<String>)> pumpComposer(
      WidgetTester tester, {
      List<bool>? shown,
      List<AgentCommandInfo>? commands,
      bool working = false,
      bool canQueue = true,
      List<String>? stops,
      List<ComposerAction> actions = const [],
    }) async {
      final controller = TextEditingController();
      addTearDown(controller.dispose);
      final sent = <String>[];
      await tester.pumpWidget(
        host(
          Align(
            alignment: Alignment.bottomCenter,
            child: Composer(
              controller: controller,
              onSend: () => sent.add(controller.text),
              working: working,
              canQueue: canQueue,
              onStop: () => stops?.add('stop'),
              onSuggestions: shown?.add,
              actions: actions,
              commands:
                  commands ??
                  const [
                    AgentCommandInfo('brief', 'Brief me on the board', hint: '[period]'),
                    AgentCommandInfo('compact', 'Make room'),
                    AgentCommandInfo('review', 'Look at the diff and summarise'),
                  ],
            ),
          ),
        ),
      );
      return (controller, sent);
    }

    testWidgets('the names that start with the word come before those that only hold it, in a name or a description', (tester) async {
      final (controller, _) = await pumpComposer(tester);
      await tester.enterText(find.byType(TextField), '/sum');
      await tester.pump();
      expect(find.text('/review'), findsOneWidget, reason: 'its description holds the word');
      expect(find.text('/brief [period]'), findsNothing);
      controller.text = '/';
      await tester.pump();
      final order = [for (final tile in tester.widgetList<ListTile>(find.byType(ListTile))) (tile.key! as ValueKey<String>).value];
      expect(order, ['command brief', 'command compact', 'command review']);
    });

    testWidgets('a name that starts with the word is listed before one whose description holds it', (tester) async {
      await pumpComposer(
        tester,
        commands: const [AgentCommandInfo('alpha', 'Works with beta'), AgentCommandInfo('beta', 'The second')],
      );
      await tester.enterText(find.byType(TextField), '/beta');
      await tester.pump();
      final order = [for (final tile in tester.widgetList<ListTile>(find.byType(ListTile))) (tile.key! as ValueKey<String>).value];
      expect(order, ['command beta', 'command alpha']);
    });

    testWidgets('a hint follows the name', (tester) async {
      await pumpComposer(tester);
      await tester.enterText(find.byType(TextField), '/br');
      await tester.pump();
      expect(find.text('/brief [period]'), findsOneWidget);
    });

    testWidgets('the arrow keys move, Enter picks without sending, and Tab picks too', (tester) async {
      final shown = <bool>[];
      final (controller, sent) = await pumpComposer(tester, shown: shown);
      await tester.tap(find.byType(TextField));
      await tester.enterText(find.byType(TextField), '/');
      await tester.pump();
      expect(shown.last, isTrue);
      await tester.sendKeyEvent(LogicalKeyboardKey.arrowDown);
      await tester.sendKeyEvent(LogicalKeyboardKey.arrowDown);
      await tester.sendKeyEvent(LogicalKeyboardKey.arrowUp);
      await tester.sendKeyEvent(LogicalKeyboardKey.enter);
      await tester.pump();
      expect(controller.text, '/compact ');
      expect(sent, isEmpty);
      expect(shown.last, isFalse);
      controller.text = '/r';
      await tester.pump();
      await tester.sendKeyEvent(LogicalKeyboardKey.tab);
      await tester.pump();
      expect(controller.text, '/review ');
    });

    testWidgets('Escape closes the list and typing on opens it again', (tester) async {
      final shown = <bool>[];
      final (controller, _) = await pumpComposer(tester, shown: shown);
      await tester.tap(find.byType(TextField));
      await tester.enterText(find.byType(TextField), '/c');
      await tester.pump();
      expect(find.byType(ListTile), findsWidgets);
      await tester.sendKeyEvent(LogicalKeyboardKey.escape);
      await tester.pump();
      expect(find.byType(ListTile), findsNothing);
      expect(shown.last, isFalse);
      controller.text = '/co';
      await tester.pump();
      expect(find.byType(ListTile), findsWidgets);
      expect(shown.last, isTrue);
    });

    testWidgets('a long list holds what the limit says and scrolls', (tester) async {
      final many = [for (var i = 0; i < 30; i++) AgentCommandInfo('skill$i', 'Does thing $i')];
      final controller = TextEditingController();
      addTearDown(controller.dispose);
      await tester.pumpWidget(host(Align(alignment: Alignment.bottomCenter, child: Composer(controller: controller, onSend: () {}, commands: many, suggestionLimit: 20))));
      await tester.enterText(find.byType(TextField), '/');
      await tester.pump();
      expect(find.byType(SingleChildScrollView), findsWidgets);
      expect(find.byKey(const ValueKey('command skill0')), findsOneWidget);
    });

    testWidgets('an action is picked with Enter too, and runs without sending', (tester) async {
      var ran = 0;
      final (controller, sent) = await pumpComposer(tester, commands: const [], actions: [ComposerAction(name: 'clear', description: 'Start over', run: () => ran++)]);
      await tester.tap(find.byType(TextField));
      await tester.enterText(find.byType(TextField), '/cl');
      await tester.pump();
      await tester.sendKeyEvent(LogicalKeyboardKey.enter);
      await tester.pump();
      expect(ran, 1);
      expect(controller.text, isEmpty);
      expect(sent, isEmpty);
    });
  });

  group('no queue', () {
    testWidgets('a turn running shows Stop alone, and Enter sends nothing', (tester) async {
      final stops = <String>[];
      final (controller, sent) = await _compose(tester, stops: stops);
      expect(find.byKey(const Key('composer-queue')), findsNothing);
      expect(find.byTooltip('Send'), findsNothing);
      await tester.tap(find.byTooltip('Stop'));
      expect(stops, ['stop']);
      await tester.tap(find.byType(TextField));
      await tester.enterText(find.byType(TextField), 'next');
      await tester.testTextInput.receiveAction(TextInputAction.send);
      expect(sent, isEmpty);
      expect(controller.text, 'next');
    });
  });
}

Future<(TextEditingController, List<String>)> _compose(WidgetTester tester, {required List<String> stops}) async {
  final controller = TextEditingController();
  addTearDown(controller.dispose);
  final sent = <String>[];
  await tester.pumpWidget(
    host(
      Align(
        alignment: Alignment.bottomCenter,
        child: Composer(controller: controller, onSend: () => sent.add(controller.text), working: true, canQueue: false, onStop: () => stops.add('stop')),
      ),
    ),
  );
  return (controller, sent);
}

class _Reference extends md.InlineSyntax {
  _Reference() : super(r'M-\d+');
  @override
  bool onMatch(md.InlineParser parser, Match match) {
    parser.addNode(md.Element.text('ref', match[0]!));
    return true;
  }
}

class _RefBuilder extends MarkdownElementBuilder {
  @override
  Widget? visitElementAfterWithContext(BuildContext context, md.Element element, TextStyle? preferredStyle, TextStyle? parentStyle) =>
      Text(element.textContent, key: Key('ref ${element.textContent}'));
}
