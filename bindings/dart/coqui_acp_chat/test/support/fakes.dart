// What the tests share: a connection in memory that a test drives and reads, and the updates agents send.
import 'dart:async';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';

/// A [ChatConnection] a test drives: it sends what an agent would, and records what the chat asked of the agent.
final class FakeConnection extends ChatConnection {
  FakeConnection({this.version = 2, this.usage = false});

  final int version;
  final _updates = StreamController<AcpSessionUpdate>.broadcast();
  final _events = StreamController<AcpEvent>.broadcast();
  final _notices = StreamController<UsageNotice>.broadcast();
  bool closed = false;

  /// Every prompt sent, as its text.
  final prompts = <String>[];
  final attachmentNames = <List<String>>[];
  final cancels = <String>[];
  final modes = <String>[];
  final configs = <(String, String)>[];

  /// Every request in the order it was made, such as `prompt:hi`, `mode:auto`, `config:model=b` and `cancel`.
  final calls = <String>[];

  /// Answers a prompt. The default answers a v2 message id at once; a v1 test gives a Completer's future to hold the turn.
  Future<String?> Function(String text)? onPrompt;
  Future<void> Function(String sessionId)? onCancel;
  Future<void> Function(String modeId)? onSetMode;
  Future<Map<String, dynamic>> Function(String id, String value)? onSetConfig;
  UsageReport? report;
  final bool usage;
  int fetches = 0;

  @override
  Stream<AcpSessionUpdate> get updates => _updates.stream;
  @override
  Stream<AcpEvent> get events => _events.stream;
  @override
  bool get isClosed => closed;
  @override
  int? get protocolVersion => version;

  @override
  Future<String?> prompt(
    String sessionId,
    String text, {
    List<AcpAttachment> attachments = const [],
  }) {
    prompts.add(text);
    calls.add('prompt:$text');
    attachmentNames.add([for (final item in attachments) item.name]);
    // A v1 prompt answers when its turn ends, which a test says by giving onPrompt a future of its own.
    return onPrompt?.call(text) ??
        (version == 1
            ? Completer<String?>().future
            : Future.value('m${prompts.length}'));
  }

  @override
  Future<void> cancel(String sessionId) {
    cancels.add(sessionId);
    calls.add('cancel');
    return onCancel?.call(sessionId) ?? Future.value();
  }

  @override
  Future<void> setMode(String sessionId, String modeId) {
    modes.add(modeId);
    calls.add('mode:$modeId');
    return onSetMode?.call(modeId) ?? Future.value();
  }

  @override
  Future<Map<String, dynamic>> setConfigOption(
    String sessionId,
    String configId,
    String value,
  ) {
    configs.add((configId, value));
    calls.add('config:$configId=$value');
    return onSetConfig?.call(configId, value) ?? Future.value({});
  }

  @override
  bool get reportsUsage => usage;
  @override
  Stream<UsageNotice> get usageNotices => _notices.stream;
  @override
  Future<UsageReport?> fetchUsage({String? sessionId}) async {
    fetches++;
    return report;
  }

  @override
  Future<void> close() async {
    closed = true;
    await _updates.close();
    await _events.close();
    await _notices.close();
  }

  /// Ends the connection as a dropped channel does: its updates end, and it is closed.
  Future<void> drop() => close();

  /// An update for [session], delivered after the microtasks it is queued behind.
  void update(Map<String, Object?> raw, {String session = 's1'}) =>
      _updates.add(AcpSessionUpdate(session, Map<String, dynamic>.of(raw)));

  void event(AcpEvent event) => _events.add(event);

  void notice(UsageReport report, {String? session}) =>
      _notices.add((sessionId: session, report: report));
}

/// Lets what an agent sent reach the chat.
Future<void> settle() => Future<void>.delayed(const Duration(milliseconds: 10));

/// A session in a store, attached to a fresh connection, as a host would make it.
({ChatStore store, ChatSession session, FakeConnection agent}) started({
  int version = 2,
  ChatLimits limits = const ChatLimits(),
  UpdateRecorder? recorder,
  String? hostNoticesKey,
  DateTime Function()? clock,
  FakeConnection? connection,
}) {
  final store = ChatStore(
    limits: limits,
    recorder: recorder,
    hostNoticesKey: hostNoticesKey,
    clock: clock,
  );
  final agent = connection ?? FakeConnection(version: version);
  final session = store.add(
    title: 'Scripted',
    agent: 'Scripted',
    peerId: 'peer-a',
    sessionId: 's1',
  );
  session.attach(agent);
  return (store: store, session: session, agent: agent);
}

const allowOnce = AcpPermissionOption(
  optionId: 'allow-once',
  name: 'Yes',
  kind: 'allow_once',
);
const allowAlways = AcpPermissionOption(
  optionId: 'allow-always',
  name: 'Always',
  kind: 'allow_always',
);
const rejectOnce = AcpPermissionOption(
  optionId: 'reject',
  name: 'No',
  kind: 'reject_once',
);
const rejectAlways = AcpPermissionOption(
  optionId: 'reject-always',
  name: 'No, never',
  kind: 'reject_always',
);

/// A request for an edit of `a.txt`, as `t<n>`, with the answers the agent offers.
AcpPermissionRequest request(
  int n, {
  List<AcpPermissionOption> options = const [allowOnce, rejectOnce],
  String session = 's1',
  Object? rawInput = const {'file_path': 'a.txt'},
  String kind = 'edit',
}) => AcpPermissionRequest(
  sessionId: session,
  toolCallId: 't$n',
  title: 'Edit a.txt',
  kind: kind,
  rawInput: rawInput,
  toolCall: {
    'toolCallId': 't$n',
    'title': 'Edit a.txt',
    'kind': 'edit',
    'status': 'pending',
  },
  options: options,
);

Map<String, Object?> text(String kind, String text, {String? id}) => {
  'sessionUpdate': kind,
  'messageId': ?id,
  'content': {'type': 'text', 'text': text},
};

Map<String, Object?> state(String state, {String? reason}) => {
  'sessionUpdate': 'state_update',
  'state': state,
  'stopReason': ?reason,
};

/// Applies one update to [session] directly, which is what its connection's stream does.
void feed(ChatSession session, Map<String, Object?> raw) =>
    session.apply(AcpSessionUpdate('s1', Map<String, dynamic>.of(raw)));

/// [text] with the id it comes under, for a closure that has a parameter of its own called `text`.
Map<String, Object?> text0(String kind, String body, String id) =>
    text(kind, body, id: id);
