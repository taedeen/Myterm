import 'dart:io' show Platform;

import 'package:xterm/xterm.dart';

/// Terminal text size limits and default (logical pixels).
const double minTerminalFontSize = 8;
const double maxTerminalFontSize = 32;
double get defaultTerminalFontSize => Platform.isWindows ? 14 : 13;

/// The terminal's font. On Windows, name fonts every Windows 10/11 install
/// has (Cascadia Mono, then Consolas) rather than xterm's generic default.
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
