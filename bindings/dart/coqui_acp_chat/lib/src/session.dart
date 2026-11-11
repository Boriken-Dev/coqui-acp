import 'dart:async';

import 'package:coqui_acp/coqui_acp.dart';

import 'connection.dart';
import 'content.dart';
import 'limits.dart';
import 'notifier.dart';
import 'options.dart';
import 'rendering.dart';
import 'text.dart';
import 'usage.dart';

/// Where a conversation stands with its agent.
enum ChatStatus { disconnected, connected, working }

/// Who a conversation entry is from, or what it is.
enum MessageRole { user, agent, notice, thought, tool, compaction }

/// What a conversation needs a person for: a permission request, a question, or a turn it finished.
enum AttentionKind { permission, question, finished }

/// How a turn ended.
enum TurnOutcome { completed, cancelled, failed }

/// The `_meta` key under which an update names the client a message was sent from. Provisional: the hub that would set it is not
/// specified yet.
const String chatClientMetaKey = 'dev.boriken.coqui.acp/client';

/// The name a request is settled under when the chat's own Bypass answered it.
const String bypassClient = 'bypass';

/// One entry of a conversation: a message, a notice, a tool call or a compaction.
class ChatMessage {
  ChatMessage({
    required this.role,
    required this.text,
    required this.at,
    this.messageId,
    this.tool,
    this.compaction,
    this.sentBy,
    this.turn,
  });

  final MessageRole role;
  String text;
  final DateTime at;
  final String? messageId;
  final ToolCall? tool;
  final Compaction? compaction;

  /// The client a person's message was sent from, when it is known; null for one the agent replayed without saying.
  String? sentBy;

  /// The [ChatTurn.id] of the turn that was running when this was added; null outside any.
  final int? turn;
}

/// One turn of the agent: from when it started working until it was idle again.
final class ChatTurn {
  ChatTurn(this.id, this.startedAt);
  final int id;
  final DateTime startedAt;
  DateTime? endedAt;
  TurnOutcome? outcome;

  /// How long it ran, or has run so far.
  Duration duration(DateTime now) => (endedAt ?? now).difference(startedAt);

  bool get isRunning => endedAt == null;
}

/// A client attached to a conversation: a window, a device, a person watching it.
final class ChatClient {
  ChatClient(this.id, this.name, this.attachedAt);
  final String id;
  final String? name;
  final DateTime attachedAt;
}

/// How a request the agent made of the person came to its end.
enum Settlement {
  /// A client answered it, [Settled.by] when it said which.
  answered,

  /// It was answered cancelled because the turn was stopped or the conversation closed.
  cancelled,

  /// The connection it came by ended or was replaced, so nobody can answer it and nothing was sent.
  withdrawn,
}

/// What became of a request.
final class Settled<T> {
  const Settled(this.how, this.at, {this.by, this.answer});
  final Settlement how;
  final DateTime at;

  /// The client that answered it, [bypassClient] for Bypass; null when nobody did.
  final String? by;

  /// What the agent was told, when it was told anything.
  final T? answer;
}

/// A tool an agent wants to use, waiting on the person. Nothing answers it for them but Bypass, which answers a request that
/// offers an allow before it would wait here. The first client to answer it settles it; the rest find [outcome] set.
final class PermissionQuestion {
  PermissionQuestion._(
    this.id,
    this.title,
    this.kind, {
    this.detail,
    this.options = const [],
    this.bypassPassed = false,
    this.forPerson = false,
    this.toolCallId,
    required this.askedAt,
  });

  /// Its place in the order the conversation was asked, from 1.
  final int id;
  final String title;
  final String? kind;
  final DateTime askedAt;

  /// The tool call this asks about, whose line in the conversation is marked stopped when the request can no longer be answered.
  final String? toolCallId;

  /// What it would run or touch, when the agent said.
  final String? detail;

  /// The answers the agent offers, in its order.
  final List<AcpPermissionOption> options;

  /// Whether Bypass was on and left this to the person.
  final bool bypassPassed;

  /// Whether it is a question or a plan for the person, which Bypass never answers.
  final bool forPerson;

  final Completer<AcpDecision> _answer = Completer();

  /// Null while it waits.
  Settled<AcpDecision>? outcome;

  bool get isOpen => outcome == null;
}

/// An agent's question, waiting on the person. Nothing answers it for them; Bypass is for permission requests only.
final class AgentQuestion {
  AgentQuestion._(this.id, this.request, this.askedAt);
  final int id;
  final AcpElicitation request;
  final DateTime askedAt;
  final Completer<AcpElicitationAnswer> _answer = Completer();
  Settled<AcpElicitationAnswer>? outcome;
  bool get isOpen => outcome == null;
}

/// A message written while the agent was working, sent when its turn ends.
final class QueuedMessage {
  QueuedMessage(this.text, this.attachments, {this.from});
  String text;
  final List<AcpAttachment> attachments;

  /// The client that wrote it.
  final String? from;
}

/// Something a conversation tells beside its state: it is delivered once, to whoever listens when it happens.
sealed class ChatEvent {
  const ChatEvent();
}

/// The conversation came to need the person.
final class AttentionEvent extends ChatEvent {
  const AttentionEvent(this.kind);
  final AttentionKind kind;
}

/// A permission request was settled, by whom [PermissionQuestion.outcome] says: the others attached see it settled.
final class PermissionSettledEvent extends ChatEvent {
  const PermissionSettledEvent(this.question);
  final PermissionQuestion question;
}

/// A question was settled.
final class QuestionSettledEvent extends ChatEvent {
  const QuestionSettledEvent(this.question);
  final AgentQuestion question;
}

/// A client attached or detached.
final class ClientEvent extends ChatEvent {
  const ClientEvent(this.client, {required this.attached});
  final ChatClient client;
  final bool attached;
}

/// Why [error] happened, in the words it carries for a person: for an agent's own error, the message it gave.
String reasonOf(Object error) => switch (error) {
  StateError(:final message) => message,
  AcpFailure(:final message) =>
    RegExp(
          r'^Agent error: \{.*\bmessage: (.*)\}$',
          dotAll: true,
        ).firstMatch(message)?.group(1) ??
        message,
  _ => '$error',
};

/// The title a conversation takes from its first message when it was given none.
String titleFrom(String text, [ChatLimits limits = const ChatLimits()]) {
  final line = text.trim().split('\n').first.trim();
  final max = limits.titleLength;
  if (line.length <= max) return line;
  return max == 0 ? '' : '${line.substring(0, max - 1).trimRight()}…';
}

/// One conversation with an agent, built from the updates it sends and the requests it makes, and the person's side of it:
/// prompts, the queue, answers. It has no widgets and no transport; a [ChatConnection] is attached to it.
///
/// Several clients can be attached ([attachClient]): a prompt is stamped with the one that sent it, and of several answers to a
/// request the first settles it. A client that attaches late reads [replay], then follows [events] and its listeners.
class ChatSession extends ChatNotifier {
  ChatSession({
    required this.id,
    this.title = 'New conversation',
    this.agent = '',
    this.peerId,
    this.sessionId,
    this.cwd,
    this.limits = const ChatLimits(),
    this.recorder,
    this.hostNoticesKey,
    DateTime Function()? clock,
    this.agentNames = true,
  }) : _clock = clock ?? DateTime.now;

  /// This view's own id, which the agent's session id is not.
  final String id;
  String title;

  /// The agent's name as shown.
  String agent;

  /// Who the agent is, as the host proved it.
  String? peerId;

  /// The agent's id for the conversation, which its updates and the requests sent for it name. Null until the agent has opened it.
  String? sessionId;

  /// Where the agent works, on its machine, when the host knows.
  String? cwd;

  final ChatLimits limits;

  /// Told the kind and shape of each update this conversation applies, never its text.
  final UpdateRecorder? recorder;

  /// The `_meta` key under which a host lists, in its answer to `session/new`, what a person should know about an agent. Null reads
  /// none.
  final String? hostNoticesKey;

  final DateTime Function() _clock;

  /// Whether the agent may name the conversation, which it may until the person has ([rename]).
  bool agentNames;

  ChatStatus status = ChatStatus.disconnected;

  /// Whether a turn ended here since the person last chose this conversation: set by [ChatStore].
  bool unread = false;

  final List<ChatMessage> messages = [];
  final Map<String, ChatMessage> toolEntries = {};
  final Map<String, ChatMessage> compactions = {};

  /// Commands the agent ran, which a tool's entry shows.
  final Map<String, TerminalView> terminals = {};

  /// The person's messages whose attachments the conversation has named, so a message sent here and echoed back is named once.
  final Set<String> attachmentsNamed = {};

  /// The agent's plan, as it last said.
  List<PlanEntry> plan = const [];

  /// What the agent lets a person choose, its commands and what it has used, each as it last said.
  List<ConfigOption> options = const [];
  List<AgentCommandInfo> commands = const [];
  SessionUsage usage = const SessionUsage();

  /// What the agent last said of the account's allowances; null while it has not.
  UsageReport? usageReport;

  /// Whether this conversation's agent reports usage, which decides whether a usage control shows at all.
  bool reportsUsage = false;

  /// What this conversation has spent, as the agent last said.
  UsageSpend? spend;

  /// What the agent is waiting for the person to answer, oldest first.
  final List<PermissionQuestion> pendingPermissions = [];

  /// The agent's questions waiting for the person, oldest first.
  final List<AgentQuestion> pendingQuestions = [];

  final List<PermissionQuestion> _settledPermissions = [];
  final List<AgentQuestion> _settledQuestions = [];

  /// The requests most recently settled, oldest first, for a client that attaches late to see who answered.
  List<PermissionQuestion> get settledPermissions =>
      List.unmodifiable(_settledPermissions);
  List<AgentQuestion> get settledQuestions =>
      List.unmodifiable(_settledQuestions);

  /// Messages written while a turn ran, oldest first. The first is sent when a turn ends as it should.
  final List<QueuedMessage> queue = [];

  /// Why the queue waits for the person rather than going on: a turn that was stopped or failed. Null while it goes on.
  String? queueHeld;

  /// Choices made while a turn ran, by option id: sent before the next prompt.
  final Map<String, PendingChoice> pending = {};

  /// Whether this chat answers the conversation's permission requests that offer an allow, as the Bypass mode of an agent that has
  /// none of its own. Off unless a person chose it ([setBypass]).
  bool bypass = false;

  /// Whether the conversation is being opened again and its agent has not loaded it yet.
  bool opening = false;

  /// The turns of this conversation, oldest first, up to [ChatLimits.turns].
  final List<ChatTurn> turns = [];

  /// The turn running now; null between turns.
  ChatTurn? get currentTurn =>
      turns.isNotEmpty && turns.last.isRunning ? turns.last : null;

  int _nextTurn = 1;
  int _nextRequest = 1;

  /// Whether something here waits for the person's answer.
  bool get needsYou =>
      pendingPermissions.isNotEmpty || pendingQuestions.isNotEmpty;

  final _events = StreamController<ChatEvent>.broadcast();

  /// What happened beside the state: attention, settled requests, clients coming and going.
  Stream<ChatEvent> get events => _events.stream;

  /// Called after every change of state; [ChatStore] sets it to pass the change on.
  void Function()? onChange;

  final Map<String, ChatClient> _clients = {};

  /// The clients attached now, oldest first.
  List<ChatClient> get clients => List.unmodifiable(_clients.values);

  ChatConnection? _connection;
  StreamSubscription<AcpSessionUpdate>? _updatesSub;
  StreamSubscription<AcpEvent>? _eventsSub;
  StreamSubscription<UsageNotice>? _usageSub;
  bool _steering = false;

  /// Whether a message is being handed to the agent: until it has taken it, or for a v1 agent until its turn has started.
  bool _delivering = false;

  /// How a turn ended while a message was being handed over, whose queue then goes on once it has been.
  TurnOutcome? _deferredEnd;
  bool _replaying = false;

  /// Whether the agent is sending the conversation so far, which is not news to anyone.
  bool get isReplaying => _replaying;

  /// The connection this conversation is with; null when it has none.
  ChatConnection? get connection => _connection;

  DateTime _now() => _clock();

  @override
  void notifyListeners() {
    super.notifyListeners();
    onChange?.call();
  }

  void _emit(ChatEvent event) {
    if (!_events.isClosed) _events.add(event);
  }

  void _needs(AttentionKind kind) {
    if (_replaying) return;
    _emit(AttentionEvent(kind));
  }

  /// Sets the person's name for the conversation, which the agent then no longer replaces.
  void rename(String name) {
    final shown = shortText(name, limits);
    if (shown == null) return;
    title = shown;
    agentNames = false;
    notifyListeners();
  }

  // Clients.

  /// Attaches [clientId], a name the host gives each client, which then may send and answer as itself. Attaching one that is
  /// attached already changes nothing. Throws when [ChatLimits.clients] are attached.
  ChatClient attachClient(String clientId, {String? name}) {
    final held = _clients[clientId];
    if (held != null) return held;
    if (clientId.isEmpty || clientId == bypassClient) {
      throw ArgumentError.value(clientId, 'clientId', 'Not a usable client id');
    }
    if (_clients.length >= limits.clients) {
      throw StateError('${limits.clients} clients are attached already');
    }
    final client = ChatClient(clientId, shortText(name, limits), _now());
    _clients[clientId] = client;
    _emit(ClientEvent(client, attached: true));
    notifyListeners();
    return client;
  }

  /// Detaches [clientId]. What it sent and answered stays; the conversation goes on without it.
  void detachClient(String clientId) {
    final client = _clients.remove(clientId);
    if (client == null) return;
    _emit(ClientEvent(client, attached: false));
    notifyListeners();
  }

  void _requireClient(String? clientId) {
    if (clientId != null &&
        clientId != bypassClient &&
        !_clients.containsKey(clientId)) {
      throw StateError('$clientId is not attached to this conversation');
    }
  }

  // The connection.

  /// Binds [connection], which the agent's updates for [sessionId] arrive on. A conversation bound to another connection first
  /// withdraws what that one asked, since nothing can answer it now. Throws when the connection is closed or no session id is
  /// known.
  void attach(ChatConnection connection) {
    if (sessionId == null || connection.isClosed) {
      throw StateError('An open connection and a session id are required');
    }
    unawaited(_updatesSub?.cancel());
    unawaited(_eventsSub?.cancel());
    unawaited(_usageSub?.cancel());
    _usageSub = null;
    final previous = _connection;
    if (previous != null && !identical(previous, connection)) _withdraw();
    _connection = connection;
    status = ChatStatus.connected;
    _updatesSub = connection.updates.listen((update) {
      if (update.sessionId == sessionId) apply(update);
    }, onDone: () => _connectionEnded(connection));
    // A v1 turn has no state updates: its end arrives as an event.
    _eventsSub = connection.events.listen(
      (event) => _onEvent(connection, event),
    );
    _watchUsage(connection);
    notifyListeners();
  }

  void _connectionEnded(ChatConnection connection) {
    if (!identical(_connection, connection)) return;
    _connection = null;
    status = ChatStatus.disconnected;
    _closeTurn(TurnOutcome.failed);
    _withdraw();
    notifyListeners();
  }

  /// Stops listening to the connection without closing it: it stays with whoever else uses it. What it asked the person is
  /// withdrawn.
  void detach() {
    unawaited(_updatesSub?.cancel());
    unawaited(_eventsSub?.cancel());
    unawaited(_usageSub?.cancel());
    _updatesSub = _eventsSub = _usageSub = null;
    if (_connection != null) {
      _connection = null;
      status = ChatStatus.disconnected;
      _closeTurn(TurnOutcome.failed);
      _withdraw();
      notifyListeners();
    }
  }

  void _onEvent(ChatConnection connection, AcpEvent event) {
    if (!identical(_connection, connection)) return;
    if (event is AcpOutputLost) {
      final messages = event.lines == 1
          ? '1 message'
          : '${event.lines} messages';
      _note(
        'While nobody was connected, $messages from the agent (${event.bytes} bytes) were dropped, '
        'so something it said or did is missing here.',
      );
      notifyListeners();
      return;
    }
    if (event is AcpBlocked) {
      // The agent was told the question was cancelled, and its turn goes on until it ends it.
      _note(
        'A question from the agent was answered cancelled: ${event.message}.',
      );
      notifyListeners();
      return;
    }
    if (connection.protocolVersion != 1) return;
    if (event is AcpCompleted || event is AcpCancelled) {
      final cancelled = event is AcpCancelled;
      status = ChatStatus.connected;
      if (cancelled) _note('Stopped.');
      if (!cancelled) _needs(AttentionKind.finished);
      _turnEnded(cancelled ? TurnOutcome.cancelled : TurnOutcome.completed);
      notifyListeners();
    } else if (event is AcpFailed) {
      status = ChatStatus.connected;
      _add(
        ChatMessage(
          role: MessageRole.notice,
          text: event.message,
          at: _now(),
          turn: currentTurn?.id,
        ),
      );
      _needs(AttentionKind.finished);
      _turnEnded(TurnOutcome.failed);
      notifyListeners();
    }
  }

  // Usage.

  void _watchUsage(ChatConnection connection) {
    usageReport = null;
    reportsUsage = connection.reportsUsage;
    if (!reportsUsage) return;
    _usageSub = connection.usageNotices.listen(
      (notice) => _usageArrived(notice.sessionId, notice.report),
    );
    unawaited(refreshUsage());
  }

  /// Asks the agent for its report now, as it is after each turn.
  Future<void> refreshUsage() async {
    final connection = _connection;
    if (connection == null || connection.isClosed || !reportsUsage) return;
    final report = await connection.fetchUsage(sessionId: sessionId);
    if (report != null && identical(_connection, connection)) {
      _usageArrived(sessionId, report);
    }
  }

  /// Takes [report] for this conversation: the newest by revision wins, and the spend is kept only when it is this one's.
  void _usageArrived(String? named, UsageReport report) {
    final mine = sessionId;
    final own =
        report.spend != null &&
        (named == mine || report.spend!.sessionId == mine);
    if (own) spend = report.spend;
    final held = usageReport;
    if (held != null && held.revision > report.revision) {
      notifyListeners();
      return;
    }
    usageReport = report.supported ? report.withSpend(null) : null;
    if (!report.supported) reportsUsage = false;
    notifyListeners();
  }

  // Applying what the agent sends.

  /// What the agent said when it opened this conversation: its options, commands and the notices a host lists.
  void applyOpened(AcpOpenedSession opened) {
    _setOptions(opened.raw['configOptions'], opened.raw['modes']);
    commands =
        parseCommands(opened.raw['availableCommands'], limits) ?? commands;
    final key = hostNoticesKey;
    final meta = opened.raw['_meta'];
    if (key != null && meta is Map && meta[key] is List) {
      // Any agent can send these, not only the host, so each is shown as the agent's word, never as the chat's.
      for (final notice in (meta[key] as List).take(limits.hostNotices)) {
        final text = shortText(notice, limits);
        if (text != null) _note('The agent says: $text');
      }
    }
    notifyListeners();
  }

  /// Opens this conversation again through [open], which asks the agent to send it: what arrives while it does is history, not
  /// news, and a tool line its earlier run left waiting or running is marked stopped, since that run ended with its process.
  Future<AcpOpenedSession> reopen(
    Future<AcpOpenedSession> Function() open,
  ) async {
    _replaying = true;
    opening = true;
    try {
      final opened = await open();
      applyOpened(opened);
      for (final entry in toolEntries.values) {
        _stop(entry.tool);
      }
      if (!opened.replayed) {
        _note('The agent resumed this without sending what was said before.');
      }
      notifyListeners();
      return opened;
    } finally {
      _replaying = false;
      opening = false;
    }
  }

  void _setOptions(Object? configOptions, Object? modes) {
    final parsed = parseConfigOptions(configOptions, limits);
    // An agent with both says the same thing twice, and ACP prefers the options.
    final mode = parsed == null || parsed.isEmpty
        ? parseModes(modes, limits)
        : null;
    if (parsed != null && parsed.isNotEmpty) {
      options = parsed;
    } else if (mode != null) {
      options = [mode];
    }
  }

  void _setCurrent(String id, String value) {
    options = [
      for (final option in options)
        option.id == id ? option.withCurrent(value) : option,
    ];
  }

  void _add(ChatMessage message) => messages.add(message);

  void _note(String text) => _add(
    ChatMessage(
      role: MessageRole.notice,
      text: text,
      at: _now(),
      turn: currentTurn?.id,
    ),
  );

  void _trim() {
    final over = messages.length - limits.entries;
    if (over <= 0) return;
    for (final gone in messages.take(over)) {
      if (gone.tool != null) toolEntries.remove(gone.tool!.id);
      if (gone.compaction != null) compactions.remove(gone.compaction!.id);
    }
    messages.removeRange(0, over);
  }

  void _applyTool(Map<String, dynamic> raw) {
    final id = raw['toolCallId'];
    if (id is! String || id.isEmpty) return;
    var entry = toolEntries[id];
    if (entry == null) {
      entry = ChatMessage(
        role: MessageRole.tool,
        text: '',
        at: _now(),
        tool: ToolCall(id),
        turn: currentTurn?.id,
      );
      toolEntries[id] = entry;
      _add(entry);
    }
    entry.tool!.apply(raw, limits);
  }

  void _applyTerminal(Map<String, dynamic> raw) {
    final id = raw['terminalId'];
    if (id is! String || id.isEmpty) return;
    if (terminals.length >= limits.terminals && !terminals.containsKey(id)) {
      if (terminals.isEmpty) return;
      terminals.remove(terminals.keys.first);
    }
    if (limits.terminals == 0) return;
    (terminals[id] ??= TerminalView()).apply(raw, limits);
  }

  /// A compaction's entry stays where its first update put it, and later ones change it there.
  void _applyCompaction(Map<String, dynamic> raw) {
    final id = raw['compactionId'];
    if (id is! String || id.isEmpty) return;
    var entry = compactions[id];
    if (entry == null) {
      entry = ChatMessage(
        role: MessageRole.compaction,
        text: '',
        at: _now(),
        compaction: Compaction(id),
        turn: currentTurn?.id,
      );
      compactions[id] = entry;
      _add(entry);
    }
    entry.compaction!.apply(raw, limits);
  }

  /// Names what went with a person's message, once per message.
  void _nameAttachments(String? messageId, List<String> names) {
    if (names.isEmpty ||
        (messageId != null && !attachmentsNamed.add(messageId))) {
      return;
    }
    _note('Sent with this: ${names.join(', ')}');
  }

  void _beginTurn() {
    if (currentTurn != null) return;
    turns.add(ChatTurn(_nextTurn++, _now()));
    if (turns.length > limits.turns) {
      turns.removeRange(0, turns.length - limits.turns);
    }
  }

  void _closeTurn(TurnOutcome outcome) {
    final turn = currentTurn;
    if (turn == null) return;
    turn
      ..endedAt = _now()
      ..outcome = outcome;
  }

  /// Applies one `session/update` of this conversation. A connection's updates arrive here by themselves; a host that feeds the
  /// chat from a channel of its own calls it.
  void apply(AcpSessionUpdate update) {
    final rendering = renderingOf(update, limits);
    recorder?.record(update, rendering, limits);
    switch (rendering) {
      case Rendering.toolCard:
        _applyTool(update.raw);
      case Rendering.terminal:
        _applyTerminal(update.raw);
      case Rendering.plan:
        final entries = update.kind == 'plan_update'
            ? planItems(update.raw['plan'])
            : update.raw['entries'];
        plan = parsePlan(entries, limits) ?? plan;
      case Rendering.notice:
        final title = shortText(update.raw['title'], limits);
        if (title != null) {
          // Any agent can send these, so each is shown as the agent's word, never as the chat's.
          final from = switch (update.raw['severity']) {
            'warning' => 'Warning from the agent',
            'error' => 'Error from the agent',
            _ => 'From the agent',
          };
          final detail = shortText(update.raw['description'], limits);
          _note('$from: $title${detail == null ? '' : '\n$detail'}');
        }
      case Rendering.compaction:
        _applyCompaction(update.raw);
      case Rendering.commands:
        commands =
            parseCommands(update.raw['availableCommands'], limits) ?? commands;
      case Rendering.usage:
        usage = usage.applied(update.raw, limits);
      case Rendering.choices when update.kind == 'config_option_update':
        _setOptions(update.raw['configOptions'], null);
      case Rendering.choices:
        if (update.raw['currentModeId'] case final String mode) {
          for (final option in options) {
            if (option.isMode) _setCurrent(option.id, mode);
          }
        }
      case Rendering.title:
        final title = shortText(update.raw['title'], limits);
        if (title != null && agentNames) this.title = title;
      case Rendering.status:
        _applyState(update);
      case Rendering.message || Rendering.thought:
        _applyMessage(update, rendering);
      case Rendering.unshown:
        break;
    }
    _trim();
    notifyListeners();
  }

  void _applyState(AcpSessionUpdate update) {
    if (update.state == 'running' || update.state == 'requires_action') {
      status = ChatStatus.working;
      _beginTurn();
    } else if (update.state == 'idle') {
      // An idle state outside a turn ends nothing.
      final ended = status == ChatStatus.working;
      status = ChatStatus.connected;
      final outcome = switch (update.reason) {
        'cancelled' => TurnOutcome.cancelled,
        'error' || 'refusal' => TurnOutcome.failed,
        _ => TurnOutcome.completed,
      };
      // A turn the person stopped is not news to them.
      if (ended && outcome != TurnOutcome.cancelled) {
        _needs(AttentionKind.finished);
      }
      if (ended) _turnEnded(outcome);
      if (update.reason == 'cancelled') {
        _note('Stopped.');
      } else if (update.reason == 'error') {
        _add(
          ChatMessage(
            role: MessageRole.notice,
            text: 'Agent reported an operation failure.',
            at: _now(),
            turn: currentTurn?.id,
          ),
        );
      }
    }
  }

  void _applyMessage(AcpSessionUpdate update, Rendering rendering) {
    final chunk = update.kind.endsWith('_chunk');
    // A v2 message or thought without content patches its other fields and keeps its text; null content clears it.
    final text = !chunk && !update.raw.containsKey('content')
        ? null
        : update.text ?? '';
    final role = update.kind.startsWith('user')
        ? MessageRole.user
        : rendering == Rendering.thought
        ? MessageRole.thought
        : MessageRole.agent;
    if (text != null) {
      ChatMessage? existing;
      if (update.messageId != null) {
        for (final message in messages) {
          if (message.messageId == update.messageId) {
            existing = message;
            break;
          }
        }
      }
      // A v1 chunk carries no message id: it continues the last message of its role.
      if (existing == null &&
          update.messageId == null &&
          chunk &&
          messages.isNotEmpty &&
          messages.last.role == role &&
          messages.last.messageId == null) {
        existing = messages.last;
      }
      final meta = update.raw['_meta'];
      final sentBy =
          role == MessageRole.user &&
              meta is Map &&
              meta[chatClientMetaKey] is String
          ? meta[chatClientMetaKey] as String
          : null;
      if (existing != null) {
        existing.text = boundedHead(
          chunk ? existing.text + text : text,
          limits.messageText,
        );
        existing.sentBy ??= sentBy;
      } else if (text.isNotEmpty) {
        _add(
          ChatMessage(
            role: role,
            text: boundedHead(text, limits.messageText),
            at: _now(),
            messageId: update.messageId,
            sentBy: sentBy,
            turn: currentTurn?.id,
          ),
        );
      }
    }
    // A replayed message carries its attachments as content blocks, which have no text of their own.
    if (role == MessageRole.user) {
      _nameAttachments(
        update.messageId,
        attachedNames(update.raw['content'], limits),
      );
    }
  }

  // Requests from the agent.

  /// Puts a permission request to the person. One past [ChatLimits.pendingPermissions] is cancelled. The first answer to it, from
  /// whichever client, is the one the agent gets.
  Future<AcpDecision> askPermission(AcpPermissionRequest request) {
    if (pendingPermissions.length >= limits.pendingPermissions) {
      _note(
        'The agent asked more questions than the ${limits.pendingPermissions} a conversation holds waiting, so the rest '
        'were answered cancelled.',
      );
      notifyListeners();
      return Future.value(const AcpCancel());
    }
    // ACP has the tool call applied before the prompt shows, so the conversation shows what is being asked about.
    if (request.toolCall != null) _applyTool(request.toolCall!);
    final title =
        shortText(request.title, limits) ??
        shortText(request.kind, limits) ??
        'A tool';
    final forPerson = _isForPerson(request);
    if (bypass && !forPerson) {
      final allow = bypassAnswer(request.options);
      if (allow != null) {
        _note(
          'Bypass allowed: $title (${shortText(allow.name, limits) ?? allow.kind}).',
        );
        _trim();
        notifyListeners();
        return Future.value(AcpSelect(allow.optionId));
      }
    }
    final question = PermissionQuestion._(
      _nextRequest++,
      title,
      request.kind,
      detail: [
        ?shortText(request.description, limits),
        ?describeToolInput(request.rawInput, limits),
      ].join('\n').trim(),
      options: request.options,
      bypassPassed: bypass,
      forPerson: forPerson,
      toolCallId: request.toolCall?['toolCallId'] as String?,
      askedAt: _now(),
    );
    pendingPermissions.add(question);
    _needs(AttentionKind.permission);
    _trim();
    notifyListeners();
    return question._answer.future;
  }

  /// Whether a request is a question or a plan for the person, whatever mode: Bypass leaves it to them.
  static bool _isForPerson(AcpPermissionRequest request) {
    if (request.kind == 'switch_mode') return true;
    final input = request.rawInput;
    return input is Map && input['questions'] is List;
  }

  /// Puts an agent's question to the person. One past [ChatLimits.pendingQuestions] is cancelled.
  Future<AcpElicitationAnswer> askQuestion(AcpElicitation request) {
    if (pendingQuestions.length >= limits.pendingQuestions) {
      _note(
        'The agent asked more questions than the ${limits.pendingQuestions} a conversation holds waiting, so the rest '
        'were answered cancelled.',
      );
      notifyListeners();
      return Future.value(const AcpElicitCancel());
    }
    final question = AgentQuestion._(_nextRequest++, request, _now());
    pendingQuestions.add(question);
    _needs(AttentionKind.question);
    notifyListeners();
    return question._answer.future;
  }

  void _settle(PermissionQuestion question, Settled<AcpDecision> outcome) {
    question.outcome = outcome;
    pendingPermissions.remove(question);
    _settledPermissions.add(question);
    if (_settledPermissions.length > limits.settledRequests) {
      _settledPermissions.removeRange(
        0,
        _settledPermissions.length - limits.settledRequests,
      );
    }
    _emit(PermissionSettledEvent(question));
  }

  void _settleQuestion(
    AgentQuestion question,
    Settled<AcpElicitationAnswer> outcome,
  ) {
    question.outcome = outcome;
    pendingQuestions.remove(question);
    _settledQuestions.add(question);
    if (_settledQuestions.length > limits.settledRequests) {
      _settledQuestions.removeRange(
        0,
        _settledQuestions.length - limits.settledRequests,
      );
    }
    _emit(QuestionSettledEvent(question));
  }

  /// Answers [question] with [decision] as client [by]. The first answer settles it, and it answers whether this one did: a
  /// client that comes second, or answers one the connection took away, changes nothing and sends nothing.
  bool settle(PermissionQuestion question, AcpDecision decision, {String? by}) {
    _requireClient(by);
    if (!question.isOpen || !pendingPermissions.contains(question)) {
      return false;
    }
    _settle(
      question,
      Settled(Settlement.answered, _now(), by: by, answer: decision),
    );
    question._answer.complete(decision);
    notifyListeners();
    return true;
  }

  /// The answer of a client this chat cannot hear, learned from elsewhere: [question] was settled by [by] with the option
  /// [optionId], or cancelled when it names none. The agent is given that answer if this chat's connection is the one waiting for
  /// it.
  bool settleElsewhere(
    PermissionQuestion question, {
    required String by,
    String? optionId,
  }) {
    if (!question.isOpen || !pendingPermissions.contains(question)) {
      return false;
    }
    final decision =
        optionId != null && question.options.any((o) => o.optionId == optionId)
        ? AcpSelect(optionId)
        : const AcpCancel();
    _settle(
      question,
      Settled(Settlement.answered, _now(), by: by, answer: decision),
    );
    question._answer.complete(decision);
    notifyListeners();
    return true;
  }

  /// The oldest waiting request's answer: the agent's first option that allows once, or refuses once. Whether this settled it.
  bool answerPermission({required bool allow, String? by}) {
    if (pendingPermissions.isEmpty) return false;
    return settle(
      pendingPermissions.first,
      allow ? const AcpAllow() : const AcpDeny('the person refused'),
      by: by,
    );
  }

  /// A client's choice of one of the agent's own answers to the oldest waiting request. What an "always" answer remembers is the
  /// agent's to keep. Whether this settled it.
  bool choosePermission(AcpPermissionOption option, {String? by}) {
    if (pendingPermissions.isEmpty) return false;
    final question = pendingPermissions.first;
    if (!question.options.contains(option)) return false;
    return settle(question, AcpSelect(option.optionId), by: by);
  }

  /// An answer to a request offering nothing a person could choose: it is answered cancelled.
  bool cancelPermission({String? by}) {
    if (pendingPermissions.isEmpty) return false;
    return settle(pendingPermissions.first, const AcpCancel(), by: by);
  }

  /// Answers [question], a form, as client [by]; the first answer settles it.
  bool settleQuestion(
    AgentQuestion question,
    AcpElicitationAnswer answer, {
    String? by,
  }) {
    _requireClient(by);
    if (!question.isOpen || !pendingQuestions.contains(question)) return false;
    _settleQuestion(
      question,
      Settled(Settlement.answered, _now(), by: by, answer: answer),
    );
    question._answer.complete(answer);
    notifyListeners();
    return true;
  }

  /// The answer to the oldest of the agent's questions.
  bool answerQuestion(AcpElicitationAnswer answer, {String? by}) {
    if (pendingQuestions.isEmpty) return false;
    return settleQuestion(pendingQuestions.first, answer, by: by);
  }

  /// Takes away what this conversation was still asking of the person when its connection ended or was replaced: the requests go,
  /// and the tool line each was about is marked stopped. Nothing is answered, since the agent that asked is gone or is no longer
  /// the one this conversation is with.
  void _withdraw() {
    for (final question in [...pendingPermissions]) {
      if (question.toolCallId case final id?) _stop(toolEntries[id]?.tool);
      _settle(question, Settled(Settlement.withdrawn, _now()));
    }
    for (final question in [...pendingQuestions]) {
      _settleQuestion(question, Settled(Settlement.withdrawn, _now()));
    }
  }

  static void _stop(ToolCall? tool) {
    if (tool != null && tool.isOpen) tool.status = ToolStatus.stopped;
  }

  /// Answers everything still waiting: its permission requests with [answer], and its questions cancelled.
  void _endWaiting(AcpDecision answer) {
    for (final question in [...pendingPermissions]) {
      _settle(question, Settled(Settlement.cancelled, _now(), answer: answer));
      question._answer.complete(answer);
    }
    for (final question in [...pendingQuestions]) {
      const cancel = AcpElicitCancel();
      _settleQuestion(
        question,
        Settled(Settlement.cancelled, _now(), answer: cancel),
      );
      question._answer.complete(cancel);
    }
  }

  // Bypass and options.

  /// The mode shown: the chat's own Bypass, else a choice still waiting, else the agent's mode option.
  String? get mode {
    if (bypass) return ownBypass;
    final waiting = pending.values
        .where((choice) => choice.role == OptionRole.mode)
        .firstOrNull;
    return waiting?.value ?? modeOptionIn(options)?.current;
  }

  /// The agent's own mode, which the chat's Bypass sits on top of: what the agent says it is in now, never a choice waiting.
  String? get underlyingMode => modeOptionIn(options)?.current;

  /// Whether the conversation is in a mode that asks for nothing: the chat's own Bypass, or the agent's as it says it is in it
  /// now. A choice still waiting is not yet in effect.
  bool get inBypass {
    if (bypass) return true;
    final value = underlyingMode;
    if (value == null) return false;
    final label = modeOptionIn(
      options,
    )?.choices.where((choice) => choice.value == value).firstOrNull?.label;
    return isBypassMode(value, label);
  }

  /// Turns the chat's own Bypass on or off. On, it answers every request that offers an allow, those already waiting included; a
  /// request with no allow, a question or a plan is still the person's.
  void setBypass(bool on, {String? by}) {
    _requireClient(by);
    if (bypass == on) return;
    bypass = on;
    if (on) {
      _note(
        'Mode: Bypass. A request that offers to allow is allowed without asking you.',
      );
      for (final question in [...pendingPermissions]) {
        final allow = question.forPerson
            ? null
            : bypassAnswer(question.options);
        if (allow == null) continue;
        settle(question, AcpSelect(allow.optionId), by: bypassClient);
        _note(
          'Bypass allowed: ${question.title} (${shortText(allow.name, limits) ?? allow.kind}).',
        );
      }
    } else {
      _note('Bypass is off. Requests are asked again.');
    }
    notifyListeners();
  }

  /// A client's choice of one of the agent's options. It goes to the agent now when it can take it; while a turn runs it waits and
  /// goes before the next prompt. [value] null takes back a choice still waiting. Answers whether the agent refused it: one still
  /// waiting, or an agent that could not be reached, has refused nothing.
  Future<bool> choose(ConfigOption option, String? value) async {
    if (value == null) {
      pending.remove(option.id);
      notifyListeners();
      return false;
    }
    if (status == ChatStatus.working) {
      pending[option.id] = PendingChoice(option.id, roleOf(option), value);
      notifyListeners();
      return false;
    }
    pending.remove(option.id);
    return _set(option, value);
  }

  /// Sets one of the agent's choices. It stays as it was unless the agent agrees; a refusal is said in the conversation, with the
  /// agent's reason.
  Future<bool> _set(ConfigOption option, String value) async {
    final connection = _connection;
    final remote = sessionId;
    if (connection == null || remote == null) return false;
    var refused = false;
    try {
      if (option.isMode) {
        await connection.setMode(remote, value);
        _setCurrent(option.id, value);
      } else {
        final reply = await connection.setConfigOption(
          remote,
          option.id,
          value,
        );
        final told = parseConfigOptions(reply['configOptions'], limits);
        if (told != null && told.isNotEmpty) {
          options = told;
        } else {
          _setCurrent(option.id, value);
        }
      }
    } on AcpFailure catch (error) {
      final shown = option.choices
          .where((choice) => choice.value == value)
          .firstOrNull
          ?.label;
      _note(
        'The agent did not change ${option.label} to ${shortText(shown ?? value, limits) ?? 'that'}: ${reasonOf(error)}',
      );
      refused = true;
    }
    notifyListeners();
    return refused;
  }

  /// Sends each choice made while a turn ran, and says what the agent would not take.
  Future<void> _applyPending() async {
    if (pending.isEmpty) return;
    final waiting = [...pending.values];
    pending.clear();
    for (final choice in waiting) {
      // `custom` only says what the agent is in: it is never chosen.
      if (choice.role == OptionRole.mode && choice.value == 'custom') continue;
      final option =
          options
              .where(
                (option) =>
                    option.id == choice.id &&
                    option.choices.any((each) => each.value == choice.value),
              )
              .firstOrNull ??
          options
              .where(
                (option) =>
                    roleOf(option) == choice.role &&
                    option.choices.any((each) => each.value == choice.value),
              )
              .firstOrNull;
      if (option == null) {
        final name = choice.role == OptionRole.other
            ? choice.id
            : choice.role.name;
        _note(
          'The agent offers no $name of ${shortText(choice.value, limits) ?? 'that'} now, so it was not set.',
        );
        continue;
      }
      // A mode is sent even when it is the one shown: what shows may not be what is in effect.
      if (option.current != choice.value || choice.role == OptionRole.mode) {
        await _set(option, choice.value);
      }
    }
    notifyListeners();
  }

  // Prompts and the queue.

  /// Sends [text] from client [from]. A prompt is never retried after a disconnect. Written while a turn runs, the message is
  /// queued instead, and sent when the turn ends; past [ChatLimits.queued] this throws.
  Future<void> send(
    String text, {
    List<AcpAttachment> attachments = const [],
    String? from,
  }) async {
    if (text.trim().isEmpty && attachments.isEmpty) return;
    _requireClient(from);
    if (status == ChatStatus.working || _delivering || queue.isNotEmpty) {
      _enqueue(
        QueuedMessage(text, List.of(attachments), from: from),
        last: true,
      );
      notifyListeners();
      // A queue waiting on nothing, after a turn that ended as it should, goes on at once.
      if (status != ChatStatus.working && !_delivering && queueHeld == null) {
        await sendQueued();
      }
      return;
    }
    await _deliver(text, attachments, from);
  }

  void _enqueue(QueuedMessage message, {required bool last}) {
    if (queue.length >= limits.queued) {
      throw StateError(
        '${limits.queued} messages already wait for this turn to end',
      );
    }
    if (last) {
      queue.add(message);
    } else {
      queue.insert(0, message);
    }
  }

  /// Sends [text] ahead of everything queued and ends the running turn to do it: the agent is stopped, then takes this message
  /// first, and the rest of the queue goes on after it as the turns end. With no turn running it is an ordinary send.
  Future<void> steer(
    String text, {
    List<AcpAttachment> attachments = const [],
    String? from,
  }) async {
    if (text.trim().isEmpty && attachments.isEmpty) return;
    _requireClient(from);
    if (status != ChatStatus.working) {
      await send(text, attachments: attachments, from: from);
      return;
    }
    final message = QueuedMessage(text, List.of(attachments), from: from);
    _enqueue(message, last: false);
    await _steerWith(message);
  }

  /// Promotes a message already queued to the front and ends the running turn, as [steer] does.
  Future<void> steerQueued(QueuedMessage message) async {
    if (!queue.contains(message)) return;
    if (status != ChatStatus.working) {
      await sendQueued(message);
      return;
    }
    queue
      ..remove(message)
      ..insert(0, message);
    await _steerWith(message);
  }

  Future<void> _steerWith(QueuedMessage message) async {
    _steering = true;
    notifyListeners();
    try {
      await cancel();
    } catch (_) {
      // The turn goes on, so the message waits in the queue as any other does.
      _steering = false;
      rethrow;
    }
  }

  /// A turn ended: the next queued message goes, unless the turn did not end as it should, when the queue waits for the person,
  /// who may send it, edit it or clear it. A turn ended to steer is the way the steering message goes.
  void _turnEnded(TurnOutcome outcome) {
    _closeTurn(outcome);
    if (reportsUsage) unawaited(refreshUsage());
    // A message is on its way: what follows the end of this turn waits until the agent has taken it.
    if (_delivering) {
      _deferredEnd = outcome;
      return;
    }
    _queueAfter(outcome);
  }

  void _queueAfter(TurnOutcome outcome) {
    final steered = _steering;
    _steering = false;
    if (queue.isEmpty) return;
    final held = steered && outcome == TurnOutcome.cancelled
        ? null
        : switch (outcome) {
            TurnOutcome.completed => null,
            TurnOutcome.cancelled => 'the turn was stopped',
            TurnOutcome.failed => 'the turn failed',
          };
    queueHeld = held;
    // After the update that ended the turn has been dispatched: an answer to the next prompt could otherwise arrive inside it.
    if (held == null) {
      scheduleMicrotask(
        () => unawaited(
          sendQueued().catchError((Object error) {
            _note('A queued message was not sent: ${reasonOf(error)}');
            notifyListeners();
          }),
        ),
      );
    }
  }

  /// Sends [message], else the first queued message, now, and lets the queue go on after it.
  Future<void> sendQueued([QueuedMessage? message]) async {
    if (queue.isEmpty || status == ChatStatus.working || _delivering) return;
    if (message != null && !queue.contains(message)) return;
    queueHeld = null;
    final next = message ?? queue.first;
    queue.remove(next);
    notifyListeners();
    try {
      await _deliver(next.text, next.attachments, next.from);
    } catch (error) {
      // Not sent, so it stays first in the queue, which waits for the person.
      if (!queue.contains(next)) queue.insert(0, next);
      queueHeld = 'it could not be sent: ${reasonOf(error)}';
      notifyListeners();
      rethrow;
    }
  }

  /// Changes the text of a queued message.
  void editQueued(QueuedMessage message, String text) {
    if (!queue.contains(message) ||
        (text.trim().isEmpty && message.attachments.isEmpty)) {
      return;
    }
    message.text = text;
    notifyListeners();
  }

  /// Takes one queued message out, or every one.
  void removeQueued([QueuedMessage? message]) {
    if (message == null) {
      queue.clear();
    } else {
      queue.remove(message);
    }
    if (queue.isEmpty) queueHeld = null;
    notifyListeners();
  }

  Future<void> _deliver(
    String text,
    List<AcpAttachment> attachments,
    String? from,
  ) async {
    final connection = _connection;
    final remote = sessionId;
    if (connection == null || connection.isClosed || remote == null) {
      throw StateError('A connected ACP peer is required');
    }
    _delivering = true;
    try {
      await _handOver(connection, remote, text, attachments, from);
    } finally {
      _delivering = false;
      final ended = _deferredEnd;
      _deferredEnd = null;
      if (ended != null) _queueAfter(ended);
    }
  }

  Future<void> _handOver(
    ChatConnection connection,
    String remote,
    String text,
    List<AcpAttachment> attachments,
    String? from,
  ) async {
    await _applyPending();
    final sent = attachments.isEmpty
        ? null
        : 'Sent with this: ${attachments.map((item) => item.name).join(', ')}';
    if (connection.protocolVersion == 1) {
      // A v1 answer arrives when the whole turn ends, so the message shows now, and the turn starts now.
      status = ChatStatus.working;
      _delivering = false;
      _beginTurn();
      if (text.trim().isNotEmpty) {
        _add(
          ChatMessage(
            role: MessageRole.user,
            text: text,
            at: _now(),
            sentBy: from,
            turn: currentTurn?.id,
          ),
        );
      }
      if (sent != null) _note(sent);
      _trim();
      final turn = connection.prompt(remote, text, attachments: attachments);
      notifyListeners();
      try {
        await turn;
      } catch (_) {
        // A turn that failed without saying so ends here, or the conversation would wait for it for good.
        if (status == ChatStatus.working &&
            identical(_connection, connection)) {
          status = ChatStatus.connected;
          _turnEnded(TurnOutcome.failed);
          notifyListeners();
        }
        rethrow;
      }
      return;
    }
    final messageId = await connection.prompt(
      remote,
      text,
      attachments: attachments,
    );
    final echoed = messageId == null
        ? null
        : messages
              .where((message) => message.messageId == messageId)
              .firstOrNull;
    if (echoed != null) {
      echoed.sentBy ??= from;
    } else if (text.trim().isNotEmpty) {
      _add(
        ChatMessage(
          role: MessageRole.user,
          text: text,
          at: _now(),
          messageId: messageId,
          sentBy: from,
          turn: currentTurn?.id,
        ),
      );
    }
    _nameAttachments(messageId, [for (final item in attachments) item.name]);
    _trim();
    notifyListeners();
  }

  /// Ends the running turn, answering every request still open as ACP requires: cancelled, since the turn cannot end until they
  /// are.
  Future<void> cancel({String? from}) async {
    _requireClient(from);
    final connection = _connection;
    final remote = sessionId;
    if (connection == null || remote == null) {
      throw StateError('No active connected ACP session');
    }
    _endWaiting(const AcpCancel());
    notifyListeners();
    await connection.cancel(remote);
  }

  /// Ends the conversation here: a request still open is answered cancelled. The connection is left to its owner.
  void close() {
    _endWaiting(const AcpCancel());
    detach();
    dispose();
  }

  @override
  void dispose() {
    unawaited(_updatesSub?.cancel());
    unawaited(_eventsSub?.cancel());
    unawaited(_usageSub?.cancel());
    _updatesSub = _eventsSub = _usageSub = null;
    _connection = null;
    unawaited(_events.close());
    super.dispose();
  }

  // Replay.

  /// What the agent has said, as the updates that would make it again: a client attaching late applies them to a [ChatSession]
  /// of its own and reaches the same state. Notices the chat wrote itself, requests still waiting (read [pendingPermissions] and
  /// [pendingQuestions]) and a tool [ToolStatus.stopped] (it goes as cancelled) are not carried.
  Iterable<AcpSessionUpdate> replay() sync* {
    final id = sessionId ?? '';
    AcpSessionUpdate update(Map<String, Object?> raw) =>
        AcpSessionUpdate(id, Map<String, dynamic>.of(raw));
    for (final message in messages) {
      switch (message.role) {
        case MessageRole.user || MessageRole.agent || MessageRole.thought:
          yield update({
            'sessionUpdate': switch (message.role) {
              MessageRole.user => 'user_message',
              MessageRole.thought => 'agent_thought',
              _ => 'agent_message',
            },
            'messageId': ?message.messageId,
            'content': {'type': 'text', 'text': message.text},
            if (message.sentBy != null)
              '_meta': {chatClientMetaKey: message.sentBy},
          });
        case MessageRole.tool:
          final tool = message.tool!;
          yield update(tool.toUpdate());
          for (final block in tool.blocks) {
            final terminal = block is ToolTerminal
                ? terminals[block.terminalId]
                : null;
            if (terminal != null) {
              yield update(
                terminal.toUpdate((block as ToolTerminal).terminalId),
              );
            }
          }
        case MessageRole.compaction:
          yield update(message.compaction!.toUpdate());
        case MessageRole.notice:
          break;
      }
    }
    if (plan.isNotEmpty) {
      yield update({
        'sessionUpdate': 'plan',
        'entries': [
          for (final entry in plan)
            {'content': entry.content, 'status': planStatusWord(entry.status)},
        ],
      });
    }
    if (commands.isNotEmpty) {
      yield update({
        'sessionUpdate': 'available_commands_update',
        'availableCommands': [
          for (final command in commands)
            {'name': command.name, 'description': command.description},
        ],
      });
    }
    if (options.isNotEmpty) {
      yield update({
        'sessionUpdate': 'config_option_update',
        'configOptions': [for (final option in options) option.toJson()],
      });
    }
    if (!usage.isEmpty) yield update(usage.toUpdate());
    if (!agentNames || title != 'New conversation') {
      yield update({'sessionUpdate': 'session_info_update', 'title': title});
    }
    if (status == ChatStatus.working) {
      yield update({'sessionUpdate': 'state_update', 'state': 'running'});
    }
  }
}

/// [entries] in runs of consecutive tool calls: a transcript can fold each run into one line, such as "Edited calc.js, ran a
/// command", and open one that failed or waits. Every other entry is a run of its own.
List<List<ChatMessage>> toolRuns(Iterable<ChatMessage> entries) {
  final runs = <List<ChatMessage>>[];
  for (final entry in entries) {
    if (entry.role == MessageRole.tool &&
        runs.isNotEmpty &&
        runs.last.last.role == MessageRole.tool) {
      runs.last.add(entry);
    } else {
      runs.add([entry]);
    }
  }
  return runs;
}
