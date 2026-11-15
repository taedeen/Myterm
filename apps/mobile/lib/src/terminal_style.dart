import 'dart:io' show Platform;

import 'package:xterm/xterm.dart';

/// Terminal text size limits and default (logical pixels).
const double minTerminalFontSize = 8;
const double maxTerminalFontSize = 32;
double get defaultTerminalFontSize => Platform.isWindows ? 14 : 13;

/// The terminal's font.
///
/// xterm's default puts the generic name 'monospace' first. Android maps that
/// to a real font; Windows has no font of that name, and there the grid
/// collapsed to one column. Name fonts every Windows 10/11 install has.
/// integration_test/terminal_layout_test.dart checks the column count on a
/// real Windows build in CI.
TerminalStyle terminalStyle(double fontSize) {
  if (Platform.isWindows) {
    return TerminalStyle(
      fontSize: fontSize,
      height: 1.2,
      fontFamily: 'Cascadia Mono',
      fontFamilyFallback: const ['Consolas', 'Courier New', 'Segoe UI Symbol', 'Segoe UI Emoji'],
    );
  }
  return TerminalStyle(fontSize: fontSize, height: 1.2);
}
