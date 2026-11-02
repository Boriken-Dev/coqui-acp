import 'dart:async';
import 'dart:convert';

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
  String toString() => 'AcpPermission(tool: $tool, allowed: $allowed, reason: $reason)';
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

final class AcpBlocked extends AcpEvent {
  final String message;
  const AcpBlocked(this.message);
  @override
  String toString() => 'AcpBlocked($message)';
}

final class AcpNotice extends AcpEvent {
  final String message;
  const AcpNotice(this.message);
  @override
  String toString() => 'AcpNotice($message)';
}

class AcpFailure implements Exception {
  const AcpFailure(this.message);
  final String message;
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
  const AcpPermissionRequest({
    required this.sessionId,
    required this.toolCallId,
    this.title,
    this.kind,
    this.rawInput,
  });
}

typedef AcpDecider = Future<AcpDecision> Function(AcpPermissionRequest request);

/// A session the agent opened, and what it said about it.
final class AcpOpenedSession {
  AcpOpenedSession(this.id, this.raw);
  final String id;
  final Map<String, dynamic> raw;
}

final class AcpSessionUpdate {
  AcpSessionUpdate(this.sessionId, this.raw);
  final String sessionId;
  final Map<String, dynamic> raw;
  String get kind => raw['sessionUpdate'] is String ? raw['sessionUpdate'] as String : '';
  String? get messageId => raw['messageId'] is String ? raw['messageId'] as String : null;
  String? get state => raw['state'] is String ? raw['state'] as String : null;
  String? get reason => raw['stopReason'] is String ? raw['stopReason'] as String : null;
  String? get text {
    final value = raw['content'];
    if (value is Map && value['type'] == 'text' && value['text'] is String) {
      return value['text'] as String;
    }
    if (value is List) {
      return value.whereType<Map>().where((entry) => entry['type'] == 'text')
          .map((entry) => entry['text']).whereType<String>().join();
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
      onDone: () => _fail(const AcpFailure('Connection lost; pending outcomes unknown')),
    );
  }

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
  int _counter = 0;
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
    unawaited(_send(jsonEncode({'jsonrpc': '2.0', 'id': id, ...body})).catchError((Object _) {}));
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
          if (method == 'session/update') _receiveUpdate(envelope['params']);
        } else if (method == 'session/request_permission') {
          unawaited(_answerPermission(id, envelope['params']));
        } else {
          _reply(id, {'error': {'code': -32601, 'message': 'Unsupported method'}});
        }
        return;
      }
      if (id is! int) return;
      final completer = _pending.remove(id);
      if (completer == null || completer.isCompleted) return;
      if (envelope['error'] is Map) {
        completer.completeError(AcpFailure('Agent error: ${envelope['error']}'));
      } else if (envelope['result'] is Map) {
        completer.complete(Map<String, dynamic>.from(envelope['result'] as Map));
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
    if (params is! Map || params['sessionId'] is! String || params['update'] is! Map) return;
    final update = AcpSessionUpdate(params['sessionId'] as String, Map<String, dynamic>.from(params['update'] as Map));
    if (!_closed) _updates.add(update);
    final raw = update.raw;
    switch (raw['sessionUpdate']) {
      case 'agent_message_chunk':
        final content = raw['content'];
        if (content is Map && content['type'] == 'text' && content['text'] is String) {
          _text.write(content['text'] as String);
        }
      case 'tool_call':
        final id = raw['toolCallId'];
        final title = raw['title'];
        if (id is String && title is String) {
          _flush();
          final kind = raw['kind'];
          _emit(AcpTool(id: id, title: title, kind: kind is String ? kind : 'other'));
        }
      case 'usage_update':
        final cost = raw['cost'];
        if (cost is Map && cost['amount'] is num && cost['currency'] is String) {
          _flush();
          _emit(AcpCost(amount: (cost['amount'] as num).toDouble(), currency: cost['currency'] as String));
        }
      case 'state_update':
        if (raw['state'] == 'idle') {
          final stop = raw['stopReason'];
          _endTurn(stop is String ? stop : 'end_turn');
        }
    }
  }

  void _endTurn(String stop) {
    _flush();
    _emit(switch (stop) {
      'end_turn' || 'max_tokens' || 'max_turn_requests' => AcpCompleted(stop),
      'cancelled' => const AcpBlocked('the turn was cancelled'),
      'refusal' => const AcpFailed('the agent refused to continue'),
      _ => AcpFailed('the agent ended the turn with $stop'),
    });
  }

  Future<void> _answerPermission(Object id, Object? params) async {
    if (params is! Map || params['toolCall'] is! Map || params['options'] is! List) {
      _reply(id, {'error': {'code': -32602, 'message': 'Invalid params'}});
      return;
    }
    final call = params['toolCall'] as Map;
    final title = call['title'];
    final tool = title is String ? title : 'a tool';
    AcpDecision decision;
    final decider = _decider;
    if (decider == null) {
      decision = const AcpDeny('no permission decider is configured');
    } else {
      try {
        decision = await decider(AcpPermissionRequest(
          sessionId: params['sessionId'] is String ? params['sessionId'] as String : '',
          toolCallId: call['toolCallId'] is String ? call['toolCallId'] as String : '',
          title: title is String ? title : null,
          kind: call['kind'] is String ? call['kind'] as String : null,
          rawInput: call['rawInput'],
        ));
      } catch (error) {
        decision = AcpDeny('the decider failed: $error');
      }
    }
    if (decision is AcpCancel) {
      _flush();
      _emit(AcpPermission(tool: tool, allowed: false, reason: 'the turn was cancelled'));
      _reply(id, {'result': {'outcome': {'outcome': 'cancelled'}}});
      return;
    }
    final wanted = decision is AcpAllow ? 'allow_once' : 'reject_once';
    _flush();
    _emit(AcpPermission(tool: tool, allowed: decision is AcpAllow, reason: decision is AcpDeny ? decision.reason : null));
    for (final option in params['options'] as List) {
      if (option is Map && option['kind'] == wanted && option['optionId'] is String) {
        _reply(id, {'result': {'outcome': {'outcome': 'selected', 'optionId': option['optionId']}}});
        return;
      }
    }
    _emit(AcpBlocked('the agent asked leave for $tool and offered no $wanted option'));
    _reply(id, {'result': {'outcome': {'outcome': 'cancelled'}}});
  }

  Future<void> initialize() async {
    if (_protocolVersion != null) return;
    final reply = await _request('initialize', {
      'protocolVersion': maxProtocolVersion,
      if (maxProtocolVersion >= 2) ...{
        'capabilities': <String, Object?>{},
        'info': {'name': clientName, 'title': clientName, 'version': clientVersion},
      } else ...{
        'clientCapabilities': {'fs': {'readTextFile': false, 'writeTextFile': false}, 'terminal': false},
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
      throw const AcpFailure('The agent selected a protocol version this client lacks');
    }
    _protocolVersion = version as int;
  }

  void _ready() {
    if (_protocolVersion == null) throw StateError('Initialize ACP before session operations');
  }

  void _checkCwd(String cwd) {
    if (!(cwd.startsWith('/') || RegExp(r'^[a-zA-Z]:[\\/]').hasMatch(cwd))) {
      throw ArgumentError.value(cwd, 'cwd', 'Must be an absolute path');
    }
  }

  Future<String> createSession({required String cwd}) => openSession(cwd: cwd).then((session) => session.id);

  /// As [createSession], with what the agent said about the session. It is the agent's to say: an agent may send neither.
  Future<AcpOpenedSession> openSession({required String cwd}) {
    _ready();
    _checkCwd(cwd);
    return _request('session/new', {'cwd': cwd, 'mcpServers': <Object>[]}).then((reply) {
      final id = reply['sessionId'];
      if (id is! String || id.isEmpty) throw const AcpFailure('Missing session ID');
      return AcpOpenedSession(id, reply);
    });
  }

  /// Sets one of the agent's `configOptions`, such as the model. The answer can change others, so read `configOptions` from it.
  Future<Map<String, dynamic>> setConfigOption(String sessionId, String configId, String value) {
    _ready();
    return _request('session/set_config_option', {'sessionId': sessionId, 'configId': configId, 'value': value});
  }

  /// Sets the agent's mode, for an agent that offers `modes` rather than `configOptions`.
  Future<void> setMode(String sessionId, String modeId) {
    _ready();
    return _request('session/set_mode', {'sessionId': sessionId, 'modeId': modeId});
  }

  Future<void> resumeSession(String id, {required String cwd, bool replayHistory = false}) {
    _ready();
    _checkCwd(cwd);
    return _request('session/resume', {
      'sessionId': id,
      'cwd': cwd,
      'mcpServers': <Object>[],
      if (replayHistory) 'replayFrom': {'type': 'start'},
    });
  }

  /// Sends a turn. In v2 it answers the inserted message's id as soon as the agent acknowledges, and the turn ends by a
  /// later event. In v1 the answer is the end of the turn, so this has no deadline and answers null.
  Future<String?> prompt(String sessionId, String text) async {
    _ready();
    if (text.trim().isEmpty) throw ArgumentError.value(text, 'text');
    final params = {
      'sessionId': sessionId,
      'prompt': [{'type': 'text', 'text': text}],
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
    if (id is! String || id.isEmpty) throw const AcpFailure('Missing inserted message ID');
    return id;
  }

  Future<void> cancel(String sessionId) async {
    _ready();
    if (_closed) throw const AcpFailure('Connection closed');
    await _send(jsonEncode({'jsonrpc': '2.0', 'method': 'session/cancel', 'params': {'sessionId': sessionId}}));
  }

  Future<void> closeSession(String sessionId) async {
    _ready();
    await _request('session/close', {'sessionId': sessionId});
  }

  Future<void> close() async {
    _fail(const AcpFailure('ACP client closed'));
    await _subscription.cancel();
  }

  Future<Map<String, dynamic>> _request(String method, Map<String, Object?> params, {bool limited = true}) async {
    if (_closed) throw const AcpFailure('Connection closed');
    final id = ++_counter;
    final pending = Completer<Map<String, dynamic>>();
    _pending[id] = pending;
    try {
      await _send(jsonEncode({'jsonrpc': '2.0', 'id': id, 'method': method, 'params': params})).timeout(requestTimeout);
      return await (limited ? pending.future.timeout(requestTimeout) : pending.future);
    } on TimeoutException {
      throw AcpFailure('$method: no answer in time');
    } finally {
      _pending.remove(id);
    }
  }
}
