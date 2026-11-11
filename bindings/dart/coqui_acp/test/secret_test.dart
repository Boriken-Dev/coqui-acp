@TestOn('vm')
library;

import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp/src/hpke.dart' as hpke;
import 'package:test/test.dart';

Uint8List unhex(String text) => Uint8List.fromList([
  for (var i = 0; i < text.length; i += 2)
    int.parse(text.substring(i, i + 2), radix: 16),
]);

String hex(List<int> bytes) =>
    bytes.map((byte) => byte.toRadixString(16).padLeft(2, '0')).join();

/// Randomness that reads off the bytes given, as the vectors fix the ephemeral keys.
final class Fixed implements Random {
  Fixed(this.bytes);

  final List<int> bytes;
  var at = 0;

  @override
  int nextInt(int max) => bytes[at++];

  @override
  bool nextBool() => throw UnimplementedError();

  @override
  double nextDouble() => throw UnimplementedError();
}

List<int> ikm(int start) => [for (var i = start; i < start + 32; i++) i];

Uint8List flipped(List<int> bytes) =>
    Uint8List.fromList(bytes)..[bytes.length - 1] ^= 1;

AcpSecretMethod methodOf(String text) =>
    text == 'show' ? AcpSecretMethod.show : AcpSecretMethod.request;

Map<String, dynamic> find(List<dynamic> items, String name) => items
    .cast<Map<String, dynamic>>()
    .firstWhere((item) => item['name'] == name);

Future<AcpSecretProver> proverOf(Map<String, dynamic> base) async {
  final private = unhex(base['private'] as String);
  if (base['kind'] == 'ecdsa') return AcpSecretProver.ecdsaP256(private);
  final collector = await hpke.x25519Public(
    unhex(base['collector_private'] as String),
  );
  return AcpSecretProver.x25519(private, [
    ...unhex('302a300506032b656e032100'),
    ...collector,
  ]);
}

Future<void> expectRefusal(Future<Object?> Function() run, String kind) async {
  try {
    await run();
  } on AcpSecretException catch (error) {
    expect(error.failure.kind, kind);
    return;
  }
  fail('expected a $kind refusal');
}

Future<AcpIdentityId> verifyCase(
  Map<String, dynamic> doc,
  Map<String, dynamic> item,
) async {
  final base = find(doc['bindings'] as List, item['base'] as String);
  final change = item['change'] as Map<String, dynamic>;
  var recipient = AcpSecretRecipient.fromJson(base['recipient']);
  final identity = AcpIdentityId.parse(base['identity'] as String);
  final own = change['no_own_identity'] == true
      ? null
      : (change['collector_private'] ?? base['collector_private']) as String?;
  final verifier = AcpSecretVerifier(
    known: change['trusted'] == false ? const [] : [identity],
    x25519Identity: own == null ? null : unhex(own),
    maxLifetime: Duration(seconds: (change['max_lifetime'] as int?) ?? 300),
  );
  var key = recipient.key;
  var binding = recipient.binding;
  if (change['flip_key'] == true) key = flipped(key);
  if (change['flip_proof'] == true) {
    binding = AcpSecretBinding(
      method: binding.method,
      key: binding.key,
      expires: binding.expires,
      audience: binding.audience,
      proof: flipped(binding.proof),
    );
  }
  if (change['binding_method'] != null) {
    binding = AcpSecretBinding(
      method: change['binding_method'] as String,
      key: binding.key,
      expires: binding.expires,
      audience: binding.audience,
      proof: binding.proof,
    );
  }
  if (change['truncate_binding_key'] == true) {
    binding = AcpSecretBinding(
      method: binding.method,
      key: binding.key.sublist(0, 10),
      expires: binding.expires,
      audience: binding.audience,
      proof: binding.proof,
    );
  }
  return verifier.verify(
    methodOf((change['method'] ?? base['method']) as String),
    (change['request_id'] ?? base['request_id']) as String,
    key,
    binding,
    (change['now'] ?? base['now']) as int,
  );
}

Future<AcpSecretValue> openCase(
  Map<String, dynamic> doc,
  Map<String, dynamic> item,
) async {
  final entry = find(doc['sealed'] as List, item['base'] as String);
  final base = find(doc['bindings'] as List, item['base'] as String);
  final change = item['change'] as Map<String, dynamic>;
  final form = AcpSecretForm.parse(entry['schema']);
  final name = item['field'] as String;
  final field = (entry['fields'] as List)
      .cast<Map<String, dynamic>>()
      .firstWhere((field) => field['field'] == name);
  var sealed = AcpSealed.fromJson(field['sealed']);
  if (change['flip_ct'] == true) {
    sealed = AcpSealed(sealed.enc, flipped(sealed.ct));
  }
  if (change['flip_enc'] == true) {
    sealed = AcpSealed(flipped(sealed.enc), sealed.ct);
  }
  final method = methodOf((change['method'] ?? base['method']) as String);
  final requestId = (change['request_id'] ?? base['request_id']) as String;
  final prover = await proverOf(base);
  final receiverIkm = change['other_receiver'] == true
      ? ikm(100)
      : unhex(base['recipient_ikm'] as String);
  if (change['seal_value'] != null) {
    final recipient = AcpSecretRecipient.fromJson(base['recipient']);
    // The value is sealed without the collector's own check, as a hostile collector would.
    final made = await hpke.hpkeSeal(
      recipient.key,
      _info(
        methodOf(base['method'] as String),
        base['request_id'] as String,
        name,
      ),
      const [],
      utf8.encode(change['seal_value'] as String),
      ikm(32),
    );
    sealed = AcpSealed(made.enc, made.ct);
  }
  final receiver = await AcpSecretReceiver.fromIkm(
    method,
    requestId,
    form,
    prover,
    base['now'] as int,
    Duration(seconds: base['lifetime'] as int),
    receiverIkm,
  );
  return receiver.open((change['as_field'] ?? name) as String, sealed);
}

/// The HPKE info of a field, written out again so a change to the library's is seen.
Uint8List _info(AcpSecretMethod method, String requestId, String field) {
  final out = BytesBuilder();
  for (final part in [
    ascii.encode('dev.boriken.coqui.acp/secret/v1 field'),
    ascii.encode(method.text),
    utf8.encode(requestId),
    utf8.encode(field),
  ]) {
    out
      ..add((ByteData(4)..setUint32(0, part.length)).buffer.asUint8List())
      ..add(part);
  }
  return out.toBytes();
}

void main() {
  final doc =
      jsonDecode(File('../../../vectors/secret.json').readAsStringSync())
          as Map<String, dynamic>;

  test('RFC 9180: the suite reproduces appendix A.1.1', () async {
    final rfc = doc['hpke'] as Map<String, dynamic>;
    final secretR = await hpke.deriveKey(unhex(rfc['ikmR'] as String));
    expect(hex(secretR), rfc['skRm']);
    final publicR = await hpke.x25519Public(secretR);
    expect(hex(publicR), rfc['pkRm']);
    final secretE = await hpke.deriveKey(unhex(rfc['ikmE'] as String));
    expect(hex(secretE), rfc['skEm']);
    expect(hex(await hpke.x25519Public(secretE)), rfc['pkEm']);
    final first = (rfc['encryptions'] as List).first as Map<String, dynamic>;
    final aad = unhex(first['aad'] as String);
    final info = unhex(rfc['info'] as String);
    final made = await hpke.hpkeSeal(
      publicR,
      info,
      aad,
      unhex(first['pt'] as String),
      unhex(rfc['ikmE'] as String),
    );
    expect(hex(made.enc), rfc['enc']);
    expect(hex(made.ct), first['ct']);
    expect(hex(await hpke.hpkeOpen(secretR, info, aad, made)), first['pt']);
    await expectLater(
      hpke.hpkeOpen(secretR, info, ascii.encode('Count-1'), made),
      throwsA(isA<hpke.HpkeFailure>()),
    );
  });

  test('a binding made here is the bytes the vector holds', () async {
    for (final raw in doc['bindings'] as List) {
      final base = raw as Map<String, dynamic>;
      final prover = await proverOf(base);
      expect(prover.id.toString(), base['identity']);
      final receiver = await AcpSecretReceiver.fromIkm(
        methodOf(base['method'] as String),
        base['request_id'] as String,
        AcpSecretForm.parse(
          find(doc['sealed'] as List, base['name'] as String)['schema'],
        ),
        prover,
        base['now'] as int,
        Duration(seconds: base['lifetime'] as int),
        unhex(base['recipient_ikm'] as String),
      );
      expect(
        receiver.recipient.toJson(),
        base['recipient'],
        reason: base['name'] as String,
      );
      expect(
        (await verifyCase(doc, {
          'base': base['name'],
          'change': <String, dynamic>{},
        })),
        prover.id,
      );
    }
  });

  test(
    'fields sealed by the Rust crate open here, and sealing again gives its bytes',
    () async {
      for (final raw in doc['sealed'] as List) {
        final entry = raw as Map<String, dynamic>;
        final base = find(doc['bindings'] as List, entry['binding'] as String);
        final form = AcpSecretForm.parse(entry['schema']);
        final method = methodOf(base['method'] as String);
        final requestId = base['request_id'] as String;
        final prover = await proverOf(base);
        final receiver = await AcpSecretReceiver.fromIkm(
          method,
          requestId,
          form,
          prover,
          base['now'] as int,
          Duration(seconds: base['lifetime'] as int),
          unhex(base['recipient_ikm'] as String),
        );
        final verifier = AcpSecretVerifier(
          known: [prover.id],
          x25519Identity: unhex(
            (base['collector_private'] ??
                    '5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb')
                as String,
          ),
        );
        final collector = await verifier.collector(
          method,
          requestId,
          form,
          receiver.recipient,
          base['now'] as int,
        );
        for (final rawField in entry['fields'] as List) {
          final field = rawField as Map<String, dynamic>;
          final name = field['field'] as String;
          final sealed = AcpSealed.fromJson(field['sealed']);
          final opened = await receiver.open(name, sealed);
          expect(opened.text, field['value'], reason: name);
          final again = await collector.seal(
            name,
            field['value'] as String,
            random: Fixed(unhex(field['ephemeral_ikm'] as String)),
          );
          expect(again.toJson(), field['sealed'], reason: name);
        }
      }
    },
  );

  group('forms', () {
    for (final raw in (doc['forms'] as Map)['accepted'] as List) {
      final item = raw as Map<String, dynamic>;
      test('accepts ${item['name']}', () {
        final form = AcpSecretForm.parse(item['schema']);
        expect([
          for (final f in form.fields)
            if (f.secret) f.field.name,
        ], item['secret']);
        expect([
          for (final f in form.fields)
            if (!f.secret) f.field.name,
        ], item['plain']);
      });
    }
    for (final raw in (doc['forms'] as Map)['refused'] as List) {
      final item = raw as Map<String, dynamic>;
      test('refuses ${item['name']}', () {
        expect(
          () => AcpSecretForm.parse(item['schema']),
          throwsA(
            isA<AcpSecretException>().having(
              (error) => error.failure.kind,
              'kind',
              item['kind'],
            ),
          ),
        );
      });
    }
    test('limits refuse a form past them', () {
      final schema = {
        'properties': {
          for (var i = 0; i < 3; i++) 'f$i': {'type': 'string'},
        },
      };
      expect(
        () => AcpSecretForm.parse(schema, const AcpSecretLimits(fields: 2)),
        throwsA(isA<AcpSecretException>()),
      );
      expect(
        () =>
            AcpSecretForm.parse(schema, const AcpSecretLimits(schemaBytes: 10)),
        throwsA(isA<AcpSecretException>()),
      );
      expect(AcpSecretForm.parse(schema).fields, hasLength(3));
    });
  });

  group('refusals', () {
    for (final raw in doc['refusals'] as List) {
      final item = raw as Map<String, dynamic>;
      test(item['name'] as String, () async {
        await expectRefusal(
          () => item['case'] == 'verify'
              ? verifyCase(doc, item)
              : openCase(doc, item),
          item['expect'] as String,
        );
      });
    }
  });

  test(
    'a request end to end: the answer reaches the receiver, the agent sees nothing',
    () async {
      final form = AcpSecretForm.parse({
        'type': 'object',
        'properties': {
          'user': {'type': 'string'},
          'password': {'type': 'string', 'writeOnly': true},
        },
        'required': ['user', 'password'],
      });
      final prover = AcpSecretProver.ecdsaP256(
        unhex(
          'c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721',
        ),
      );
      final receiver = await AcpSecretReceiver.create(
        AcpSecretMethod.request,
        'r1',
        form,
        prover,
        1000,
        const Duration(minutes: 5),
      );
      final params = AcpSecretRequestParams(
        sessionId: 's1',
        requestId: 'r1',
        message: 'login',
        schema: {
          'type': 'object',
          'properties': {
            'user': {'type': 'string'},
            'password': {'type': 'string', 'writeOnly': true},
          },
          'required': ['user', 'password'],
        },
        recipient: receiver.recipient,
      );
      final wire = jsonDecode(jsonEncode(params.toJson()));
      final read = AcpSecretRequestParams.fromJson(wire);
      final collector = await AcpSecretVerifier(known: [prover.id]).collector(
        AcpSecretMethod.request,
        read.requestId,
        AcpSecretForm.parse(read.schema),
        read.recipient,
        1000,
      );
      final reply = AcpSecretAccepted(
        {'user': 'ana'},
        {'password': await collector.seal('password', 'correct horse')},
      );
      final text = jsonEncode(reply.toJson());
      expect(text, isNot(contains('correct horse')));
      final back =
          AcpSecretRequestReply.fromJson(
                jsonDecode(text),
                (name) => name == 'password',
              )
              as AcpSecretAccepted;
      final opened = await receiver.openValues(back.plain, back.sealed);
      expect(opened.plain, {'user': 'ana'});
      expect(opened.secrets['password']!.text, 'correct horse');
      expect(opened.secrets['password'].toString(), isNot(contains('correct')));
      // A collector that was told to send a plain password, or to leave one out, is refused.
      await expectRefusal(
        () => receiver.openValues({'user': 'ana', 'password': 'x'}, {}),
        'field',
      );
      await expectRefusal(
        () => receiver.openValues({'user': 'ana'}, {}),
        'field',
      );
    },
  );

  test('the messages read what they write, and refuse what is not theirs', () {
    expect(AcpSecretsCapability().toJson(), {
      'version': 1,
      'methods': ['request', 'show'],
      'bindings': ['ecdsa-p256-sha256', 'x25519-hmac-sha256'],
    });
    expect(
      AcpSecretsCapability.fromMeta({
        acpSecretsCapability: AcpSecretsCapability().toJson(),
      }),
      isNotNull,
    );
    expect(AcpSecretsCapability.fromMeta({}), isNull);
    expect(
      AcpSecretsCapability.fromMeta({
        acpSecretsCapability: {
          'version': 2,
          'methods': ['request'],
        },
      }),
      isNull,
    );
    expect(
      AcpSecretsCapability.fromMeta({
        acpSecretsCapability: {'version': 1, 'methods': <String>[]},
      }),
      isNull,
    );
    final open = AcpSecretShowOpen(
      sessionId: 's',
      requestId: 'r',
      message: 'm',
      schema: {
        'properties': {
          'k': {'type': 'string', 'writeOnly': true},
        },
      },
      values: {},
    );
    expect(AcpSecretShowOpen.fromJson(open.toJson()).requestId, 'r');
    expect(
      () => AcpSecretShowDeliver.fromJson(open.toJson()),
      throwsA(isA<AcpSecretException>()),
    );
    expect(acpSecretShowDoneFromJson(acpSecretShowDone(shown: true)), isTrue);
    expect(
      () => AcpSealed.fromJson({'enc': 'AA', 'ct': 'AA'}),
      throwsA(isA<AcpSecretException>()),
    );
    expect(
      () => AcpIdentityId.parse('sha256:12'),
      throwsA(isA<AcpSecretException>()),
    );
  });
}
