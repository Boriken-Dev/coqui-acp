import 'dart:js_interop';

@JS('fetch')
external JSPromise<_Response> _fetch(JSString url);

extension type _Response._(JSObject _) implements JSObject {
  external JSPromise<JSString> text();
}

/// The text of `vectors/secret.json`, which a browser run serves from the test directory as `secret_vectors.json`: `tool/test_web.sh` puts it there for the run.
Future<String> loadSecretVectors() async {
  final response = await _fetch('secret_vectors.json'.toJS).toDart;
  return (await response.text().toDart).toDart;
}
