import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:xterm/xterm.dart';

import 'app_state.dart';
import 'native.dart';
import 'rust/api.dart';

/// One SSH session and its terminal buffer.
///
/// Owned by [AppState], not by a screen, so it keeps running (and keeps its
/// scrollback) while you look at the vault or other hosts. The terminal
/// screen only displays it. Dialogs during connect (host key, password) use
/// the app-wide navigator, so they work whichever screen is showing.
class TerminalSession extends ChangeNotifier {
  TerminalSession({required this.state, required HostDto host}) : _host = host {
    _text = const Utf8Decoder(allowMalformed: true).bind(_output.stream).listen(terminal.write);
    terminal.onOutput = (data) => send(Uint8List.fromList(utf8.encode(data)));
    terminal.onResize = (w, h, pw, ph) {
      final id = sessionId;
      if (id != null) core.resize(sessionId: id, cols: w, rows: h);
    };
  }

  final AppState state;
  final terminal = Terminal(maxLines: 10000);

  HostDto _host;
  HostDto get host => _host;

  int? sessionId;
  bool get isLive => sessionId != null;
  bool _ended = false;
  bool get ended => _ended;

  String _status = 'Connecting…';
  String get status => _status;

  List<SuggestionDto> suggestions = const [];

  StreamSubscription<SessionEventDto>? _sub;
  bool _authTried = false;

  // Server output -> UTF-8 text -> terminal, chunk by chunk. The streaming
  // decoder also handles characters split across chunks.
  final _output = StreamController<List<int>>();
  late final StreamSubscription<String> _text;

  MyTerm get core => state.core;

  BuildContext? get _ctx => AppState.navigatorKey.currentContext;

  void _setStatus(String? s) {
    _status = s ?? '';
    notifyListeners();
  }

  // ------------------------------------------------------------ connecting

  Future<void> connect({AuthChoice? auth}) async {
    _ended = false;
    _setStatus('Connecting…');
    auth ??= await _auth();
    if (auth == null) {
      _setStatus('Cancelled');
      _ended = true;
      notifyListeners();
      return;
    }
    _authTried = true;
    await _sub?.cancel();
    final stream = core.connect(
      hostId: _host.id,
      cols: terminal.viewWidth,
      rows: terminal.viewHeight,
      auth: auth,
      confirmHostKey: _confirmHostKey,
      hardwareSign: (alias, data) => Native.signWithHardwareKey(alias, data),
    );
    _sub = stream.listen(_onEvent, onError: (Object e) => _setStatus('Error: $e'));
  }

  Future<AuthChoice?> _auth() async {
    final keyId = _host.hardwareKeyId;
    if (keyId != null) {
      final keys = await Native.listHardwareKeys();
      for (final k in keys) {
        if (k.alias == keyId) {
          return AuthChoice.hardware(keyId: k.alias, publicSec1: k.publicSec1, protection: k.protection);
        }
      }
    }
    if (core.hostHasCredential(hostId: _host.id)) return const AuthChoice.stored();
    return _askPassword(retry: false);
  }

  void _onEvent(SessionEventDto ev) {
    switch (ev) {
      case SessionEventDto_Started(:final field0):
        sessionId = field0;
        // The PTY was opened before layout; send the real size now.
        core.resize(sessionId: field0, cols: terminal.viewWidth, rows: terminal.viewHeight);
        state.sessionStarted(this);
        _setStatus(null);
      case SessionEventDto_Output(:final field0):
        _output.add(field0);
      case SessionEventDto_Exit(:final field0):
        terminal.write('\r\n[process exited${field0 == null ? '' : ' with $field0'}]\r\n');
      case SessionEventDto_Closed():
        _closed('Disconnected');
      case SessionEventDto_Error(:final field0):
        if (field0.contains('authentication failed') && _host.hardwareKeyId == null && _authTried) {
          _authTried = false;
          _setStatus(field0);
          _retryWithPassword();
        } else {
          _closed(field0);
        }
    }
  }

  /// Wrong or missing password: ask again, then reconnect.
  Future<void> _retryWithPassword() async {
    final auth = await _askPassword(retry: true);
    if (auth == null) {
      _closed('Authentication failed');
      return;
    }
    await connect(auth: auth);
  }

  void _closed(String status) {
    sessionId = null;
    _ended = true;
    suggestions = const [];
    _setStatus(status);
    state.sessionEnded(this);
  }

  /// Ends the session (the X button).
  void disconnect() {
    final id = sessionId;
    if (id != null) {
      core.disconnect(sessionId: id);
    } else {
      _closed('Disconnected');
    }
  }

  /// Releases resources once the session is closed and off screen.
  void release() {
    _sub?.cancel();
    _text.cancel();
    _output.close();
  }

  // ------------------------------------------------------------ input

  void send(Uint8List bytes) {
    final id = sessionId;
    if (id == null) return;
    try {
      core.sendInput(sessionId: id, data: bytes);
    } catch (_) {
      return;
    }
    // Enter finishes a command: persist history shortly after.
    if (bytes.contains(13)) state.saveSoon();
    refreshSuggestions();
  }

  void refreshSuggestions() {
    final id = sessionId;
    if (id == null) return;
    suggestions = core.suggest(sessionId: id, limit: 6);
    notifyListeners();
  }

  void accept(SuggestionDto s) {
    final id = sessionId;
    if (id == null) return;
    core.acceptSuggestion(sessionId: id, line: s.line);
    // Run it straight away (Settings → "Run suggestions straight away"),
    // except snippets that still have {{placeholders}} to fill in.
    if (state.runSuggestionsImmediately && !s.line.contains('{{')) {
      send(Uint8List.fromList([13]));
    } else {
      refreshSuggestions();
    }
  }

  // ------------------------------------------------------------ dialogs

  Future<bool> _confirmHostKey(String host, int port, String kind, String fingerprint) async {
    final ctx = _ctx;
    if (ctx == null) return false;
    final ok = await showDialog<bool>(
      context: ctx,
      barrierDismissible: false,
      builder: (ctx) => AlertDialog(
        title: Text(kind == 'new_key_type' ? 'New host key type' : 'Unknown host'),
        content: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
          Text('$host:$port'),
          const SizedBox(height: 12),
          const Text('Check this fingerprint against the server before trusting it:'),
          const SizedBox(height: 8),
          SelectableText(fingerprint, style: const TextStyle(fontFamily: 'monospace')),
        ]),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Trust')),
        ],
      ),
    );
    return ok ?? false;
  }

  /// Asks for the host's login password. Returns null if cancelled.
  /// With "Save to Keychain" ticked the password is stored and linked to the
  /// host; otherwise it is used for this connection only.
  Future<AuthChoice?> _askPassword({required bool retry}) async {
    final ctx0 = _ctx;
    if (ctx0 == null) return null;
    final ctrl = TextEditingController();
    var save = true;
    var show = false;
    final who = '${_host.username.isEmpty ? '' : '${_host.username}@'}${_host.label}';
    final ok = await showDialog<bool>(
      context: ctx0,
      barrierDismissible: false,
      builder: (ctx) => StatefulBuilder(
        builder: (ctx, setD) => AlertDialog(
          title: Text(retry ? 'Password rejected for $who' : 'Password for $who'),
          content: Column(mainAxisSize: MainAxisSize.min, children: [
            TextField(
              controller: ctrl,
              autofocus: true,
              obscureText: !show,
              enableSuggestions: false,
              autocorrect: false,
              keyboardType: TextInputType.visiblePassword,
              decoration: InputDecoration(
                labelText: 'Password',
                suffixIcon: IconButton(
                  icon: Icon(show ? Icons.visibility_off : Icons.visibility),
                  onPressed: () => setD(() => show = !show),
                ),
              ),
              onSubmitted: (_) => Navigator.pop(ctx, true),
            ),
            CheckboxListTile(
              contentPadding: EdgeInsets.zero,
              value: save,
              title: const Text('Save to Keychain'),
              onChanged: (v) => setD(() => save = v ?? true),
            ),
          ]),
          actions: [
            TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
            FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Connect')),
          ],
        ),
      ),
    );
    final bytes = Uint8List.fromList(utf8.encode(ctrl.text));
    ctrl.clear();
    ctrl.dispose();
    if (ok != true || bytes.isEmpty) {
      wipe(bytes);
      return null;
    }
    if (save) {
      final id = await state.saveHostPassword(_host, bytes);
      _host = AppState.withCredential(_host, id);
      return const AuthChoice.stored();
    }
    return AuthChoice.password(password: bytes);
  }
}
