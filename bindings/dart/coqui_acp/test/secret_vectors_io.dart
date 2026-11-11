import 'dart:io';

/// The text of `vectors/secret.json`, read from the repository.
Future<String> loadSecretVectors() async =>
    File('../../../vectors/secret.json').readAsString();
