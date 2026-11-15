// Runs on a real Windows build in CI (flutter test integration_test -d windows),
// where the system fonts are real, unlike in plain widget tests.
//
// Guards against the terminal grid collapsing to one column (it did on
// Windows: the pane sized itself to an empty 0x0 child), and prints the font
// measurements for each style.
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:myterm/src/screens/terminal_screen.dart' show TerminalPane;
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

  // The pane as the terminal screen lays it out: under an Expanded in a
  // Column, whose cross axis gives it a loose width.
  for (final selecting in [false, true]) {
    testWidgets('TerminalPane fills the window width (selection: $selecting)', (tester) async {
      final terminal = Terminal();
      final controller = TerminalController();
      await tester.pumpWidget(MaterialApp(
        home: Align(
          alignment: Alignment.topLeft,
          child: SizedBox(
            width: 800,
            height: 400,
            child: Column(children: [
              Expanded(child: TerminalPane(terminal: terminal, controller: controller, style: terminalStyle(14))),
              const SizedBox(height: 40),
            ]),
          ),
        ),
      ));
      if (selecting) {
        terminal.write('hello world');
        controller.setSelection(terminal.buffer.createAnchor(0, 0), terminal.buffer.createAnchor(5, 0));
      }
      await tester.pumpAndSettle();
      final width = tester.getSize(find.byType(TerminalView)).width;
      // ignore: avoid_print
      print('LAYOUT pane selection=$selecting: width=$width cols=${terminal.viewWidth}');
      expect(width, 800);
      expect(terminal.viewWidth, greaterThan(80));
    });
  }
}
