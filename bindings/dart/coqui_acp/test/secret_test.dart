import 'dart:convert';
import 'dart:math';
import 'dart:typed_data';

import 'package:coqui_acp/secret.dart';
import 'package:coqui_acp/secret_unprotected.dart';
import 'package:coqui_acp/src/hpke.dart' as hpke;
import 'package:coqui_acp/src/secret.dart' as hooks;
import 'package:pointycastle/export.dart' as pc;
import 'package:test/test.dart';

import 'secret_vectors.dart';

const now = 1700000000;
const lifetime = Duration(seconds: 240);

Uint8List unhex(String text) => Uint8List.fromList([
  for (var i = 0; i < text.length; i += 2)
    int.parse(text.substring(i, i + 2), radix: 16),
]);

String hex(List<int> bytes) =>
    bytes.map((byte) => byte.toRadixString(16).padLeft(2, '0')).join();

List<int> ikm(int start) => [for (var i = start; i < start + 32; i++) i];

Uint8List flipped(List<int> bytes) =>
    Uint8List.fromList(bytes)..[bytes.length - 1] ^= 1;

AcpSecretMethod methodOf(String text) =>
    text == 'show' ? AcpSecretMethod.show : AcpSecretMethod.request;

Map<String, dynamic> find(List<dynamic> items, String name) => items
    .cast<Map<String, dynamic>>()
    .firstWhere((item) => item['name'] == name);

AcpSecretContext contextOf(
  Map<String, dynamic> base,
  Map<String, dynamic> change,
) {
  final context = base['context'] as Map<String, dynamic>;
  Object? pick(String key) =>
      change.containsKey(key) ? change[key] : context[key];
  return AcpSecretContext(
    sessionId: pick('session_id')! as String,
    requestId: pick('request_id')! as String,
    message: pick('message')! as String,
    schema: pick('schema')! as String,
    form: pick('form') as String?,
    values: pick('values') as String?,
  );
}

/// The receiver's and the sender's identities of a case.
Future<(AcpSecretProver, AcpSecretProver)> proversOf(
  Map<String, dynamic> base,
) async {
  final receiver = unhex(base['receiver_private'] as String);
  final sender = unhex(base['sender_private'] as String);
  if (base['kind'] == 'ecdsa') {
    return (ecdsaP256Prover(receiver), ecdsaP256Prover(sender));
  }
  if (base['kind'] == 'link') {
    final link = await AcpLinkKey.derive(receiver);
    return (
      AcpSecretProver.linked(link, AcpLinkRole.host),
      AcpSecretProver.linked(link, AcpLinkRole.app),
    );
  }
  return (
    await x25519Prover(receiver, await x25519Spki(sender)),
    await x25519Prover(sender, await x25519Spki(receiver)),
  );
}

Future<AcpSecretVerifier> verifierOf(
  String kind,
  String private, {
  AcpIdentityId? own,
  AcpIdentityId? trusts,
  bool noAgreement = false,
  int? maxLifetime,
  int? skew,
  String? otherSecret,
  bool receiver = false,
}) async {
  if (kind == 'link') {
    final link = await AcpLinkKey.derive(unhex(otherSecret ?? private));
    final role = receiver ? AcpLinkRole.host : AcpLinkRole.app;
    final peer = link.identity(role.other);
    final linked = trusts != null && own == null && !noAgreement;
    return AcpSecretVerifier(
      own: own ?? link.identity(role),
      known: [if (trusts != null && !linked) peer],
      link: linked ? link : null,
      linkRole: linked ? role : null,
      maxLifetime: Duration(seconds: maxLifetime ?? 300),
      skew: Duration(seconds: skew ?? 30),
    );
  }
  final secret = unhex(private);
  final spki = kind == 'ecdsa'
      ? ecdsaP256Spki(secret)
      : await x25519Spki(secret);
  final me = AcpIdentityId.of(spki);
  final agreeing = kind != 'ecdsa' && own == null && !noAgreement;
  return AcpSecretVerifier(
    own: own ?? me,
    known: [?trusts],
    agreementSpki: agreeing ? spki : null,
    agree: agreeing ? x25519Agreement(secret) : null,
    maxLifetime: Duration(seconds: maxLifetime ?? 300),
    skew: Duration(seconds: skew ?? 30),
  );
}

Future<void> expectRefusal(Future<void> Function() run, String want) async {
  try {
    await run();
  } on AcpSecretException catch (error) {
    expect(error.failure.kind, want);
    return;
  }
  if (want != 'ok') fail('expected a $want refusal');
}

Future<AcpIdentityId> verifyCase(
  Map<String, dynamic> base,
  Map<String, dynamic> change,
) async {
  final verifier = await verifierOf(
    base['kind'] as String,
    base['sender_private'] as String,
    own: change['own_id'] == null
        ? null
        : AcpIdentityId.parse(change['own_id'] as String),
    trusts: change['trusted'] == false
        ? null
        : AcpIdentityId.parse(base['receiver_id'] as String),
    noAgreement: change['no_agreement'] == true,
    maxLifetime: change['max_lifetime'] as int?,
    skew: change['skew'] as int?,
    otherSecret: change['other_secret'] as String?,
  );
  var recipient = AcpSecretRecipient.fromJson(base['recipient']);
  var key = recipient.key;
  var binding = recipient.binding;
  if (change['flip_key'] == true) key = flipped(key);
  if (change['flip_proof'] == true) {
    binding = hooks.secretWithProof(binding, flipped(binding.proof));
  }
  if (change['high_s'] == true) {
    binding = hooks.secretWithProof(binding, hooks.secretHighS(binding.proof));
  }
  if (change['binding_method'] != null ||
      change['truncate_binding_key'] == true) {
    binding = AcpSecretBinding(
      method: (change['binding_method'] ?? binding.method) as String,
      key: change['truncate_binding_key'] == true
          ? binding.key.sublist(0, 10)
          : binding.key,
      expires: binding.expires,
      audience: binding.audience,
      proof: binding.proof,
    );
  }
  recipient = AcpSecretRecipient(key, binding);
  final collector = await verifier.collector(
    change['expected'] == null
        ? AcpIdentityId.parse(base['receiver_id'] as String)
        : AcpIdentityId.parse(change['expected'] as String),
    methodOf((change['method'] ?? base['method']) as String),
    contextOf(base, change),
    recipient,
    (change['now'] ?? base['now']) as int,
  );
  return collector.holder;
}

Future<void> openCase(
  Map<String, dynamic> base,
  Map<String, dynamic> change,
) async {
  final method = methodOf(base['method'] as String);
  final kind = base['kind'] as String;
  final context = contextOf(base, const {});
  final digest = context.digest(method);
  final (proverR, proverS) = await proversOf(base);
  final idS = proverS.id;
  final ikmR = change['other_receiver'] == true
      ? ikm(100)
      : unhex(base['recipient_ikm'] as String);
  final receiverContext = change['receiver_request_id'] == null
      ? context
      : contextOf(base, {'request_id': change['receiver_request_id']});
  final receiver = await hooks.secretReceiverFromIkm(
    method,
    receiverContext,
    proverR,
    idS,
    now,
    lifetime,
    ikmR,
  );
  final recipient = AcpSecretRecipient.fromJson(base['recipient']);
  final verifierR = await verifierOf(
    kind,
    base['receiver_private'] as String,
    trusts: change['trusted'] == false ? null : idS,
    otherSecret: change['other_secret'] as String?,
    receiver: true,
  );
  final sealed = <String, AcpSealed>{
    for (final field in base['fields'] as List)
      (field as Map)['field'] as String: AcpSealed.fromJson(field['sealed']),
  };
  var auth = AcpSecretBinding.fromJson(base['auth']);
  final plainUsed = (change['plain'] ?? base['plain']) as String;
  if (change.keys.any(
    (key) =>
        key.startsWith('reseal_') ||
        key == 'seal_value' ||
        key == 'authenticated',
  )) {
    final requestId =
        (change['reseal_request_id'] ?? context.requestId) as String;
    final resealMethod = change['reseal_method'] == null
        ? method
        : methodOf(change['reseal_method'] as String);
    final resealDigest = change['reseal_message'] == null
        ? digest
        : contextOf(base, {'message': change['reseal_message']}).digest(method);
    for (final field in base['fields'] as List) {
      final name = (field as Map)['field'] as String;
      if (change['authenticated_drop'] == name) continue;
      final target = name == 'password'
          ? (change['reseal_as_field'] ?? name) as String
          : name;
      final value = name == 'pin'
          ? (change['seal_value'] ?? field['value']) as String
          : field['value'] as String;
      final made = await hpke.hpkeSeal(
        recipient.key,
        hooks.secretFieldInfo(resealMethod, requestId, resealDigest, target),
        const [],
        utf8.encode(value),
        ikm(32),
      );
      sealed[name] = AcpSealed(made.enc, made.ct);
    }
    sealed.remove(change['authenticated_drop']);
    auth = await hooks.secretBindAnswer(
      proverS,
      method,
      context,
      recipient.key,
      plainUsed,
      sealed,
      now + ((change['auth_lifetime'] ?? 240) as int),
      proverR.id,
    );
  }
  if (change['flip_ct'] != null) {
    final name = change['flip_ct'] as String;
    sealed[name] = AcpSealed(sealed[name]!.enc, flipped(sealed[name]!.ct));
  }
  if (change['flip_enc'] != null) {
    final name = change['flip_enc'] as String;
    sealed[name] = AcpSealed(flipped(sealed[name]!.enc), sealed[name]!.ct);
  }
  if (change['swap'] != null) {
    final [x as String, y as String] = change['swap'] as List;
    final vx = sealed[x]!, vy = sealed[y]!;
    sealed[x] = vy;
    sealed[y] = vx;
  }
  if (change['drop'] != null) sealed.remove(change['drop'] as String);
  await receiver.openValues(
    verifierR,
    change['expected_sender'] == null
        ? idS
        : AcpIdentityId.parse(change['expected_sender'] as String),
    plainUsed,
    sealed,
    auth,
    (change['open_now'] ?? now) as int,
  );
}

/// A receiver made for a context with one thing changed: the context itself is refused, before any binding.
Future<void> receiverCase(
  Map<String, dynamic> base,
  Map<String, dynamic> change,
) async {
  final (proverR, proverS) = await proversOf(base);
  await hooks.secretReceiverFromIkm(
    methodOf(base['method'] as String),
    contextOf(base, change),
    proverR,
    proverS.id,
    now,
    lifetime,
    unhex(base['recipient_ikm'] as String),
  );
}

/// A sender asked to sign an answer the form does not allow.
Future<void> authenticateCase(
  Map<String, dynamic> base,
  Map<String, dynamic> change,
) async {
  final method = methodOf(base['method'] as String);
  final (proverR, proverS) = await proversOf(base);
  final sender = await verifierOf(
    base['kind'] as String,
    base['sender_private'] as String,
    trusts: proverR.id,
  );
  final collector = await sender.collector(
    proverR.id,
    method,
    contextOf(base, const {}),
    AcpSecretRecipient.fromJson(base['recipient']),
    now,
  );
  final sealed = <String, AcpSealed>{
    for (final field in base['fields'] as List)
      if (change['drop'] != (field as Map)['field'])
        field['field'] as String: AcpSealed.fromJson(field['sealed']),
  };
  await collector.authenticate(
    proverS,
    (change['plain'] ?? base['plain']) as String,
    sealed,
    now,
  );
}

void main() {
  late Map<String, dynamic> doc;
  setUpAll(() async {
    doc = jsonDecode(await loadSecretVectors()) as Map<String, dynamic>;
  });

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

  test('one grammar for whole numbers, numbers and base64', () {
    for (final raw in doc['grammar'] as List) {
      final item = raw as Map<String, dynamic>;
      expect(
        hooks.secretGrammar(item['kind'] as String, item['text'] as String),
        item['valid'],
        reason: '${item['kind']} ${jsonEncode(item['text'])}',
      );
    }
  });

  test('every artifact of a case is the bytes the vector holds', () async {
    for (final raw in doc['bindings'] as List) {
      final base = raw as Map<String, dynamic>;
      final name = base['name'] as String;
      final method = methodOf(base['method'] as String);
      final context = contextOf(base, const {});
      expect(hex(context.digest(method)), base['context_digest'], reason: name);
      final (proverR, proverS) = await proversOf(base);
      expect(proverR.id.toString(), base['receiver_id']);
      expect(proverS.id.toString(), base['sender_id']);
      final link = base['link'] as Map<String, dynamic>?;
      if (link != null) {
        expect(hex(proverR.spki), link['host_tag'], reason: name);
        expect(hex(proverS.spki), link['app_tag'], reason: name);
      }
      final receiver = await hooks.secretReceiverFromIkm(
        method,
        context,
        proverR,
        proverS.id,
        now,
        lifetime,
        unhex(base['recipient_ikm'] as String),
      );
      expect(receiver.recipient.toJson(), base['recipient'], reason: name);
      expect(
        hex(
          hooks.secretKeyStatement(
            method,
            context.requestId,
            context.digest(method),
            receiver.recipient.key,
            receiver.recipient.binding.expires,
            proverS.id,
          ),
        ),
        base['key_statement'],
      );
      expect(await verifyCase(base, const {}), proverR.id, reason: name);
      final sender = await verifierOf(
        base['kind'] as String,
        base['sender_private'] as String,
        trusts: proverR.id,
      );
      final collector = await sender.collector(
        proverR.id,
        method,
        context,
        receiver.recipient,
        now,
      );
      final sealed = <String, AcpSealed>{};
      for (final rawField in base['fields'] as List) {
        final field = rawField as Map<String, dynamic>;
        final fieldName = field['field'] as String;
        final again = await hooks.secretSealWithIkm(
          collector,
          fieldName,
          field['value'] as String,
          unhex(field['ephemeral_ikm'] as String),
        );
        expect(again.toJson(), field['sealed'], reason: '$name $fieldName');
        sealed[fieldName] = again;
      }
      final plain = base['plain'] as String;
      expect(
        hex(hooks.secretValuesDigest(plain, sealed)),
        base['values_digest'],
      );
      expect(
        hex(
          hooks.secretValuesStatement(
            method,
            context.requestId,
            context.digest(method),
            receiver.recipient.key,
            plain,
            sealed,
            now + 240,
            proverR.id,
          ),
        ),
        base['values_statement'],
      );
      final auth = await collector.authenticate(proverS, plain, sealed, now);
      expect(auth.toJson(), base['auth'], reason: name);
      await openCase(base, const {});
    }
  });

  group('forms', () {
    test('accepted and refused schemas', () {
      final forms = doc['forms'] as Map<String, dynamic>;
      for (final raw in forms['accepted'] as List) {
        final item = raw as Map<String, dynamic>;
        final form = AcpSecretForm.parseText(item['schema'] as String);
        expect(
          [
            for (final f in form.fields)
              if (f.secret) f.field.name,
          ],
          item['secret'],
          reason: item['name'] as String,
        );
        expect(
          [
            for (final f in form.fields)
              if (!f.secret) f.field.name,
          ],
          item['plain'],
          reason: item['name'] as String,
        );
      }
      for (final raw in forms['refused'] as List) {
        final item = raw as Map<String, dynamic>;
        expect(
          () => AcpSecretForm.parseText(item['schema'] as String),
          throwsA(
            isA<AcpSecretException>().having(
              (error) => error.failure.kind,
              'kind',
              item['kind'],
            ),
          ),
          reason: item['name'] as String,
        );
      }
    });

    test('limits refuse a form past them', () {
      final schema = jsonEncode({
        'properties': {
          for (var i = 0; i < 3; i++) 'f$i': {'type': 'string'},
        },
      });
      expect(
        () => AcpSecretForm.parseText(schema, const AcpSecretLimits(fields: 2)),
        throwsA(isA<AcpSecretException>()),
      );
      expect(
        () => AcpSecretForm.parseText(
          schema,
          const AcpSecretLimits(schemaBytes: 10),
        ),
        throwsA(isA<AcpSecretException>()),
      );
      expect(AcpSecretForm.parseText(schema).fields, hasLength(3));
    });
  });

  test('layouts: accepted and refused', () {
    final layouts = doc['layouts'] as Map<String, dynamic>;
    for (final raw in layouts['accepted'] as List) {
      final item = raw as Map<String, dynamic>;
      AcpSecretForm.parseText(
        item['schema'] as String,
      ).checkLayout(item['layout'] as String);
    }
    for (final raw in layouts['refused'] as List) {
      final item = raw as Map<String, dynamic>;
      expect(
        () => AcpSecretForm.parseText(
          item['schema'] as String,
        ).checkLayout(item['layout'] as String),
        throwsA(
          isA<AcpSecretException>().having(
            (error) => error.failure.kind,
            'kind',
            item['kind'],
          ),
        ),
        reason: item['name'] as String,
      );
    }
  });

  test(
    'a lone surrogate in any hashed part is refused, not hashed as U+FFFD',
    () async {
      final base = (doc['bindings'] as List).first as Map<String, dynamic>;
      final method = methodOf(base['method'] as String);
      final (proverR, proverS) = await proversOf(base);
      for (final raw in doc['surrogates'] as List) {
        final item = raw as Map<String, dynamic>;
        final part = item['part'] as String;
        final lone = String.fromCharCodes([
          for (var i = 0; i < (item['units'] as String).length; i += 4)
            int.parse((item['units'] as String).substring(i, i + 4), radix: 16),
        ]);
        final bad = 'a$lone';
        final context = contextOf(base, {
          if (part != 'plain' && part != 'field_name') part: bad,
        });
        await expectLater(
          expectRefusal(() async {
            switch (part) {
              case 'plain':
                final sender = await verifierOf(
                  base['kind'] as String,
                  base['sender_private'] as String,
                  trusts: proverR.id,
                );
                final collector = await sender.collector(
                  proverR.id,
                  method,
                  context,
                  AcpSecretRecipient.fromJson(base['recipient']),
                  now,
                );
                await collector.authenticate(
                  proverS,
                  '{"user":"$bad"}',
                  const {},
                  now,
                );
              case 'field_name':
                AcpSecretForm.parseText(
                  '{"properties":{"$bad":{"type":"string"}}}',
                );
              default:
                context.digest(method);
            }
          }, item['expect'] as String),
          completes,
          reason: part,
        );
      }
      // The case the digest alone must not confuse: U+FFFD itself is a character, a lone surrogate is not.
      expect(
        contextOf(base, {'message': 'a\u{FFFD}'}).digest(method),
        isNot(equals(contextOf(base, const {}).digest(method))),
      );
    },
  );

  group('refusals', () {
    test('each changes one thing and is refused as the vector says', () async {
      for (final raw in doc['refusals'] as List) {
        final item = raw as Map<String, dynamic>;
        final base = find(doc['bindings'] as List, item['base'] as String);
        final change = item['change'] as Map<String, dynamic>;
        await expectLater(
          expectRefusal(
            () => switch (item['case']) {
              'verify' => verifyCase(base, change),
              'open' => openCase(base, change),
              'receiver' => receiverCase(base, change),
              _ => authenticateCase(base, change),
            },
            item['expect'] as String,
          ),
          completes,
          reason: item['name'] as String,
        );
      }
    });
  });

  group('signers', () {
    Future<AcpSecretProver> randomized() async {
      final scalar = unhex(
        'c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721',
      );
      final d = BigInt.parse(hex(scalar), radix: 16);
      final params = pc.ECDomainParameters('prime256v1');
      final seed = pc.FortunaRandom()
        ..seed(
          pc.KeyParameter(
            Uint8List.fromList([
              for (var i = 0; i < 32; i++) Random.secure().nextInt(256),
            ]),
          ),
        );
      return AcpSecretProver.signing(
        spki: ecdsaP256Spki(scalar),
        sign: (message) async {
          final signer = pc.ECDSASigner(pc.SHA256Digest())
            ..init(
              true,
              pc.ParametersWithRandom(
                pc.PrivateKeyParameter(pc.ECPrivateKey(d, params)),
                seed,
              ),
            );
          final signature = signer.generateSignature(message) as pc.ECSignature;
          Uint8List word(BigInt value) =>
              unhex(value.toRadixString(16).padLeft(64, '0'));
          return Uint8List.fromList([
            ...word(signature.r),
            ...word(signature.s),
          ]);
        },
      );
    }

    test(
      'a randomized signer, whichever s it gives, binds and verifies',
      () async {
        final prover = await randomized();
        final other = ecdsaP256Prover(ikm(1));
        final context = AcpSecretContext(
          sessionId: 's',
          requestId: 'r',
          message: 'm',
          schema: jsonEncode({
            'properties': {
              'k': {'type': 'string', 'writeOnly': true},
            },
          }),
        );
        final verifier = AcpSecretVerifier(own: other.id, known: [prover.id]);
        for (var i = 0; i < 12; i++) {
          final receiver = await AcpSecretReceiver.create(
            AcpSecretMethod.request,
            context,
            prover,
            other.id,
            now,
          );
          await verifier.collector(
            prover.id,
            AcpSecretMethod.request,
            context,
            receiver.recipient,
            now,
          );
        }
      },
    );

    test(
      'a low-order recipient key is a seal refusal, not an HPKE failure',
      () async {
        final prover = ecdsaP256Prover(ikm(1));
        final other = ecdsaP256Prover(ikm(2));
        final context = AcpSecretContext(
          sessionId: 's',
          requestId: 'r',
          message: 'm',
          schema: jsonEncode({
            'properties': {
              'k': {'type': 'string', 'writeOnly': true},
            },
          }),
        );
        final recipient = await hooks.secretBindKey(
          prover,
          AcpSecretMethod.request,
          context,
          other.id,
          Uint8List(32),
          now,
        );
        final collector =
            await AcpSecretVerifier(
              own: other.id,
              known: [prover.id],
            ).collector(
              prover.id,
              AcpSecretMethod.request,
              context,
              recipient,
              now,
            );
        await expectRefusal(() => collector.seal('k', 'x'), 'seal');
      },
    );
  });

  group('the in-process link', () {
    final secret = unhex(
      'a1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff00',
    );

    test('a short secret is refused and each end is its own role', () async {
      await expectRefusal(() => AcpLinkKey.derive(Uint8List(15)), 'key');
      final link = await AcpLinkKey.derive(secret);
      expect(
        link.identity(AcpLinkRole.host),
        isNot(link.identity(AcpLinkRole.app)),
      );
      expect(
        () => AcpSecretVerifier(
          own: link.identity(AcpLinkRole.host),
          link: link,
          linkRole: AcpLinkRole.app,
        ),
        throwsA(isA<AcpSecretException>()),
      );
    });

    test('a request end to end, sealed and opened by the host', () async {
      final link = await AcpLinkKey.derive(secret);
      final host = AcpSecretProver.linked(link, AcpLinkRole.host);
      final app = AcpSecretProver.linked(link, AcpLinkRole.app);
      final context = AcpSecretContext(
        sessionId: 's1',
        requestId: 'r1',
        message: 'login',
        schema: jsonEncode({
          'type': 'object',
          'properties': {
            'password': {'type': 'string', 'writeOnly': true},
          },
          'required': ['password'],
        }),
      );
      final receiver = await AcpSecretReceiver.create(
        AcpSecretMethod.request,
        context,
        host,
        app.id,
        now,
      );
      final collector =
          await AcpSecretVerifier(
            own: app.id,
            link: link,
            linkRole: AcpLinkRole.app,
          ).collector(
            host.id,
            AcpSecretMethod.request,
            context,
            receiver.recipient,
            now,
          );
      final sealed = {'password': await collector.seal('password', 'hunter2')};
      final auth = await collector.authenticate(app, '{}', sealed, now);
      final opened = await receiver.openValues(
        AcpSecretVerifier(own: host.id, link: link, linkRole: AcpLinkRole.host),
        app.id,
        '{}',
        sealed,
        auth,
        now,
      );
      expect(utf8.decode(opened.secrets['password']!.bytes), 'hunter2');
    });

    test('a binding under another link is refused at either end', () async {
      final link = await AcpLinkKey.derive(secret);
      final other = await AcpLinkKey.derive(Uint8List(32)..[0] = 9);
      final host = AcpSecretProver.linked(link, AcpLinkRole.host);
      final context = AcpSecretContext(
        sessionId: 's1',
        requestId: 'r1',
        message: 'login',
        schema: jsonEncode({
          'type': 'object',
          'properties': {
            'password': {'type': 'string', 'writeOnly': true},
          },
        }),
      );
      final receiver = await AcpSecretReceiver.create(
        AcpSecretMethod.request,
        context,
        host,
        link.identity(AcpLinkRole.app),
        now,
      );
      await expectRefusal(
        () =>
            AcpSecretVerifier(
              own: other.identity(AcpLinkRole.app),
              link: other,
              linkRole: AcpLinkRole.app,
            ).collector(
              host.id,
              AcpSecretMethod.request,
              context,
              receiver.recipient,
              now,
            ),
        'unknown_identity',
      );
      // A verifier that knows the host by its id but holds no link takes no link binding.
      await expectRefusal(
        () =>
            AcpSecretVerifier(
              own: link.identity(AcpLinkRole.app),
              known: [host.id],
            ).collector(
              host.id,
              AcpSecretMethod.request,
              context,
              receiver.recipient,
              now,
            ),
        'unsupported',
      );
    });
  });

  test('a request end to end, and the receiver opens once', () async {
    final host = ecdsaP256Prover(ikm(1));
    final frontend = ecdsaP256Prover(ikm(2));
    final context = AcpSecretContext(
      sessionId: 's1',
      requestId: 'r1',
      message: 'login',
      schema: jsonEncode({
        'type': 'object',
        'properties': {
          'user': {'type': 'string'},
          'password': {'type': 'string', 'writeOnly': true},
        },
        'required': ['user', 'password'],
      }),
    );
    final receiver = await AcpSecretReceiver.create(
      AcpSecretMethod.request,
      context,
      host,
      frontend.id,
      now,
    );
    final params = AcpSecretRequestParams.fromJson(
      jsonDecode(
        jsonEncode(
          AcpSecretRequestParams(
            context: context,
            recipient: receiver.recipient,
          ).toJson(),
        ),
      ),
    );
    final collector =
        await AcpSecretVerifier(own: frontend.id, known: [host.id]).collector(
          host.id,
          AcpSecretMethod.request,
          params.context,
          params.recipient,
          now,
        );
    final sealed = {
      'password': await collector.seal('password', 'correct horse'),
    };
    const plain = '{"user":"ana"}';
    final reply = AcpSecretAccepted(
      plain,
      sealed,
      await collector.authenticate(frontend, plain, sealed, now),
    );
    final text = jsonEncode(reply.toJson());
    expect(text, isNot(contains('correct horse')));
    final back =
        AcpSecretRequestReply.fromJson(
              jsonDecode(text),
              (name) => name == 'password',
            )
            as AcpSecretAccepted;
    final hostSide = AcpSecretVerifier(own: host.id, known: [frontend.id]);
    final opened = await receiver.openValues(
      hostSide,
      frontend.id,
      back.plain,
      back.sealed,
      back.auth,
      now,
    );
    expect(opened.plain, {'user': 'ana'});
    expect(opened.secrets['password']!.text, 'correct horse');
    expect(opened.secrets['password'].toString(), isNot(contains('correct')));
    // The same receiver does not answer twice.
    await expectRefusal(
      () => receiver.openValues(
        hostSide,
        frontend.id,
        back.plain,
        back.sealed,
        back.auth,
        now,
      ),
      'open',
    );
  });

  test('the messages read what they write, and refuse what is not theirs', () {
    expect(const AcpSecretsCapability().toJson(), {
      'version': 1,
      'methods': ['request', 'show'],
      'bindings': ['ecdsa-p256-sha256', 'x25519-hmac-sha256'],
    });
    expect(
      AcpSecretsCapability.fromMeta({
        acpSecretsCapability: const AcpSecretsCapability().toJson(),
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
    expect(acpSecretRequestMethod, '_coqui_acp/secret_request');
    expect(acpSecretShowOpenMethod, '_coqui_acp/secret_show_open');
    expect(acpSecretShowDeliverMethod, '_coqui_acp/secret_show_deliver');
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
