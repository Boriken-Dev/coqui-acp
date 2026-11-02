@TestOn('vm')
library;

import 'package:coqui_acp/page.dart';
import 'package:test/test.dart';

void main() {
  test('outside a browser there is no provider', () async {
    expect(await findAcp(), isNull);
  });
}
