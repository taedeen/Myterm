// Driver for integration_test/screenshots_test.dart: writes the PNGs the
// test reports to $SCREENSHOT_DIR (default: screenshots/).
import 'dart:convert';
import 'dart:io';

import 'package:integration_test/integration_test_driver.dart';

Future<void> main() => integrationDriver(responseDataCallback: (data) async {
      final shots = (data?['shots'] as Map<String, dynamic>?) ?? const {};
      final dir = Directory(Platform.environment['SCREENSHOT_DIR'] ?? 'screenshots')..createSync(recursive: true);
      for (final e in shots.entries) {
        File('${dir.path}/${e.key}.png').writeAsBytesSync(base64Decode(e.value as String));
      }
      stdout.writeln('Wrote ${shots.length} screenshots to ${dir.path}');
    });
