import 'package:coqui_acp_chat/coqui_acp_chat.dart';

/// One entry of a conversation as the widgets read it. A [ChatMessage] of `coqui_acp_chat` is one through [ModelEntry]; an app
/// that keeps its own entries implements this.
abstract interface class ChatEntryData {
  MessageRole get role;
  String get text;
  DateTime get at;

  /// The tool call, for [MessageRole.tool].
  ToolCall? get tool;

  /// The compaction, for [MessageRole.compaction].
  Compaction? get compaction;
}

/// A plain [ChatEntryData].
final class ChatEntry implements ChatEntryData {
  const ChatEntry(this.role, this.text, {required this.at, this.tool, this.compaction});

  @override
  final MessageRole role;
  @override
  final String text;
  @override
  final DateTime at;
  @override
  final ToolCall? tool;
  @override
  final Compaction? compaction;
}

/// A [ChatMessage] of the model, read as an entry.
final class ModelEntry implements ChatEntryData {
  const ModelEntry(this.message);

  final ChatMessage message;

  @override
  MessageRole get role => message.role;
  @override
  String get text => message.text;
  @override
  DateTime get at => message.at;
  @override
  ToolCall? get tool => message.tool;
  @override
  Compaction? get compaction => message.compaction;
}

/// What a transcript lays out: an entry as it is, a burst of finished tool calls folded into one line, or the work of a finished
/// turn folded into one.
sealed class TranscriptItem {
  const TranscriptItem();
}

/// One entry, drawn by what it is.
final class EntryItem extends TranscriptItem {
  const EntryItem(this.entry);
  final ChatEntryData entry;
}

/// Tool calls that finished and went well, one after another, drawn as a single line that opens into their cards.
final class BurstItem extends TranscriptItem {
  const BurstItem(this.calls);

  /// At least one entry, each with a [ChatEntryData.tool].
  final List<ChatEntryData> calls;

  List<ToolCall> get tools => [for (final call in calls) call.tool!];
}

/// The work of a turn that ended: its tool calls, thoughts and what the agent said between them, drawn as one line that opens.
final class WorkItem extends TranscriptItem {
  const WorkItem(this.items, this.duration);

  final List<TranscriptItem> items;

  /// From the person's message to the last piece of work.
  final Duration duration;

  /// The tool calls inside, in order.
  List<ToolCall> get tools => [
    for (final item in items)
      ...switch (item) {
        EntryItem(:final entry) => [?entry.tool],
        BurstItem() => item.tools,
        WorkItem() => item.tools,
      },
  ];
}
