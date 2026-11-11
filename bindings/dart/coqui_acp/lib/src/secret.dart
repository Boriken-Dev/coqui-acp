part of '../coqui_acp.dart';

// The secret-prompt extension, `docs/secrets.md`: a form whose secret fields travel sealed (HPKE, RFC 9180) to a per-request key that
// is bound to an identity the collector already knows. The Rust crate holds the same, and vectors/secret.json holds both to it.

/// The `_meta` key in which a frontend says it takes the extension.
const acpSecretsCapability = 'dev.boriken.coqui.acp/secrets';

/// The request that asks the person for values, some of them secret.
const acpSecretRequestMethod = '_coqui/secret/request';

/// The request that shows the person values, some of them secret.
const acpSecretShowMethod = '_coqui/secret/show';

/// The extension's version.
const acpSecretsVersion = 1;

/// The binding method for an identity that signs: ECDSA over P-256 and SHA-256, IEEE P1363.
const acpBindingEcdsa = 'ecdsa-p256-sha256';

/// The binding method for an X25519 identity: HMAC-SHA-256 under a key derived from the two static keys' Diffie-Hellman.
const acpBindingDh = 'x25519-hmac-sha256';

/// Why a secret message was refused; the same names as the Rust crate's error kinds.
enum AcpSecretFailure {
  schema('schema'),
  malformed('malformed'),
  unsupported('unsupported'),
  key('key'),
  unknownIdentity('unknown_identity'),
  badProof('bad_proof'),
  expired('expired'),
  wrongAudience('wrong_audience'),
  field('field'),
  seal('seal'),
  open('open'),
  tooLarge('too_large');

  const AcpSecretFailure(this.kind);

  /// The kind as `vectors/secret.json` names it.
  final String kind;
}

/// A refusal. It holds which check refused and a fixed reason: never a value, a key or a signature.
final class AcpSecretException implements Exception {
  const AcpSecretException(this.failure, [this.reason = '']);

  final AcpSecretFailure failure;
  final String reason;

  @override
  String toString() => 'AcpSecretException(${failure.kind}: $reason)';
}

AcpSecretException _secretFailure(AcpSecretFailure failure, String reason) =>
    AcpSecretException(failure, reason);

/// How much of a form is taken; every value from 0 up is taken.
final class AcpSecretLimits {
  const AcpSecretLimits({
    this.fields = 64,
    this.options = 256,
    this.schemaBytes = 64 * 1024,
    this.nameBytes = 128,
    this.valueBytes = 64 * 1024,
  });

  final int fields, options, schemaBytes, nameBytes, valueBytes;
}

/// One question of a secret form.
final class AcpSecretField {
  const AcpSecretField(
    this.field, {
    required this.secret,
    this.encoding,
    this.mediaType,
  });

  /// The question as a plain form has it.
  final AcpField field;

  /// Whether its value is a secret: sealed on the way.
  final bool secret;

  /// The schema's `contentEncoding`.
  final String? encoding;

  /// The schema's `contentMediaType`.
  final String? mediaType;
}

/// A form read from a schema whose secret properties are `writeOnly` or `x-sensitive`.
final class AcpSecretForm {
  AcpSecretForm._(this.title, this.description, this.fields, this.limits);

  final String? title;
  final String? description;
  final List<AcpSecretField> fields;
  final AcpSecretLimits limits;

  /// Reads a schema, or throws [AcpSecretException] (`schema`) naming the rule it breaks.
  factory AcpSecretForm.parse(
    Object? schema, [
    AcpSecretLimits limits = const AcpSecretLimits(),
  ]) {
    AcpSecretException bad(String why) =>
        _secretFailure(AcpSecretFailure.schema, why);
    if (schema is! Map) throw bad('the schema is not an object');
    if (utf8.encode(jsonEncode(schema)).length > limits.schemaBytes) {
      throw bad('the schema is larger than the limit');
    }
    if (schema.containsKey('type') && schema['type'] != 'object') {
      throw bad("the schema's type is not object");
    }
    final properties = schema['properties'];
    if (properties is! Map) throw bad('the schema has no properties');
    if (properties.isEmpty) throw bad('the schema asks nothing');
    if (properties.length > limits.fields) {
      throw bad('the schema has more questions than the limit');
    }
    final required = <String>[];
    if (schema.containsKey('required')) {
      final names = schema['required'];
      if (names is! List) throw bad('required is not a list');
      for (final name in names) {
        if (name is! String) {
          throw bad('required holds a name that is not text');
        }
        required.add(name);
      }
      if (required.any((name) => !properties.containsKey(name))) {
        throw bad('required names a property the schema lacks');
      }
    }
    final fields = <AcpSecretField>[];
    for (final MapEntry(:key, :value) in properties.entries) {
      final name = '$key';
      if (name.isEmpty ||
          utf8.encode(name).length > limits.nameBytes ||
          name.runes.any(
            (rune) => rune < 0x20 || (rune >= 0x7f && rune < 0xa0),
          )) {
        throw bad(
          "a property's name is empty, too long or holds a control character",
        );
      }
      if (value is! Map) throw bad('a property is not an object');
      final field = _field(name, value, required.contains(name));
      final options = switch (field) {
        AcpChoiceField(:final options) ||
        AcpChoicesField(:final options) => options.length,
        _ => 0,
      };
      if (options > limits.options) {
        throw bad('a question has more choices than the limit');
      }
      if (field is AcpUnsupportedField) {
        throw bad("a property's type is one no control can show");
      }
      final secret = value['writeOnly'] == true || value['x-sensitive'] == true;
      if (secret) {
        if (field is! AcpTextField && field is! AcpNumberField) {
          throw bad('a secret field is not text or a number');
        }
        if (const [
          'default',
          'const',
          'enum',
          'oneOf',
          'examples',
        ].any(value.containsKey)) {
          throw bad('a secret field carries a value or a choice in the schema');
        }
      }
      fields.add(
        AcpSecretField(
          field,
          secret: secret,
          encoding: _string(value, 'contentEncoding'),
          mediaType: _string(value, 'contentMediaType'),
        ),
      );
    }
    return AcpSecretForm._(
      _string(schema, 'title'),
      _string(schema, 'description'),
      fields,
      limits,
    );
  }

  /// The question named [name], or null.
  AcpSecretField? field(String name) {
    for (final field in fields) {
      if (field.field.name == name) return field;
    }
    return null;
  }

  /// Throws `field` unless [values] answer the plain questions: no name that is not one, none that is secret, every required one
  /// present, each of its type and within its bounds.
  void checkPlain(Map<String, Object?> values) {
    for (final name in values.keys) {
      final field = this.field(name);
      if (field == null) {
        throw _secretFailure(
          AcpSecretFailure.field,
          'a value for a question the form lacks',
        );
      }
      if (field.secret) {
        throw _secretFailure(
          AcpSecretFailure.field,
          'a secret field holds a plain value',
        );
      }
    }
    final plain = AcpElicitation(
      sessionId: '',
      message: '',
      fields: [
        for (final field in fields)
          if (!field.secret) field.field,
      ],
    );
    if (_misfit(plain, values) != null) {
      throw _secretFailure(
        AcpSecretFailure.field,
        'a plain value does not fit its question',
      );
    }
  }

  /// Throws `field` unless [names] are secret questions with every required one present.
  void checkSealed(Iterable<String> names) {
    final given = names.toList();
    for (final name in given) {
      if (!(field(name)?.secret ?? false)) {
        throw _secretFailure(
          AcpSecretFailure.field,
          'a sealed value for a question that is not secret',
        );
      }
    }
    if (fields.any(
      (field) =>
          field.secret &&
          field.field.required &&
          !given.contains(field.field.name),
    )) {
      throw _secretFailure(
        AcpSecretFailure.field,
        'a required secret question has no value',
      );
    }
  }

  /// Throws unless [value] fits secret question [name]: UTF-8 text of its length, a number as decimal text within its bounds,
  /// and base64 when the schema says so.
  void checkSecret(String name, List<int> value) {
    final question = field(name);
    if (question == null || !question.secret) {
      throw _secretFailure(
        AcpSecretFailure.field,
        'not a secret question of the form',
      );
    }
    if (value.length > limits.valueBytes) {
      throw _secretFailure(AcpSecretFailure.tooLarge, 'a secret value');
    }
    final String text;
    try {
      text = utf8.decode(value);
    } on FormatException {
      throw _secretFailure(
        AcpSecretFailure.field,
        'a secret value is not text',
      );
    }
    bool within(num at, num? least, num? most) =>
        (least == null || at >= least) && (most == null || at <= most);
    final fits = switch (question.field) {
      AcpTextField(:final minLength, :final maxLength) =>
        within(text.runes.length, minLength, maxLength) &&
            switch (question.encoding) {
              'base64' => _base64Text(text, url: false),
              'base64url' => _base64Text(text, url: true),
              _ => true,
            },
      AcpNumberField(:final integer, :final minimum, :final maximum) =>
        (integer
                ? RegExp(r'^[+-]?[0-9]+$').hasMatch(text) &&
                      int.tryParse(text) != null
                : double.tryParse(text)?.isFinite ?? false) &&
            within(
              integer ? int.parse(text) : double.parse(text),
              minimum,
              maximum,
            ),
      _ => false,
    };
    if (!fits) {
      throw _secretFailure(
        AcpSecretFailure.field,
        'a secret value does not fit its question',
      );
    }
  }
}

/// Whether [text] is base64 (or base64url), with or without its padding.
bool _base64Text(String text, {required bool url}) {
  final body = text.replaceFirst(RegExp(r'=+$'), '');
  final pattern = url
      ? RegExp(r'^[A-Za-z0-9_-]*$')
      : RegExp(r'^[A-Za-z0-9+/]*$');
  return pattern.hasMatch(body) && body.length % 4 != 1;
}

String _b64u(List<int> bytes) => base64Url.encode(bytes).replaceAll('=', '');

Uint8List _b64uDecode(String text) {
  if (text.contains('=') ||
      !RegExp(r'^[A-Za-z0-9_-]*$').hasMatch(text) ||
      text.length % 4 == 1) {
    throw _secretFailure(AcpSecretFailure.malformed, 'not unpadded base64url');
  }
  final padded = text.padRight((text.length + 3) ~/ 4 * 4, '=');
  final bytes = Uint8List.fromList(base64Url.decode(padded));
  // Trailing bits must be zero: the canonical text is the only one.
  if (_b64u(bytes) != text) {
    throw _secretFailure(AcpSecretFailure.malformed, 'not unpadded base64url');
  }
  return bytes;
}

String _hex(List<int> bytes) =>
    bytes.map((byte) => byte.toRadixString(16).padLeft(2, '0')).join();

Uint8List _unhex(String text) => Uint8List.fromList([
  for (var i = 0; i < text.length; i += 2)
    int.parse(text.substring(i, i + 2), radix: 16),
]);

Uint8List _concat(Iterable<List<int>> parts) {
  final out = BytesBuilder(copy: false);
  for (final part in parts) {
    out.add(part);
  }
  return out.toBytes();
}

Uint8List _lp(List<int> bytes) => _concat([
  (ByteData(4)..setUint32(0, bytes.length)).buffer.asUint8List(),
  bytes,
]);

bool _sameBytes(List<int> a, List<int> b) {
  if (a.length != b.length) return false;
  var diff = 0;
  for (var i = 0; i < a.length; i++) {
    diff |= a[i] ^ b[i];
  }
  return diff == 0;
}

/// An identity's id as the peer methods write it: `sha256:` and the lower-case hexadecimal SHA-256 of its `SubjectPublicKeyInfo`.
final class AcpIdentityId implements Comparable<AcpIdentityId> {
  AcpIdentityId._(this.bytes);

  /// The id of the public key whose encoding is [spki].
  factory AcpIdentityId.of(List<int> spki) =>
      AcpIdentityId._(pc.SHA256Digest().process(Uint8List.fromList(spki)));

  /// Reads `sha256:` and 64 hexadecimal digits, either case.
  factory AcpIdentityId.parse(String text) {
    final failure = _secretFailure(
      AcpSecretFailure.malformed,
      'an identity is sha256: and 64 hexadecimal digits',
    );
    if (!text.startsWith('sha256:')) throw failure;
    final hex = text.substring(7);
    if (!RegExp(r'^[0-9a-fA-F]{64}$').hasMatch(hex)) throw failure;
    return AcpIdentityId._(_unhex(hex.toLowerCase()));
  }

  final Uint8List bytes;

  @override
  String toString() => 'sha256:${_hex(bytes)}';

  @override
  bool operator ==(Object other) =>
      other is AcpIdentityId && _sameBytes(bytes, other.bytes);

  @override
  int get hashCode => Object.hashAll(bytes);

  @override
  int compareTo(AcpIdentityId other) => toString().compareTo(other.toString());
}

/// Which of the extension's two methods a request is; it is bound into every sealed field and key binding.
enum AcpSecretMethod {
  request('request'),
  show('show');

  const AcpSecretMethod(this.text);
  final String text;
}

/// A key binding as it travels.
final class AcpSecretBinding {
  const AcpSecretBinding({
    required this.method,
    required this.key,
    required this.expires,
    required this.proof,
    this.audience,
  });

  /// [acpBindingEcdsa] or [acpBindingDh].
  final String method;

  /// The holder's `SubjectPublicKeyInfo`.
  final Uint8List key;

  /// When it ends, in Unix seconds.
  final int expires;

  /// The collector it was made for, when known; required by [acpBindingDh].
  final AcpIdentityId? audience;

  /// The signature or the tag.
  final Uint8List proof;

  Map<String, Object?> toJson() => {
    'method': method,
    'key': _b64u(key),
    'expires': expires,
    if (audience != null) 'audience': audience.toString(),
    'proof': _b64u(proof),
  };

  factory AcpSecretBinding.fromJson(Object? json) {
    if (json is! Map) {
      throw _secretFailure(
        AcpSecretFailure.malformed,
        'a binding is not an object',
      );
    }
    String need(String key, String why) => json[key] is String
        ? json[key] as String
        : throw _secretFailure(AcpSecretFailure.malformed, why);
    final expires = json['expires'];
    if (expires is! int || expires < 0) {
      throw _secretFailure(AcpSecretFailure.malformed, 'a binding has no end');
    }
    final audience = json['audience'];
    return AcpSecretBinding(
      method: need('method', 'a binding has no method'),
      key: _b64uDecode(need('key', 'a binding has no key')),
      expires: expires,
      audience: switch (audience) {
        null => null,
        final String text => AcpIdentityId.parse(text),
        _ => throw _secretFailure(
          AcpSecretFailure.malformed,
          "a binding's audience is not text",
        ),
      },
      proof: _b64uDecode(need('proof', 'a binding has no proof')),
    );
  }
}

/// A sealed field as it travels: HPKE's encapsulated key and the ciphertext with its tag.
final class AcpSealed {
  const AcpSealed(this.enc, this.ct);

  /// The encapsulated key, 32 bytes.
  final Uint8List enc;

  /// The ciphertext followed by its 16-byte tag.
  final Uint8List ct;

  Map<String, Object?> toJson() => {'enc': _b64u(enc), 'ct': _b64u(ct)};

  factory AcpSealed.fromJson(Object? json) {
    const shape = 'a sealed value is not {enc, ct}';
    if (json is! Map ||
        json.length != 2 ||
        json['enc'] is! String ||
        json['ct'] is! String) {
      throw _secretFailure(AcpSecretFailure.malformed, shape);
    }
    final enc = _b64uDecode(json['enc'] as String);
    final ct = _b64uDecode(json['ct'] as String);
    if (ct.length < 16) {
      throw _secretFailure(
        AcpSecretFailure.malformed,
        'a sealed value is shorter than its tag',
      );
    }
    if (enc.length != 32) {
      throw _secretFailure(
        AcpSecretFailure.malformed,
        "a sealed value's key is not 32 bytes",
      );
    }
    return AcpSealed(enc, ct);
  }
}

/// The receiver's per-request public key with its binding.
final class AcpSecretRecipient {
  const AcpSecretRecipient(this.key, this.binding);

  final Uint8List key;
  final AcpSecretBinding binding;

  Map<String, Object?> toJson() => {
    'key': _b64u(key),
    'binding': binding.toJson(),
  };

  factory AcpSecretRecipient.fromJson(Object? json) {
    if (json is! Map || json['key'] is! String || json['binding'] == null) {
      throw _secretFailure(
        AcpSecretFailure.malformed,
        'a recipient has no key or binding',
      );
    }
    final key = _b64uDecode(json['key'] as String);
    if (key.length != 32) {
      throw _secretFailure(
        AcpSecretFailure.malformed,
        "a recipient's key is not 32 bytes",
      );
    }
    return AcpSecretRecipient(key, AcpSecretBinding.fromJson(json['binding']));
  }
}

/// A secret value held for its use. Dart cannot zero memory it has handed to the garbage collector, so [clear] overwrites the
/// bytes this object holds, and nothing prints it.
final class AcpSecretValue {
  AcpSecretValue(this._bytes);

  final Uint8List _bytes;

  /// The value as bytes.
  Uint8List get bytes => _bytes;

  /// The value as text; it was checked to be UTF-8 when it was opened.
  String get text => utf8.decode(_bytes);

  /// Overwrites the held bytes.
  void clear() => _bytes.fillRange(0, _bytes.length, 0);

  @override
  String toString() => 'AcpSecretValue(..)';
}

/// What a receiver gets back: the plain answers, and each secret one opened.
final class AcpOpened {
  const AcpOpened(this.plain, this.secrets);

  final Map<String, Object?> plain;
  final Map<String, AcpSecretValue> secrets;
}

Uint8List _fieldInfo(AcpSecretMethod method, String requestId, String field) =>
    _concat([
      _lp(ascii.encode('dev.boriken.coqui.acp/secret/v1 field')),
      _lp(ascii.encode(method.text)),
      _lp(utf8.encode(requestId)),
      _lp(utf8.encode(field)),
    ]);

void _checkRequestId(String requestId) {
  if (requestId.isEmpty ||
      utf8.encode(requestId).length > 128 ||
      requestId.runes.any(
        (rune) => rune < 0x20 || (rune >= 0x7f && rune < 0xa0),
      )) {
    throw _secretFailure(
      AcpSecretFailure.malformed,
      'a request id is 1 to 128 bytes with no control character',
    );
  }
}

// ---- Bindings.

final _p256 = pc.ECDomainParameters('prime256v1');
final _p256SpkiPrefix = _unhex(
  '3059301306072a8648ce3d020106082a8648ce3d030107034200',
);
final _x25519SpkiPrefix = _unhex('302a300506032b656e032100');
final _dhLabel = ascii.encode('dev.boriken.coqui.acp/secret/v1 dh');

Uint8List _statement(
  AcpSecretMethod method,
  String requestId,
  List<int> key,
  int expires,
  AcpIdentityId? audience,
) => _concat([
  _lp(ascii.encode('dev.boriken.coqui.acp/secret/v1 binding')),
  _lp(ascii.encode(method.text)),
  _lp(utf8.encode(requestId)),
  _lp(key),
  _lp((ByteData(8)..setUint64(0, expires)).buffer.asUint8List()),
  _lp(audience?.bytes ?? const <int>[]),
]);

Uint8List _bigBytes(BigInt value) {
  final out = Uint8List(32);
  for (var i = 31; i >= 0; i--) {
    out[i] = (value & BigInt.from(0xff)).toInt();
    value = value >> 8;
  }
  return out;
}

BigInt _bigOf(List<int> bytes) =>
    bytes.fold(BigInt.zero, (value, byte) => (value << 8) | BigInt.from(byte));

Uint8List _x25519OfSpki(List<int> spki) {
  if (spki.length != 44 ||
      !_sameBytes(spki.sublist(0, 12), _x25519SpkiPrefix)) {
    throw _secretFailure(
      AcpSecretFailure.key,
      'not an X25519 SubjectPublicKeyInfo',
    );
  }
  return Uint8List.fromList(spki.sublist(12));
}

Future<Uint8List> _dhMacKey(
  List<int> secret,
  List<int> other,
  List<int> holder,
  List<int> collector,
) async {
  final dh = await hpke.x25519Dh(secret, other);
  final prk = await hpke.hmacSha256(Uint8List(32), dh);
  return hpke.expandOne(prk, [..._dhLabel, ...holder, ...collector], 32);
}

/// The holder's side of a binding: the identity's private key.
final class AcpSecretProver {
  AcpSecretProver._ecdsa(BigInt scalar, this.spki)
    : _scalar = scalar,
      _dhSecret = null,
      _collector = null,
      _audience = null;

  AcpSecretProver._dh(
    this._dhSecret,
    this.spki,
    this._collector,
    this._audience,
  ) : _scalar = null;

  final BigInt? _scalar;
  final Uint8List? _dhSecret;
  final Uint8List? _collector;
  final AcpIdentityId? _audience;

  /// The identity's public key as `SubjectPublicKeyInfo`.
  final Uint8List spki;

  /// The identity's id.
  AcpIdentityId get id => AcpIdentityId.of(spki);

  /// An identity that signs, from its P-256 private scalar (32 bytes).
  factory AcpSecretProver.ecdsaP256(List<int> scalar) {
    final d = _bigOf(scalar);
    if (scalar.length != 32 || d == BigInt.zero || d >= _p256.n) {
      throw _secretFailure(AcpSecretFailure.key, 'not a P-256 private scalar');
    }
    final point = (_p256.G * d)!;
    return AcpSecretProver._ecdsa(
      d,
      _concat([_p256SpkiPrefix, point.getEncoded(false)]),
    );
  }

  /// An X25519 identity, from its private key (32 bytes), for the collector whose `SubjectPublicKeyInfo` is [collectorSpki].
  static Future<AcpSecretProver> x25519(
    List<int> secret,
    List<int> collectorSpki,
  ) async {
    if (secret.length != 32) {
      throw _secretFailure(
        AcpSecretFailure.key,
        'an X25519 private key is 32 bytes',
      );
    }
    final collector = _x25519OfSpki(collectorSpki);
    final publicKey = await hpke.x25519Public(secret);
    return AcpSecretProver._dh(
      Uint8List.fromList(secret),
      _concat([_x25519SpkiPrefix, publicKey]),
      collector,
      AcpIdentityId.of(collectorSpki),
    );
  }

  Future<AcpSecretBinding> _bind(
    AcpSecretMethod method,
    String requestId,
    Uint8List key,
    int expires,
  ) async {
    if (_scalar != null) {
      final signer = pc.ECDSASigner(
        pc.SHA256Digest(),
        pc.HMac(pc.SHA256Digest(), 64),
      )..init(true, pc.PrivateKeyParameter(pc.ECPrivateKey(_scalar, _p256)));
      final signature =
          signer.generateSignature(
                _statement(method, requestId, key, expires, null),
              )
              as pc.ECSignature;
      // Low s, as the Rust crate writes it; a verifier takes either.
      final s = signature.s > _p256.n >> 1
          ? _p256.n - signature.s
          : signature.s;
      return AcpSecretBinding(
        method: acpBindingEcdsa,
        key: spki,
        expires: expires,
        proof: _concat([_bigBytes(signature.r), _bigBytes(s)]),
      );
    }
    final holder = _x25519OfSpki(spki);
    final mac = await _dhMacKey(_dhSecret!, _collector!, holder, _collector);
    final proof = await hpke.hmacSha256(
      mac,
      _statement(method, requestId, key, expires, _audience),
    );
    return AcpSecretBinding(
      method: acpBindingDh,
      key: spki,
      expires: expires,
      audience: _audience,
      proof: proof,
    );
  }
}

/// The collector's side: the identities it knows by pairing or pinning, and its own X25519 key when it takes [acpBindingDh].
final class AcpSecretVerifier {
  AcpSecretVerifier({
    Iterable<AcpIdentityId> known = const [],
    List<int>? x25519Identity,
    this.maxLifetime = const Duration(minutes: 5),
  }) : _known = {for (final id in known) id.toString()},
       _own = x25519Identity == null
           ? null
           : Uint8List.fromList(x25519Identity);

  final Set<String> _known;
  final Uint8List? _own;

  /// The longest a binding may last from now.
  final Duration maxLifetime;

  /// Knows an identity by its `SubjectPublicKeyInfo`.
  void trustSpki(List<int> spki) =>
      _known.add(AcpIdentityId.of(spki).toString());

  /// Knows an identity by its id.
  void trust(AcpIdentityId id) => _known.add(id.toString());

  /// Checks that [binding] binds [key] to a known identity for this request at [now] (Unix seconds); returns that identity. The
  /// order is the Rust crate's: method and key shape, whether the holder is known, the end, the collector, last the proof.
  Future<AcpIdentityId> verify(
    AcpSecretMethod method,
    String requestId,
    Uint8List key,
    AcpSecretBinding binding,
    int now,
  ) async {
    final ecdsa = switch (binding.method) {
      acpBindingEcdsa => true,
      acpBindingDh => false,
      _ => throw _secretFailure(
        AcpSecretFailure.unsupported,
        'a binding method this side does not have',
      ),
    };
    if (ecdsa) {
      if (binding.key.length != 91 ||
          !_sameBytes(binding.key.sublist(0, 26), _p256SpkiPrefix)) {
        throw _secretFailure(
          AcpSecretFailure.key,
          'not a P-256 SubjectPublicKeyInfo',
        );
      }
    } else {
      _x25519OfSpki(binding.key);
    }
    final holder = AcpIdentityId.of(binding.key);
    if (!_known.contains(holder.toString())) {
      throw _secretFailure(AcpSecretFailure.unknownIdentity, '');
    }
    if (now > binding.expires ||
        binding.expires > now + maxLifetime.inSeconds) {
      throw _secretFailure(AcpSecretFailure.expired, '');
    }
    final ownId = _own == null
        ? null
        : AcpIdentityId.of(
            _concat([_x25519SpkiPrefix, await hpke.x25519Public(_own)]),
          );
    if (binding.audience != null &&
        ownId != null &&
        binding.audience != ownId) {
      throw _secretFailure(AcpSecretFailure.wrongAudience, '');
    }
    final signed = _statement(
      method,
      requestId,
      key,
      binding.expires,
      binding.audience,
    );
    if (ecdsa) {
      final point = _p256.curve.decodePoint(binding.key.sublist(26));
      if (point == null || binding.proof.length != 64) {
        throw _secretFailure(AcpSecretFailure.badProof, '');
      }
      final verifier = pc.ECDSASigner(pc.SHA256Digest())
        ..init(false, pc.PublicKeyParameter(pc.ECPublicKey(point, _p256)));
      final signature = pc.ECSignature(
        _bigOf(binding.proof.sublist(0, 32)),
        _bigOf(binding.proof.sublist(32)),
      );
      if (!verifier.verifySignature(signed, signature)) {
        throw _secretFailure(AcpSecretFailure.badProof, '');
      }
    } else {
      if (_own == null) {
        throw _secretFailure(
          AcpSecretFailure.unsupported,
          'this collector has no X25519 identity of its own',
        );
      }
      if (binding.audience != ownId) {
        throw _secretFailure(AcpSecretFailure.wrongAudience, '');
      }
      final holderKey = _x25519OfSpki(binding.key);
      final mac = await _dhMacKey(
        _own,
        holderKey,
        holderKey,
        await hpke.x25519Public(_own),
      );
      if (!_sameBytes(await hpke.hmacSha256(mac, signed), binding.proof)) {
        throw _secretFailure(AcpSecretFailure.badProof, '');
      }
    }
    return holder;
  }

  /// Checks the receiver's key and binding at [now], and returns the collector that seals to it.
  Future<AcpSecretCollector> collector(
    AcpSecretMethod method,
    String requestId,
    AcpSecretForm form,
    AcpSecretRecipient recipient,
    int now,
  ) async {
    _checkRequestId(requestId);
    final holder = await verify(
      method,
      requestId,
      recipient.key,
      recipient.binding,
      now,
    );
    return AcpSecretCollector._(method, requestId, form, recipient.key, holder);
  }
}

/// The collecting side of one request, after it checked the receiver's key.
final class AcpSecretCollector {
  AcpSecretCollector._(
    this.method,
    this.requestId,
    this.form,
    this._key,
    this.holder,
  );

  final AcpSecretMethod method;
  final String requestId;
  final AcpSecretForm form;
  final Uint8List _key;

  /// The identity that stands behind the key.
  final AcpIdentityId holder;

  /// Whether the field is one this collector would seal.
  bool isSecret(String name) => form.field(name)?.secret ?? false;

  /// Seals the value of secret field [name]: UTF-8 text, or a number as decimal text. [random] is for tests.
  Future<AcpSealed> seal(String name, String value, {Random? random}) async {
    final bytes = utf8.encode(value);
    form.checkSecret(name, bytes);
    final source = random ?? Random.secure();
    final ikm = Uint8List.fromList([
      for (var i = 0; i < 32; i++) source.nextInt(256),
    ]);
    final made = await hpke.hpkeSeal(
      _key,
      _fieldInfo(method, requestId, name),
      const [],
      bytes,
      ikm,
    );
    return AcpSealed(made.enc, made.ct);
  }
}

/// The receiving side of one request: it holds the per-request private key and gives the collector the public key with a binding.
final class AcpSecretReceiver {
  AcpSecretReceiver._(
    this.method,
    this.requestId,
    this.form,
    this._secret,
    this.recipient,
  );

  /// Makes a key pair for one request and binds the public key to [prover]'s identity until [now] plus [lifetime]. [random] is for
  /// tests.
  static Future<AcpSecretReceiver> create(
    AcpSecretMethod method,
    String requestId,
    AcpSecretForm form,
    AcpSecretProver prover,
    int now,
    Duration lifetime, {
    Random? random,
  }) async {
    final source = random ?? Random.secure();
    final ikm = Uint8List.fromList([
      for (var i = 0; i < 32; i++) source.nextInt(256),
    ]);
    return fromIkm(method, requestId, form, prover, now, lifetime, ikm);
  }

  /// As [create], the key pair derived from [ikm] (RFC 9180's DeriveKeyPair).
  static Future<AcpSecretReceiver> fromIkm(
    AcpSecretMethod method,
    String requestId,
    AcpSecretForm form,
    AcpSecretProver prover,
    int now,
    Duration lifetime,
    List<int> ikm,
  ) async {
    _checkRequestId(requestId);
    final secret = await hpke.deriveKey(ikm);
    final publicKey = await hpke.x25519Public(secret);
    final binding = await prover._bind(
      method,
      requestId,
      publicKey,
      now + lifetime.inSeconds,
    );
    return AcpSecretReceiver._(
      method,
      requestId,
      form,
      secret,
      AcpSecretRecipient(publicKey, binding),
    );
  }

  final AcpSecretMethod method;
  final String requestId;
  final AcpSecretForm form;
  final Uint8List _secret;

  /// What to send the collector.
  final AcpSecretRecipient recipient;

  /// Opens one secret field.
  Future<AcpSecretValue> open(String name, AcpSealed sealed) async {
    if (!(form.field(name)?.secret ?? false)) {
      throw _secretFailure(
        AcpSecretFailure.field,
        'not a secret question of the form',
      );
    }
    final Uint8List plain;
    try {
      plain = await hpke.hpkeOpen(
        _secret,
        _fieldInfo(method, requestId, name),
        const [],
        hpke.HpkeSealed(sealed.enc, sealed.ct),
      );
    } on hpke.HpkeFailure {
      throw _secretFailure(
        AcpSecretFailure.open,
        'the value cannot be opened for this request and field',
      );
    }
    try {
      form.checkSecret(name, plain);
    } on Object {
      plain.fillRange(0, plain.length, 0);
      rethrow;
    }
    return AcpSecretValue(plain);
  }

  /// Opens an answer: checks the plain values against the form and opens every sealed one.
  Future<AcpOpened> openValues(
    Map<String, Object?> plain,
    Map<String, AcpSealed> sealed,
  ) async {
    form.checkPlain(plain);
    form.checkSealed(sealed.keys);
    final secrets = <String, AcpSecretValue>{};
    try {
      for (final MapEntry(:key, :value) in sealed.entries) {
        secrets[key] = await open(key, value);
      }
    } on Object {
      for (final secret in secrets.values) {
        secret.clear();
      }
      rethrow;
    }
    return AcpOpened(Map.of(plain), secrets);
  }
}

// ---- Messages.

/// What a frontend offers under [acpSecretsCapability].
final class AcpSecretsCapability {
  const AcpSecretsCapability({
    this.request = true,
    this.show = true,
    this.bindings = const [acpBindingEcdsa, acpBindingDh],
  });

  final bool request, show;
  final List<String> bindings;

  Map<String, Object?> toJson() => {
    'version': acpSecretsVersion,
    'methods': [if (request) 'request', if (show) 'show'],
    'bindings': bindings,
  };

  /// Reads the capability from a `_meta` object, or null when it is absent, of another version, or offers no method: a host then
  /// serves no secret tools to that client.
  static AcpSecretsCapability? fromMeta(Map<String, Object?> meta) {
    final map = meta[acpSecretsCapability];
    if (map is! Map || map['version'] != acpSecretsVersion) return null;
    final methods = map['methods'];
    if (methods is! List) return null;
    final bindings = map['bindings'];
    final capability = AcpSecretsCapability(
      request: methods.contains('request'),
      show: methods.contains('show'),
      bindings: bindings is List
          ? bindings.whereType<String>().toList()
          : const [],
    );
    return capability.request || capability.show ? capability : null;
  }
}

Map<String, Object?> _mapOf(Object? value, String why) => value is Map
    ? value.cast<String, Object?>()
    : throw _secretFailure(AcpSecretFailure.malformed, why);

String _textOf(Map map, String key, String why) => map[key] is String
    ? map[key] as String
    : throw _secretFailure(AcpSecretFailure.malformed, why);

Map<String, Object?>? _formOverlay(Map map) => switch (map['form']) {
  null => null,
  final Map form => form.cast<String, Object?>(),
  _ => throw _secretFailure(
    AcpSecretFailure.malformed,
    'the form overlay is not an object',
  ),
};

/// `_coqui/secret/request`'s parameters.
final class AcpSecretRequestParams {
  const AcpSecretRequestParams({
    required this.sessionId,
    required this.requestId,
    required this.message,
    required this.schema,
    required this.recipient,
    this.form,
  });

  final String sessionId, requestId, message;
  final Map<String, Object?> schema;
  final Map<String, Object?>? form;
  final AcpSecretRecipient recipient;

  Map<String, Object?> toJson() => {
    'sessionId': sessionId,
    'requestId': requestId,
    'message': message,
    'schema': schema,
    if (form != null) 'form': form,
    'recipient': recipient.toJson(),
  };

  factory AcpSecretRequestParams.fromJson(Object? json) {
    final map = _mapOf(json, 'the request is not an object');
    return AcpSecretRequestParams(
      sessionId: _textOf(map, 'sessionId', 'the request has no session'),
      requestId: _textOf(map, 'requestId', 'the request has no id'),
      message: _textOf(map, 'message', 'the request has no message'),
      schema: _mapOf(map['schema'], 'the request has no schema'),
      form: _formOverlay(map),
      recipient: AcpSecretRecipient.fromJson(map['recipient']),
    );
  }
}

/// The person's answer to a request.
sealed class AcpSecretRequestReply {
  const AcpSecretRequestReply();

  Map<String, Object?> toJson();

  /// Reads an answer; [isSecret] says which fields are secret (the form's).
  factory AcpSecretRequestReply.fromJson(
    Object? json,
    bool Function(String name) isSecret,
  ) {
    final map = _mapOf(json, 'the answer is not an object');
    switch (map['action']) {
      case 'decline':
        return const AcpSecretDeclined();
      case 'cancel':
        return const AcpSecretCancelled();
      case 'accept':
        final values = _mapOf(map['values'], 'the values are not an object');
        final plain = <String, Object?>{};
        final sealed = <String, AcpSealed>{};
        for (final MapEntry(:key, :value) in values.entries) {
          if (isSecret(key)) {
            sealed[key] = AcpSealed.fromJson(value);
          } else {
            plain[key] = value;
          }
        }
        return AcpSecretAccepted(plain, sealed);
    }
    throw _secretFailure(
      AcpSecretFailure.malformed,
      "the answer's action is not accept, decline or cancel",
    );
  }
}

final class AcpSecretAccepted extends AcpSecretRequestReply {
  const AcpSecretAccepted(this.plain, this.sealed);

  final Map<String, Object?> plain;
  final Map<String, AcpSealed> sealed;

  @override
  Map<String, Object?> toJson() => {
    'action': 'accept',
    'values': {
      ...plain,
      for (final MapEntry(:key, :value) in sealed.entries) key: value.toJson(),
    },
  };
}

final class AcpSecretDeclined extends AcpSecretRequestReply {
  const AcpSecretDeclined();

  @override
  Map<String, Object?> toJson() => {'action': 'decline'};
}

final class AcpSecretCancelled extends AcpSecretRequestReply {
  const AcpSecretCancelled();

  @override
  Map<String, Object?> toJson() => {'action': 'cancel'};
}

/// `_coqui/secret/show` with `"step": "open"`.
final class AcpSecretShowOpen {
  const AcpSecretShowOpen({
    required this.sessionId,
    required this.requestId,
    required this.message,
    required this.schema,
    required this.values,
    this.form,
  });

  final String sessionId, requestId, message;
  final Map<String, Object?> schema, values;
  final Map<String, Object?>? form;

  Map<String, Object?> toJson() => {
    'step': 'open',
    'sessionId': sessionId,
    'requestId': requestId,
    'message': message,
    'schema': schema,
    if (form != null) 'form': form,
    'values': values,
  };

  factory AcpSecretShowOpen.fromJson(Object? json) {
    final map = _mapOf(json, 'the show is not an object');
    if (map['step'] != 'open') {
      throw _secretFailure(
        AcpSecretFailure.malformed,
        "the show's step is not open",
      );
    }
    return AcpSecretShowOpen(
      sessionId: _textOf(map, 'sessionId', 'the show has no session'),
      requestId: _textOf(map, 'requestId', 'the show has no id'),
      message: _textOf(map, 'message', 'the show has no message'),
      schema: _mapOf(map['schema'], 'the show has no schema'),
      form: _formOverlay(map),
      values: _mapOf(map['values'], 'the show has no values'),
    );
  }
}

/// The answer to an `open` step: [recipient] when ready, else null with [action] `decline` or `cancel`.
final class AcpSecretShowOpened {
  const AcpSecretShowOpened.ready(AcpSecretRecipient this.recipient)
    : action = 'ready';
  const AcpSecretShowOpened.declined() : action = 'decline', recipient = null;
  const AcpSecretShowOpened.cancelled() : action = 'cancel', recipient = null;

  final String action;
  final AcpSecretRecipient? recipient;

  Map<String, Object?> toJson() => {
    'action': action,
    if (recipient != null) 'recipient': recipient!.toJson(),
  };

  factory AcpSecretShowOpened.fromJson(Object? json) {
    final map = _mapOf(json, 'the answer is not an object');
    return switch (map['action']) {
      'ready' => AcpSecretShowOpened.ready(
        AcpSecretRecipient.fromJson(map['recipient']),
      ),
      'decline' => const AcpSecretShowOpened.declined(),
      'cancel' => const AcpSecretShowOpened.cancelled(),
      _ => throw _secretFailure(
        AcpSecretFailure.malformed,
        "the answer's action is not ready, decline or cancel",
      ),
    };
  }
}

/// `_coqui/secret/show` with `"step": "deliver"`.
final class AcpSecretShowDeliver {
  const AcpSecretShowDeliver({
    required this.sessionId,
    required this.requestId,
    required this.sealed,
  });

  final String sessionId, requestId;
  final Map<String, AcpSealed> sealed;

  Map<String, Object?> toJson() => {
    'step': 'deliver',
    'sessionId': sessionId,
    'requestId': requestId,
    'values': {
      for (final MapEntry(:key, :value) in sealed.entries) key: value.toJson(),
    },
  };

  factory AcpSecretShowDeliver.fromJson(Object? json) {
    final map = _mapOf(json, 'the show is not an object');
    if (map['step'] != 'deliver') {
      throw _secretFailure(
        AcpSecretFailure.malformed,
        "the show's step is not deliver",
      );
    }
    final values = _mapOf(map['values'], 'the show has no values');
    return AcpSecretShowDeliver(
      sessionId: _textOf(map, 'sessionId', 'the show has no session'),
      requestId: _textOf(map, 'requestId', 'the show has no id'),
      sealed: {
        for (final MapEntry(:key, :value) in values.entries)
          key: AcpSealed.fromJson(value),
      },
    );
  }
}

/// The answer to a `deliver` step: `shown` when the person saw the values, else `cancel`.
Map<String, Object?> acpSecretShowDone({required bool shown}) => {
  'action': shown ? 'shown' : 'cancel',
};

/// Reads the answer to a `deliver` step: true for `shown`.
bool acpSecretShowDoneFromJson(Object? json) =>
    switch (_mapOf(json, 'the answer is not an object')['action']) {
      'shown' => true,
      'cancel' => false,
      _ => throw _secretFailure(
        AcpSecretFailure.malformed,
        "the answer's action is not shown or cancel",
      ),
    };
