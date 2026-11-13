import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat_ui/coqui_acp_chat_ui.dart';
import 'package:flutter/material.dart';

const limits = ChatLimits();

final DateTime t0 = DateTime.utc(2026, 11, 1, 9);

/// A tool call as an agent's updates would build it.
ToolCall tool(
  String id, {
  String kind = 'other',
  String title = 'A tool',
  String status = 'completed',
  List<Map<String, Object?>> content = const [],
  List<Map<String, Object?>> locations = const [],
}) => ToolCall(id)
  ..apply({
    'sessionUpdate': 'tool_call',
    'toolCallId': id,
    'title': title,
    'kind': kind,
    'status': status,
    'content': content,
    'locations': locations,
  }, limits);

ChatEntry user(String text, int seconds) => ChatEntry(MessageRole.user, text, at: t0.add(Duration(seconds: seconds)));
ChatEntry agent(String text, int seconds) => ChatEntry(MessageRole.agent, text, at: t0.add(Duration(seconds: seconds)));
ChatEntry thought(String text, int seconds) => ChatEntry(MessageRole.thought, text, at: t0.add(Duration(seconds: seconds)));
ChatEntry toolEntry(ToolCall call, int seconds) => ChatEntry(MessageRole.tool, call.title, at: t0.add(Duration(seconds: seconds)), tool: call);

Map<String, Object?> diff(String path, String? before, String after) => {
  'type': 'diff',
  'path': path,
  'oldText': ?before,
  'newText': after,
};

/// The conversation of the Claude sample: an edit, a command, then the answer.
List<ChatEntry> worked() => [
  user('Fix addNumbers', 0),
  thought('It subtracts', 2),
  toolEntry(tool('t1', kind: 'edit', title: 'Edit calc.js', content: [diff('src/calc.js', 'a - b\n', 'a + b\n')], locations: [{'path': 'src/calc.js', 'line': 2}]), 5),
  toolEntry(tool('t2', kind: 'execute', title: 'node -e addNumbers(2,3)'), 40),
  agent('I fixed `addNumbers` in src/calc.js.', 89),
];

ThemeData theme({ChatLook? look, Brightness brightness = Brightness.dark}) {
  final base = ThemeData(useMaterial3: true, brightness: brightness, colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFFD9A066), brightness: brightness));
  return base.copyWith(extensions: [look ?? ChatLook.fromScheme(base.colorScheme)]);
}

Widget host(Widget child, {ChatLook? look, Size? size}) => MaterialApp(
  theme: theme(look: look),
  home: Scaffold(body: child),
);
