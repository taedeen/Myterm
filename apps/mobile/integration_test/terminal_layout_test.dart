// Runs on a real Windows build in CI (flutter test integration_test -d windows),
// where the system fonts are real, unlike in plain widget tests.
//
// Guards against the terminal grid collapsing to one column on Windows, and
// prints the font measurements behind it.
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:myterm/src/terminal_style.dart';
import 'package:xterm/xterm.dart';

/// Width of one 'm', measured the way xterm does it (unbounded layout) and
/// with a bounded layout, for comparison.
String measure(TerminalStyle style) {
  final ts = style.toTextStyle();
  String one(double layoutWidth) {
    final b = ui.ParagraphBuilder(ts.getParagraphStyle())
      ..pushStyle(ts.getTextStyle())
      ..addText('mmmmmmmmmm');
    final p = b.build()..layout(ui.ParagraphConstraints(width: layoutWidth));
    final r = 'maxIntrinsic=${p.maxIntrinsicWidth.toStringAsFixed(2)} '
        'longestLine=${p.longestLine.toStringAsFixed(2)} height=${p.height.toStringAsFixed(2)}';
    p.dispose();
    return r;
  }

  return 'infinite[${one(double.infinity)}] bounded[${one(100000)}]';
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  final styles = <String, TerminalStyle>{
    'xterm default': const TerminalStyle(),
    'MyTerm 14': terminalStyle(14),
    'MyTerm 24': terminalStyle(24),
  };

  for (final MapEntry(key: name, value: style) in styles.entries) {
    testWidgets('terminal columns: $name', (tester) async {
      final terminal = Terminal();
      await tester.pumpWidget(MaterialApp(
        home: Align(
          alignment: Alignment.topLeft,
          child: SizedBox(
            width: 800,
            height: 400,
            child: TerminalView(terminal, textStyle: style, textScaler: TextScaler.noScaling),
          ),
        ),
      ));
      await tester.pumpAndSettle();
      // ignore: avoid_print
      print('LAYOUT $name: cols=${terminal.viewWidth} rows=${terminal.viewHeight} ${measure(style)}');
      if (name.startsWith('MyTerm')) {
        // 800 px at 14 px is ~95 columns, at 24 px ~55; one column means broken.
        expect(terminal.viewWidth, greaterThan(40), reason: 'grid collapsed for $name');
      }
    });
  }
}
