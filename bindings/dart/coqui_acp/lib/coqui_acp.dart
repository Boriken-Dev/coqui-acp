import 'dart:async';
import 'dart:convert';
import 'dart:math';

/// What `coqui-acp-bridge serve --linger` sends when it dropped output while nobody was connected.
const acpOutputLost = '_dev.boriken.coqui.acp/output_lost';

/// The `_meta` key of an agent's capabilities with which it says it answers [acpSessionOptionsMethod].
const acpSessionOptionsCapability = 'dev.boriken.coqui.acp/session_options';

/// The request for what a session can be started with, before any session: `{}`, answered
/// `{"configOptions": [SessionConfigOption], "modes": SessionModeState, "unknown": {part: reason}}`, where a part the agent cannot
/// tell is left out and may say why under `unknown`.
const acpSessionOptionsMethod = '_coqui_acp/session_options';

/// One part of what an agent offers: [AcpKnown], [AcpUnknown] or [AcpNotSupported].
sealed class AcpAnswer<T> {
  const AcpAnswer();
}

/// What the agent said.
final class AcpKnown<T> extends AcpAnswer<T> {
  const AcpKnown(this.value);
  final T value;
  @override
  String toString() => 'AcpKnown($value)';
}

/// The agent offers the question and could not tell, for this reason, in its words.
final class AcpUnknown<T> extends AcpAnswer<T> {
  const AcpUnknown(this.reason);
  final String reason;
  @override
  String toString() => 'AcpUnknown($reason)';
}

/// The agent does not offer the question, so it was not asked.
final class AcpNotSupported<T> extends AcpAnswer<T> {
  const AcpNotSupported();
  @override
  String toString() => 'AcpNotSupported()';
}

/// What an agent offers before any session; see [AcpClient.describe]. All of it is the agent's to say, read as ACP's own JSON
/// and bounded.
final class AcpDescription {
  const AcpDescription({
    required this.options,
    required this.modes,
    required this.sessions,
  });

  /// The options a session can start with, each ACP's `SessionConfigOption` of the version spoken (`id` in v1, `configId` in v2),
  /// at the value a session starts with. A model is the option of category `model`, an effort the one of category `thought_level`.
  final AcpAnswer<List<Map<String, Object?>>> options;

  /// The modes a session can start in, ACP's v1 `SessionModeState`. v2 has none, so it is not supported there.
  final AcpAnswer<Map<String, Object?>> modes;

  /// The sessions the agent lists: `session/list`'s first page.
  final AcpAnswer<List<AcpSessionInfo>> sessions;
}

sealed class AcpEvent {
  const AcpEvent();
}

final class AcpText extends AcpEvent {
  final String text;
  const AcpText(this.text);
  @override
  String toString() => 'AcpText($text)';
}

final class AcpTool extends AcpEvent {
  final String id;
  final String title;
  final String kind;
  const AcpTool({required this.id, required this.title, required this.kind});
  @override
  String toString() => 'AcpTool(id: $id, title: $title, kind: $kind)';
}

final class AcpCost extends AcpEvent {
  final double amount;
  final String currency;
  const AcpCost({required this.amount, required this.currency});
  @override
  String toString() => 'AcpCost(amount: $amount, currency: $currency)';
}

final class AcpPermission extends AcpEvent {
  final String tool;
  final bool allowed;
  final String? reason;
  const AcpPermission({required this.tool, required this.allowed, this.reason});
  @override
  String toString() =>
      'AcpPermission(tool: $tool, allowed: $allowed, reason: $reason)';
}

final class AcpCompleted extends AcpEvent {
  final String stop;
  const AcpCompleted(this.stop);
  @override
  String toString() => 'AcpCompleted($stop)';
}

final class AcpFailed extends AcpEvent {
  final String message;
  const AcpFailed(this.message);
  @override
  String toString() => 'AcpFailed($message)';
}

/// The agent ended the turn as cancelled, whether asked to or by itself.
final class AcpCancelled extends AcpEvent {
  const AcpCancelled();
  @override
  String toString() => 'AcpCancelled()';
}

/// A permission request could not be answered, so the agent was told it was cancelled. The turn goes on until the agent ends it.
final class AcpBlocked extends AcpEvent {
  final String message;
  const AcpBlocked(this.message);
  @override
  String toString() => 'AcpBlocked($message)';
}

/// Output of the agent's was dropped before it reached this connection: `_dev.boriken.coqui.acp/output_lost`, which a bridge that kept
/// the agent while nobody was connected sends first on the next connection. What the session shows has a gap there.
final class AcpOutputLost extends AcpEvent {
  /// Messages dropped.
  final int lines;

  /// Their bytes.
  final int bytes;
  const AcpOutputLost({required this.lines, required this.bytes});
  @override
  String toString() => 'AcpOutputLost(lines: $lines, bytes: $bytes)';
}

final class AcpNotice extends AcpEvent {
  final String message;
  const AcpNotice(this.message);
  @override
  String toString() => 'AcpNotice($message)';
}

class AcpFailure implements Exception {
  const AcpFailure(this.message, {this.code});
  final String message;

  /// The JSON-RPC error code the agent answered with, when this is its error.
  final int? code;
  @override
  String toString() => 'ACP: $message';
}

sealed class AcpDecision {
  const AcpDecision();
}

final class AcpAllow extends AcpDecision {
  const AcpAllow();
  @override
  String toString() => 'AcpAllow()';
}

final class AcpDeny extends AcpDecision {
  final String reason;
  const AcpDeny(this.reason);
  @override
  String toString() => 'AcpDeny($reason)';
}

/// The turn was cancelled while this was waiting. ACP requires this answer, rather than a refusal, for a request still open then.
final class AcpCancel extends AcpDecision {
  const AcpCancel();
  @override
  String toString() => 'AcpCancel()';
}

final class AcpPermissionRequest {
  final String sessionId;
  final String toolCallId;
  final String? title;
  final String? kind;
  final Object? rawInput;

  /// The tool call as the agent sent it, which a client applies to what it shows before asking.
  final Map<String, dynamic>? toolCall;

  /// Why the agent says it needs this, in v2.
  final String? description;
  const AcpPermissionRequest({
    required this.sessionId,
    required this.toolCallId,
    this.title,
    this.kind,
    this.rawInput,
    this.toolCall,
    this.description,
  });
}

typedef AcpDecider = Future<AcpDecision> Function(AcpPermissionRequest request);

/// A session the agent opened, and what it said about it.
final class AcpOpenedSession {
  AcpOpenedSession(this.id, this.raw, {this.replayed = false});
  final String id;
  final Map<String, dynamic> raw;

  /// Whether the agent was asked to send the conversation so far, which arrives as updates before this does.
  final bool replayed;
}

/// A session an agent says it has. All of it is the agent's to say.
final class AcpSessionInfo {
  const AcpSessionInfo({
    required this.id,
    this.cwd,
    this.title,
    this.updatedAt,
  });
  final String id;
  final String? cwd;
  final String? title;
  final String? updatedAt;
}

/// One page of what an agent lists. Ask again with [nextCursor] for more.
final class AcpSessionPage {
  const AcpSessionPage(this.sessions, this.nextCursor);
  final List<AcpSessionInfo> sessions;
  final String? nextCursor;
}

/// Something sent with a prompt. An agent says what it takes: see [AcpClient.takesImages] and [AcpClient.takesFiles].
final class AcpAttachment {
  AcpAttachment._(this.name, this.isImage, this._block);

  /// An image, sent whole.
  factory AcpAttachment.image(String name, String mimeType, List<int> bytes) =>
      AcpAttachment._(name, true, {
        'type': 'image',
        'mimeType': mimeType,
        'data': base64Encode(bytes),
      });

  /// A text file's content, sent whole. Its address names no path of this machine, which the agent has no use for.
  factory AcpAttachment.text(String name, String text, {String? mimeType}) =>
      AcpAttachment._(name, false, {
        'type': 'resource',
        'resource': {
          'uri': 'attachment:${Uri.encodeComponent(name)}',
          'text': text,
          'mimeType': ?mimeType,
        },
      });

  final String name;
  final bool isImage;
  final Map<String, Object?> _block;
}

final class AcpSessionUpdate {
  AcpSessionUpdate(this.sessionId, this.raw);
  final String sessionId;
  final Map<String, dynamic> raw;
  String get kind =>
      raw['sessionUpdate'] is String ? raw['sessionUpdate'] as String : '';
  String? get messageId =>
      raw['messageId'] is String ? raw['messageId'] as String : null;
  String? get state => raw['state'] is String ? raw['state'] as String : null;
  String? get reason =>
      raw['stopReason'] is String ? raw['stopReason'] as String : null;
  String? get text {
    final value = raw['content'];
    if (value is Map && value['type'] == 'text' && value['text'] is String) {
      return value['text'] as String;
    }
    if (value is List) {
      return value
          .whereType<Map>()
          .where((entry) => entry['type'] == 'text')
          .map((entry) => entry['text'])
          .whereType<String>()
          .join();
    }
    return null;
  }
}

/// A JSON-RPC ACP client over any stream of whole messages. It authenticates nobody: give it a channel only after
/// the peer is proven. It speaks whichever of v1 and v2 the agent selects, and never retries anything unacknowledged.
final class AcpClient {
  AcpClient({
    required Stream<String> incoming,
    required Future<void> Function(String) send,
    Future<void> Function()? closeSource,
    AcpDecider? decider,
    this.maxProtocolVersion = 2,
    this.clientName = 'coqui-acp',
    this.clientVersion = '0.0.0',
    this.requestTimeout = const Duration(seconds: 20),
  }) : _send = send,
       _closeSource = closeSource,
       _decider = decider {
    _subscription = incoming.listen(
      _receive,
      onError: _fail,
      onDone: () =>
          _fail(const AcpFailure('Connection lost; pending outcomes unknown')),
    );
  }

  /// As many of a page of sessions as this takes: an agent chooses how many it sends.
  static const maxListedSessions = 500;

  final int maxProtocolVersion;
  final String clientName;
  final String clientVersion;
  final Duration requestTimeout;
  final Future<void> Function(String) _send;
  final Future<void> Function()? _closeSource;
  final AcpDecider? _decider;
  final Map<int, Completer<Map<String, dynamic>>> _pending = {};
  final _events = StreamController<AcpEvent>.broadcast(sync: true);
  final _updates = StreamController<AcpSessionUpdate>.broadcast(sync: true);
  final StringBuffer _text = StringBuffer();
  late final StreamSubscription<String> _subscription;

  /// Ids start at a random point, so those of a new connection to an agent that outlived the last one are not the last one's, whose
  /// late answers would be taken for theirs. The product stays below 2^53, which a JavaScript number holds exactly.
  int _counter = Random.secure().nextInt(0x7fffffff) * 0x200000;

  /// The sessions this client opened or used. An update or a request for any other, such as one an earlier connection left running
  /// on the same agent, is not this client's: it is dropped, and a request is cancelled without asking the decider.
  final Set<String> _sessions = {};
  bool _closed = false;
  int? _protocolVersion;

  /// One normalized stream, whichever version carried it. Text gathers until something follows it.
  Stream<AcpEvent> get events => _events.stream;

  /// Every `session/update` as it arrives, for a UI that shows text while it streams.
  Stream<AcpSessionUpdate> get updates => _updates.stream;

  /// The version the agent selected; null before [initialize].
  int? get protocolVersion => _protocolVersion;
  bool get isClosed => _closed;

  void _fail(Object error, [StackTrace? trace]) {
    if (_closed) return;
    _closed = true;
    for (final completer in _pending.values) {
      if (!completer.isCompleted) completer.completeError(error, trace);
    }
    _pending.clear();
    unawaited(_events.close());
    unawaited(_updates.close());
    final closeSource = _closeSource;
    if (closeSource != null) unawaited(closeSource().catchError((Object _) {}));
  }

  void _emit(AcpEvent event) {
    if (!_closed) _events.add(event);
  }

  void _flush() {
    if (_text.isEmpty) return;
    _emit(AcpText(_text.toString()));
    _text.clear();
  }

  void _reply(Object? id, Map<String, Object?> body) {
    if (_closed) return;
    unawaited(
      _send(
        jsonEncode({'jsonrpc': '2.0', 'id': id, ...body}),
      ).catchError((Object _) {}),
    );
  }

  void _receive(String message) {
    if (_closed) return;
    try {
      final envelope = jsonDecode(message);
      if (envelope is! Map<String, dynamic> || envelope['jsonrpc'] != '2.0') {
        throw const FormatException('Not JSON-RPC 2.0');
      }
      final method = envelope['method'];
      final id = envelope['id'];
      if (method is String) {
        if (id == null) {
          if (method == 'session/update') {
            _receiveUpdate(envelope['params']);
          } else if (method == acpOutputLost) {
            _receiveOutputLost(envelope['params']);
          }
        } else if (method == 'session/request_permission') {
          unawaited(_answerPermission(id, envelope['params']));
        } else {
          _reply(id, {
            'error': {'code': -32601, 'message': 'Unsupported method'},
          });
        }
        return;
      }
      if (id is! int) return;
      final completer = _pending.remove(id);
      if (completer == null || completer.isCompleted) return;
      if (envelope['error'] is Map) {
        final error = envelope['error'] as Map;
        completer.completeError(
          AcpFailure(
            'Agent error: $error',
            code: error['code'] is int ? error['code'] as int : null,
          ),
        );
      } else if (envelope['result'] is Map) {
        completer.complete(
          Map<String, dynamic>.from(envelope['result'] as Map),
        );
      } else {
        completer.completeError(const AcpFailure('Malformed agent response'));
      }
    } on FormatException catch (error) {
      _fail(AcpFailure('Invalid agent message: $error'));
    } on TypeError catch (error) {
      _fail(AcpFailure('Invalid message fields: $error'));
    }
  }

  /// An agent may be newer than this client, so what cannot be read is ignored, never fatal.
  void _receiveUpdate(Object? params) {
    if (params is! Map ||
        params['sessionId'] is! String ||
        params['update'] is! Map) {
      return;
    }
    final update = AcpSessionUpdate(
      params['sessionId'] as String,
      Map<String, dynamic>.from(params['update'] as Map),
    );
    if (!_sessions.contains(update.sessionId)) return;
    if (!_closed) _updates.add(update);
    final raw = update.raw;
    switch (raw['sessionUpdate']) {
      case 'agent_message_chunk':
        final content = raw['content'];
        if (content is Map &&
            content['type'] == 'text' &&
            content['text'] is String) {
          _text.write(content['text'] as String);
        }
      case 'tool_call':
        final id = raw['toolCallId'];
        final title = raw['title'];
        if (id is String && title is String) {
          _flush();
          final kind = raw['kind'];
          _emit(
            AcpTool(
              id: id,
              title: title,
              kind: kind is String ? kind : 'other',
            ),
          );
        }
      case 'usage_update':
        final cost = raw['cost'];
        if (cost is Map &&
            cost['amount'] is num &&
            cost['currency'] is String) {
          _flush();
          _emit(
            AcpCost(
              amount: (cost['amount'] as num).toDouble(),
              currency: cost['currency'] as String,
            ),
          );
        }
      case 'state_update':
        if (raw['state'] == 'idle') {
          final stop = raw['stopReason'];
          _endTurn(stop is String ? stop : 'end_turn');
        }
    }
  }

  void _receiveOutputLost(Object? params) {
    int count(String key) =>
        params is Map && params[key] is int ? params[key] as int : 0;
    _flush();
    _emit(AcpOutputLost(lines: count('lines'), bytes: count('bytes')));
  }

  void _endTurn(String stop) {
    _flush();
    _emit(switch (stop) {
      'end_turn' || 'max_tokens' || 'max_turn_requests' => AcpCompleted(stop),
      'cancelled' => const AcpCancelled(),
      'refusal' => const AcpFailed('the agent refused to continue'),
      _ => AcpFailed('the agent ended the turn with $stop'),
    });
  }

  Future<void> _answerPermission(Object id, Object? params) async {
    // Another session's, such as one an earlier connection left waiting: the decider answers for this client's sessions only.
    if (params is Map &&
        params['sessionId'] is String &&
        !_sessions.contains(params['sessionId'])) {
      _reply(id, {
        'result': {
          'outcome': {'outcome': 'cancelled'},
        },
      });
      return;
    }
    // An agent has been seen to go ahead with the tool on an error answer, so what cannot be asked is cancelled, and said.
    if (params is! Map || params['options'] is! List) {
      _emit(
        const AcpBlocked(
          'the agent asked leave in a form this client cannot read',
        ),
      );
      _reply(id, {
        'result': {
          'outcome': {'outcome': 'cancelled'},
        },
      });
      return;
    }
    // v1 names the tool call directly. v2 has its own title for the prompt, and a subject that is a tool call or a command.
    final subject = params['subject'] is Map
        ? params['subject'] as Map
        : const {};
    final call = params['toolCall'] is Map
        ? params['toolCall'] as Map
        : subject['toolCall'] is Map
        ? subject['toolCall'] as Map
        : const {};
    final title = params['title'] is String ? params['title'] : call['title'];
    final tool = title is String ? title : 'a tool';
    AcpDecision decision;
    final decider = _decider;
    if (decider == null) {
      decision = const AcpDeny('no permission decider is configured');
    } else {
      try {
        decision = await decider(
          AcpPermissionRequest(
            sessionId: params['sessionId'] is String
                ? params['sessionId'] as String
                : '',
            toolCallId: call['toolCallId'] is String
                ? call['toolCallId'] as String
                : '',
            title: title is String ? title : null,
            kind: call['kind'] is String ? call['kind'] as String : null,
            rawInput:
                call['rawInput'] ??
                (subject['command'] is String
                    ? {'command': subject['command']}
                    : null),
            toolCall: call.isEmpty ? null : Map<String, dynamic>.from(call),
            description: params['description'] is String
                ? params['description'] as String
                : null,
          ),
        );
      } catch (error) {
        decision = AcpDeny('the decider failed: $error');
      }
    }
    if (decision is AcpCancel) {
      _flush();
      _emit(
        AcpPermission(
          tool: tool,
          allowed: false,
          reason: 'the turn was cancelled',
        ),
      );
      _reply(id, {
        'result': {
          'outcome': {'outcome': 'cancelled'},
        },
      });
      return;
    }
    final wanted = decision is AcpAllow ? 'allow_once' : 'reject_once';
    _flush();
    _emit(
      AcpPermission(
        tool: tool,
        allowed: decision is AcpAllow,
        reason: decision is AcpDeny ? decision.reason : null,
      ),
    );
    for (final option in params['options'] as List) {
      if (option is Map &&
          option['kind'] == wanted &&
          option['optionId'] is String) {
        _reply(id, {
          'result': {
            'outcome': {'outcome': 'selected', 'optionId': option['optionId']},
          },
        });
        return;
      }
    }
    _emit(
      AcpBlocked(
        'the agent asked leave for $tool and offered no $wanted option',
      ),
    );
    _reply(id, {
      'result': {
        'outcome': {'outcome': 'cancelled'},
      },
    });
  }

  Future<void> initialize() async {
    if (_protocolVersion != null) return;
    final reply = await _request('initialize', {
      'protocolVersion': maxProtocolVersion,
      if (maxProtocolVersion >= 2) ...{
        'capabilities': <String, Object?>{},
        'info': {
          'name': clientName,
          'title': clientName,
          'version': clientVersion,
        },
      } else ...{
        'clientCapabilities': {
          'fs': {'readTextFile': false, 'writeTextFile': false},
          'terminal': false,
        },
        'clientInfo': {'name': clientName, 'version': clientVersion},
      },
    });
    final version = reply['protocolVersion'];
    if (version == 2) {
      final capabilities = reply['capabilities'];
      if (capabilities is! Map || capabilities['session'] is! Map) {
        throw const AcpFailure('Agent does not support ACP v2 sessions');
      }
    } else if (version != 1) {
      throw const AcpFailure(
        'The agent selected a protocol version this client lacks',
      );
    }
    _protocolVersion = version as int;
    final capabilities =
        reply[version == 2 ? 'capabilities' : 'agentCapabilities'];
    _capabilities = capabilities is Map ? capabilities : const {};
  }

  Map<dynamic, dynamic> _capabilities = const {};

  /// A capability under [path], which v1 says with `true` or an object and v2 with an object. Absent is no.
  bool _says(List<String> path) {
    Object? at = _capabilities;
    for (final step in path) {
      if (at is! Map) return false;
      at = at[step];
    }
    return at == true || at is Map;
  }

  /// Whether this agent takes an image with a prompt.
  bool get takesImages => _says(
    _protocolVersion == 2
        ? const ['session', 'prompt', 'image']
        : const ['promptCapabilities', 'image'],
  );

  /// Whether this agent takes a file's content with a prompt.
  bool get takesFiles => _says(
    _protocolVersion == 2
        ? const ['session', 'prompt', 'embeddedContext']
        : const ['promptCapabilities', 'embeddedContext'],
  );

  /// Whether an earlier session can be opened again. It is part of v2, and each of its two forms is optional in v1.
  bool get canReopenSessions =>
      _protocolVersion == 2 ||
      _says(const ['loadSession']) ||
      _says(const ['sessionCapabilities', 'resume']);

  /// The sessions an agent says it has, in [cwd] or everywhere. v1 must offer this to be asked, and a v2 agent can say so only by
  /// answering: a real one lists without declaring it, and one that does not fails this with [AcpFailure].
  Future<AcpSessionPage> listSessions({String? cwd, String? cursor}) async {
    _ready();
    if (_protocolVersion == 1 &&
        !_says(const ['sessionCapabilities', 'list'])) {
      throw const AcpFailure('This agent does not list its sessions');
    }
    final reply = await _request('session/list', {
      'cwd': ?cwd,
      'cursor': ?cursor,
    });
    final listed = reply['sessions'];
    final sessions = <AcpSessionInfo>[];
    if (listed is List) {
      for (final item in listed.take(maxListedSessions)) {
        if (item is! Map ||
            item['sessionId'] is! String ||
            (item['sessionId'] as String).isEmpty) {
          continue;
        }
        String? text(String key) =>
            item[key] is String ? item[key] as String : null;
        sessions.add(
          AcpSessionInfo(
            id: item['sessionId'] as String,
            cwd: text('cwd'),
            title: text('title'),
            updatedAt: text('updatedAt'),
          ),
        );
      }
    }
    final next = reply['nextCursor'];
    return AcpSessionPage(
      sessions,
      next is String && next.isNotEmpty ? next : null,
    );
  }

  /// Opens an earlier session, on this connection, with its conversation replayed as updates where the agent can. Listen to
  /// [updates] first. Nothing bounds how long a replay takes, so this has no deadline.
  Future<AcpOpenedSession> reopenSession(
    String id, {
    required String cwd,
  }) async {
    _ready();
    _checkCwd(cwd);
    // Before asking, since the replay arrives before the answer.
    return _using(id, () => _reopen(id, cwd));
  }

  /// Runs [opening] with [id] taken as this client's, which it stays only if [opening] succeeds and it was not already.
  Future<T> _using<T>(String id, Future<T> Function() opening) async {
    final added = _sessions.add(id);
    try {
      return await opening();
    } catch (_) {
      if (added) _sessions.remove(id);
      rethrow;
    }
  }

  Future<AcpOpenedSession> _reopen(String id, String cwd) async {
    final params = {'sessionId': id, 'cwd': cwd, 'mcpServers': <Object>[]};
    if (_protocolVersion == 2) {
      final reply = await _request('session/resume', {
        ...params,
        'replayFrom': {'type': 'start'},
      }, limited: false);
      return AcpOpenedSession(id, reply, replayed: true);
    }
    if (_says(const ['loadSession'])) {
      return AcpOpenedSession(
        id,
        await _request('session/load', params, limited: false),
        replayed: true,
      );
    }
    if (_says(const ['sessionCapabilities', 'resume'])) {
      return AcpOpenedSession(
        id,
        await _request('session/resume', params, limited: false),
      );
    }
    throw const AcpFailure('This agent cannot open an earlier session');
  }

  void _ready() {
    if (_protocolVersion == null) {
      throw StateError('Initialize ACP before session operations');
    }
  }

  void _checkCwd(String cwd) {
    if (!(cwd.startsWith('/') || RegExp(r'^[a-zA-Z]:[\\/]').hasMatch(cwd))) {
      throw ArgumentError.value(cwd, 'cwd', 'Must be an absolute path');
    }
  }

  /// What the agent offers before any session, asked without opening one: the options a session can start with and its modes,
  /// by [acpSessionOptionsMethod] when the agent's capabilities advertise [acpSessionOptionsCapability] in `_meta`, and its
  /// sessions, by `session/list` when v1's `sessionCapabilities.list` is set, or always in v2, where a method-not-found answer
  /// means not supported. What it does not advertise is [AcpNotSupported] and nothing is sent for it; a call that fails or
  /// times out is [AcpUnknown]. Call it after [initialize].
  Future<AcpDescription> describe() async {
    _ready();
    final v2 = _protocolVersion == 2;
    final meta = _capabilities['_meta'];
    final extension = meta is Map ? meta[acpSessionOptionsCapability] : null;
    AcpAnswer<List<Map<String, Object?>>> options = const AcpNotSupported();
    AcpAnswer<Map<String, Object?>> modes = const AcpNotSupported();
    if (extension != null && extension != false) {
      try {
        final answer = await _request(acpSessionOptionsMethod, {});
        options = _describedOptions(answer, v2 ? 'configId' : 'id');
        if (!v2) modes = _describedModes(answer);
      } on AcpFailure catch (error) {
        final why = _cut('the agent could not say: ${error.message}');
        options = AcpUnknown(why);
        if (!v2) modes = AcpUnknown(why);
      }
    }
    final session = _capabilities['sessionCapabilities'];
    AcpAnswer<List<AcpSessionInfo>> sessions = const AcpNotSupported();
    if (v2 || (session is Map && session['list'] is Map)) {
      try {
        sessions = _describedSessions(await _request('session/list', {}));
      } on AcpFailure catch (error) {
        sessions = v2 && error.code == -32601
            ? const AcpNotSupported()
            : AcpUnknown(_cut('the agent could not say: ${error.message}'));
      }
    }
    return AcpDescription(options: options, modes: modes, sessions: sessions);
  }

  Future<String> createSession({required String cwd}) =>
      openSession(cwd: cwd).then((session) => session.id);

  /// As [createSession], with what the agent said about the session. It is the agent's to say: an agent may send neither.
  Future<AcpOpenedSession> openSession({required String cwd}) {
    _ready();
    _checkCwd(cwd);
    return _request('session/new', {'cwd': cwd, 'mcpServers': <Object>[]}).then(
      (reply) {
        final id = reply['sessionId'];
        if (id is! String || id.isEmpty) {
          throw const AcpFailure('Missing session ID');
        }
        _sessions.add(id);
        return AcpOpenedSession(id, reply);
      },
    );
  }

  /// Sets one of the agent's `configOptions`, such as the model. The answer can change others, so read `configOptions` from it.
  Future<Map<String, dynamic>> setConfigOption(
    String sessionId,
    String configId,
    String value,
  ) {
    _ready();
    return _request('session/set_config_option', {
      'sessionId': sessionId,
      'configId': configId,
      'value': value,
    });
  }

  /// Sets the agent's mode, for an agent that offers `modes` rather than `configOptions`.
  Future<void> setMode(String sessionId, String modeId) {
    _ready();
    return _request('session/set_mode', {
      'sessionId': sessionId,
      'modeId': modeId,
    });
  }

  Future<void> resumeSession(
    String id, {
    required String cwd,
    bool replayHistory = false,
  }) {
    _ready();
    _checkCwd(cwd);
    return _using(
      id,
      () => _request('session/resume', {
        'sessionId': id,
        'cwd': cwd,
        'mcpServers': <Object>[],
        if (replayHistory) 'replayFrom': {'type': 'start'},
      }),
    );
  }

  /// Sends a turn. In v2 it answers the inserted message's id as soon as the agent acknowledges, and the turn ends by a
  /// later event. In v1 the answer is the end of the turn, so this has no deadline and answers null.
  ///
  /// [attachments] go with it, and one this agent does not take is refused before anything is sent, since an agent might otherwise
  /// answer as though it had read what never reached it.
  Future<String?> prompt(
    String sessionId,
    String text, {
    List<AcpAttachment> attachments = const [],
  }) async {
    _ready();
    if (text.trim().isEmpty && attachments.isEmpty) {
      throw ArgumentError.value(text, 'text');
    }
    for (final attachment in attachments) {
      if (attachment.isImage ? !takesImages : !takesFiles) {
        throw AcpFailure(
          'This agent does not take ${attachment.isImage ? 'images' : 'files'}',
        );
      }
    }
    _sessions.add(sessionId);
    final params = {
      'sessionId': sessionId,
      'prompt': [
        if (text.trim().isNotEmpty) {'type': 'text', 'text': text},
        for (final attachment in attachments) attachment._block,
      ],
    };
    if (_protocolVersion == 1) {
      try {
        final reply = await _request('session/prompt', params, limited: false);
        final stop = reply['stopReason'];
        _endTurn(stop is String ? stop : 'none');
      } on AcpFailure catch (error) {
        if (!_closed) {
          _flush();
          _emit(AcpFailed(error.message));
        }
      }
      return null;
    }
    final reply = await _request('session/prompt', params);
    final id = reply['messageId'];
    if (id is! String || id.isEmpty) {
      throw const AcpFailure('Missing inserted message ID');
    }
    return id;
  }

  Future<void> cancel(String sessionId) async {
    _ready();
    if (_closed) throw const AcpFailure('Connection closed');
    await _send(
      jsonEncode({
        'jsonrpc': '2.0',
        'method': 'session/cancel',
        'params': {'sessionId': sessionId},
      }),
    );
  }

  Future<void> closeSession(String sessionId) async {
    _ready();
    await _request('session/close', {'sessionId': sessionId});
    _sessions.remove(sessionId);
  }

  Future<void> close() async {
    _fail(const AcpFailure('ACP client closed'));
    await _subscription.cancel();
  }

  Future<Map<String, dynamic>> _request(
    String method,
    Map<String, Object?> params, {
    bool limited = true,
  }) async {
    if (_closed) throw const AcpFailure('Connection closed');
    final id = ++_counter;
    final pending = Completer<Map<String, dynamic>>();
    _pending[id] = pending;
    try {
      await _send(
        jsonEncode({
          'jsonrpc': '2.0',
          'id': id,
          'method': method,
          'params': params,
        }),
      ).timeout(requestTimeout);
      return await (limited
          ? pending.future.timeout(requestTimeout)
          : pending.future);
    } on TimeoutException {
      throw AcpFailure('$method: no answer in time');
    } finally {
      _pending.remove(id);
    }
  }
}

// What [AcpClient.describe] keeps of an agent's answer, which is untrusted: what cannot be read as ACP's own types is skipped,
// lists are cut at their limit, an item whose identifier is longer than [_maxText] is dropped, since it could not be named back,
// and other text is cut there. The Rust client keeps the same, as vectors/describe.json says.
const _maxOptions = 64;
const _maxValues = 1024;
const _maxModes = 64;
const _maxSessions = 500;

/// The longest text kept, in UTF-8 bytes.
const _maxText = 4096;

String _cut(String text) {
  final bytes = utf8.encode(text);
  if (bytes.length <= _maxText) return text;
  var end = _maxText;
  while (end > 0 && (bytes[end] & 0xC0) == 0x80) {
    end--;
  }
  return utf8.decode(bytes.sublist(0, end));
}

bool _fits(Object? id) =>
    id is String && id.isNotEmpty && utf8.encode(id).length <= _maxText;

/// [into]'s [key] set from [from]'s, cut, when it is text; anything else is left out.
void _text(Map<String, Object?> into, Map from, String key) {
  final value = from[key];
  if (value is String) into[key] = _cut(value);
}

/// A select's value, or null when it is not one.
Map<String, Object?>? _value(Object? item) {
  if (item is! Map || item['value'] is! String || item['name'] is! String) {
    return null;
  }
  return {'value': item['value'], 'name': _cut(item['name'] as String)}
    ..addAll({
      if (item['description'] is String)
        'description': _cut(item['description'] as String),
    });
}

/// A select's options, flat or grouped, bounded; null when they are neither.
List<Map<String, Object?>>? _values(Object? options) {
  if (options is! List) return null;
  final flat = options.map(_value).toList();
  if (!flat.contains(null)) {
    return flat
        .cast<Map<String, Object?>>()
        .where((value) => _fits(value['value']))
        .take(_maxValues)
        .toList();
  }
  final groups = <Map<String, Object?>>[];
  var left = _maxValues;
  for (final group in options) {
    if (group is! Map ||
        group['group'] is! String ||
        group['name'] is! String ||
        !group.containsKey('options')) {
      return null;
    }
    if (!_fits(group['group'])) continue;
    final inside = group['options'] is List
        ? group['options'] as List
        : const [];
    final values = inside
        .map(_value)
        .whereType<Map<String, Object?>>()
        .where((value) => _fits(value['value']))
        .take(left)
        .toList();
    left -= values.length;
    groups.add({
      'group': group['group'],
      'name': _cut(group['name'] as String),
      'options': values,
    });
  }
  return groups;
}

/// One option, read as ACP's `SessionConfigOption` with its identifier under [idKey], or null.
Map<String, Object?>? _option(Object? item, String idKey) {
  if (item is! Map || !_fits(item[idKey]) || item['name'] is! String) {
    return null;
  }
  final option = <String, Object?>{
    idKey: item[idKey],
    'name': _cut(item['name'] as String),
  };
  _text(option, item, 'description');
  final category = item['category'];
  if (category is String && utf8.encode(category).length <= _maxText) {
    option['category'] = category;
  }
  switch (item['type']) {
    case 'select':
      final values = _values(item['options']);
      if (!_fits(item['currentValue']) || values == null) return null;
      option['type'] = 'select';
      option['currentValue'] = item['currentValue'];
      option['options'] = values;
    case 'boolean':
      if (item['currentValue'] is! bool) return null;
      option['type'] = 'boolean';
      option['currentValue'] = item['currentValue'];
    default:
      return null;
  }
  return option;
}

String _unsaid(Map<String, dynamic> answer, String part) {
  final unknown = answer['unknown'];
  final reason = unknown is Map ? unknown[part] : null;
  return reason is String
      ? _cut(reason)
      : 'the agent did not say what $part it has';
}

AcpAnswer<List<Map<String, Object?>>> _describedOptions(
  Map<String, dynamic> answer,
  String idKey,
) {
  final listed = answer['configOptions'];
  if (listed is! List) return AcpUnknown(_unsaid(answer, 'configOptions'));
  return AcpKnown(
    listed
        .map((item) => _option(item, idKey))
        .whereType<Map<String, Object?>>()
        .take(_maxOptions)
        .toList(),
  );
}

AcpAnswer<Map<String, Object?>> _describedModes(Map<String, dynamic> answer) {
  final modes = answer['modes'];
  if (modes is! Map) return AcpUnknown(_unsaid(answer, 'modes'));
  if (!_fits(modes['currentModeId']) || !modes.containsKey('availableModes')) {
    return const AcpUnknown("the agent's modes could not be read");
  }
  final listed = modes['availableModes'] is List
      ? modes['availableModes'] as List
      : const [];
  final available = <Map<String, Object?>>[];
  for (final mode in listed) {
    if (mode is! Map || mode['id'] is! String || mode['name'] is! String) {
      continue;
    }
    if (!_fits(mode['id'])) continue;
    final kept = <String, Object?>{
      'id': mode['id'],
      'name': _cut(mode['name'] as String),
    };
    _text(kept, mode, 'description');
    available.add(kept);
    if (available.length == _maxModes) break;
  }
  return AcpKnown({
    'currentModeId': modes['currentModeId'],
    'availableModes': available,
  });
}

AcpAnswer<List<AcpSessionInfo>> _describedSessions(
  Map<String, dynamic> answer,
) {
  if (!answer.containsKey('sessions')) {
    return const AcpUnknown("the agent's sessions could not be read");
  }
  final listed = answer['sessions'] is List
      ? answer['sessions'] as List
      : const [];
  final sessions = <AcpSessionInfo>[];
  for (final item in listed) {
    if (item is! Map || !_fits(item['sessionId']) || !_fits(item['cwd'])) {
      continue;
    }
    String? text(String key) =>
        item[key] is String ? _cut(item[key] as String) : null;
    sessions.add(
      AcpSessionInfo(
        id: item['sessionId'] as String,
        cwd: item['cwd'] as String,
        title: text('title'),
        updatedAt: text('updatedAt'),
      ),
    );
    if (sessions.length == _maxSessions) break;
  }
  return AcpKnown(sessions);
}
