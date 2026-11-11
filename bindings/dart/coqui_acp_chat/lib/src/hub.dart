part of 'session.dart';

// EXPERIMENTAL. What a conversation needs to be served to another client that attaches late: the updates that rebuild it, and the
// answer of a client this chat cannot hear. The wire a session hub would carry them on is not specified, so the `_meta` keys here
// are provisional and this API may change or go. It is exported by `package:coqui_acp_chat/experimental.dart` only.

/// The `_meta` key under which an update names the client a message was sent from.
const String chatClientMetaKey = _clientMetaKey;

/// Replay, and requests settled elsewhere. Experimental.
extension ChatSessionHub on ChatSession {
  /// Records that [question] was settled by client [by], which must be attached, with the option [optionId], or cancelled when it
  /// names none the request offered. The agent is given that answer if this chat's connection is the one waiting for it.
  bool settleElsewhere(
    PermissionQuestion question, {
    required String by,
    String? optionId,
  }) {
    _requireClient(by);
    final decision =
        optionId != null && question.options.any((o) => o.optionId == optionId)
        ? AcpSelect(optionId)
        : const AcpCancel();
    return settle(question, decision, by: by);
  }

  /// What the agent has said, as the updates that would make it again: a client attaching late applies them with [applyReplay]
  /// to a [ChatSession] of its own and reaches the same conversation, with its turns, times and options. Notices the chat wrote
  /// itself and a tool [ToolStatus.stopped] (it goes as cancelled) are not carried, and nor are requests still waiting: read
  /// [ChatSession.pendingPermissions] and [ChatSession.pendingQuestions].
  List<AcpSessionUpdate> replay() {
    final out = <AcpSessionUpdate>[];
    final session = sessionId ?? '';
    String at(DateTime time) => time.toUtc().toIso8601String();
    void add(Map<String, Object?> raw, DateTime time) {
      final meta = <String, Object?>{
        ...?((raw['_meta'] as Map?)?.cast<String, Object?>()),
        _atMetaKey: at(time),
      };
      out.add(
        AcpSessionUpdate(
          session,
          Map<String, dynamic>.of({...raw, '_meta': meta}),
        ),
      );
    }

    ChatTurn? turnOf(int id) => _turns.where((t) => t.id == id).firstOrNull;
    int? open;
    void closeOpen() {
      final id = open;
      if (id == null) return;
      open = null;
      final turn = turnOf(id);
      if (turn == null || turn.isRunning) return;
      add({
        'sessionUpdate': 'state_update',
        'state': 'idle',
        'stopReason': switch (turn.outcome) {
          TurnOutcome.cancelled => 'cancelled',
          TurnOutcome.failed => 'refusal',
          _ => 'end_turn',
        },
      }, turn.endedAt!);
    }

    void openFor(ChatTurn turn) {
      open = turn.id;
      add({
        'sessionUpdate': 'state_update',
        'state': 'running',
      }, turn.startedAt);
    }

    // The turns go out in order, those that kept no entry too, each opened before its first entry and closed after its last.
    var next = 0;
    void advanceTo(int? id) {
      if (id == open) return;
      closeOpen();
      if (id == null || turnOf(id) == null) return;
      while (open != id && next < _turns.length) {
        final turn = _turns[next++];
        openFor(turn);
        if (turn.id != id) closeOpen();
      }
    }

    for (final message in _messages) {
      advanceTo(message.turn);
      switch (message.role) {
        case MessageRole.user || MessageRole.agent || MessageRole.thought:
          add({
            'sessionUpdate': switch (message.role) {
              MessageRole.user => 'user_message',
              MessageRole.thought => 'agent_thought',
              _ => 'agent_message',
            },
            'messageId': ?message.messageId,
            'content': {'type': 'text', 'text': message.text},
            if (message.sentBy != null)
              '_meta': {_clientMetaKey: message.sentBy},
          }, message.at);
        case MessageRole.tool:
          final tool = message.tool!;
          add(tool.toUpdate(), message.at);
          for (final block in tool.blocks) {
            final terminal = block is ToolTerminal
                ? _terminals[block.terminalId]
                : null;
            if (terminal != null) {
              add(
                terminal.toUpdate((block as ToolTerminal).terminalId),
                message.at,
              );
            }
          }
        case MessageRole.compaction:
          add(message.compaction!.toUpdate(), message.at);
        case MessageRole.notice:
          break;
      }
    }
    closeOpen();
    while (next < _turns.length) {
      openFor(_turns[next++]);
      closeOpen();
    }
    // A turn still running stays open: closeOpen leaves it without an end.
    final now = _clock();
    if (_plan.isNotEmpty) {
      add({
        'sessionUpdate': 'plan',
        'entries': [
          for (final entry in _plan)
            {'content': entry.content, 'status': planStatusWord(entry.status)},
        ],
      }, now);
    }
    if (_commands.isNotEmpty) {
      add({
        'sessionUpdate': 'available_commands_update',
        'availableCommands': [
          for (final command in _commands)
            {'name': command.name, 'description': command.description},
        ],
      }, now);
    }
    if (_options.isNotEmpty) {
      final mode = _options.where((o) => o.isMode).firstOrNull;
      add({
        'sessionUpdate': 'config_option_update',
        'configOptions': [
          for (final option in _options)
            if (!option.isMode) option.toJson(),
        ],
        if (mode != null)
          'modes': {
            'currentModeId': mode.current,
            'availableModes': [
              for (final choice in mode.choices)
                {'id': choice.value, 'name': choice.label},
            ],
          },
      }, now);
    }
    if (!_usage.isEmpty) add(_usage.toUpdate(), now);
    if (!_agentNames || _title != 'New conversation') {
      add({
        'sessionUpdate': 'session_info_update',
        'title': _title,
        if (!_agentNames) '_meta': {_namesMetaKey: false},
      }, now);
    }
    return out;
  }

  /// Applies [updates] made by [replay] to a conversation of this chat's own: what they say is history, not news, so nobody is
  /// told of an attention or a turn ending.
  void applyReplay(Iterable<AcpSessionUpdate> updates) {
    _replaying = true;
    try {
      for (final update in updates) {
        apply(update);
      }
    } finally {
      _replaying = false;
    }
  }
}
