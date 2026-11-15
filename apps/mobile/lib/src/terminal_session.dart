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

  /// Suggestion picked with Tab (keyboard), or null. Enter runs it.
  int? selected;

  /// Tab / Shift+Tab: moves the highlight through the suggestions.
  void selectNext({bool back = false}) {
    if (suggestions.isEmpty) return;
    final n = suggestions.length;
    final cur = selected;
    selected = cur == null ? (back ? n - 1 : 0) : (cur + (back ? n - 1 : 1)) % n;
    notifyListeners();
  }

  void clearSelection() {
    if (selected == null) return;
    selected = null;
    notifyListeners();
  }

  StreamSubscription<SessionEventDto>? _sub;
  bool _authTried = false;

  /// For hosts with "Keep session running" (tmux): this tab's server-side
  /// session, e.g. myterm-1. Set on first connect and kept for reconnects.
  String? tmuxName;
  bool get persistent => _host.persistent == true;

  // Reconnecting after a dropped connection (persistent hosts only).
  bool _wasLive = false; // reached Started at least once since connect()
  bool _exited = false; // the remote program ended (exit, tmux detach)
  bool _userClosed = false;
  AuthChoice? _reusableAuth; // stored / hardware: safe to reuse unprompted
  Timer? _retry;
  int _attempt = 0;
  static const _backoff = [1, 2, 5, 10, 20, 30];
  static const _maxAttempts = 30;

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
    _retry?.cancel();
    _ended = false;
    _exited = false;
    _userClosed = false;
    _wasLive = false;
    _setStatus(_attempt > 0 ? 'Reconnecting… (attempt $_attempt)' : 'Connecting…');
    // Hosts that sign in with a MyTerm certificate: renew it first if needed.
    if (_usesCertificate()) {
      try {
        await state.sync.renewCertificate();
      } catch (_) {
        // Connect anyway: an unexpired certificate still works, and the
        // connect error says what's missing otherwise.
      }
    }
    auth ??= _reusableAuth ?? await _auth();
    if (auth == null) {
      _setStatus('Cancelled');
      _ended = true;
      notifyListeners();
      return;
    }
    _authTried = true;
    _reusableAuth = auth is AuthChoice_Password ? null : auth;
    if (persistent) tmuxName ??= state.tmuxNameFor(this);
    await _sub?.cancel();
    final stream = core.connect(
      hostId: _host.id,
      cols: terminal.viewWidth,
      rows: terminal.viewHeight,
      auth: auth,
      tmuxSession: persistent ? tmuxName : null,
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
        _wasLive = true;
        _attempt = 0;
        // The PTY was opened before layout; send the real size now.
        core.resize(sessionId: field0, cols: terminal.viewWidth, rows: terminal.viewHeight);
        state.sessionStarted(this);
        _setStatus(null);
      case SessionEventDto_Output(:final field0):
        _output.add(field0);
      case SessionEventDto_Exit(:final field0):
        _exited = true;
        terminal.write('\r\n[process exited${field0 == null ? '' : ' with $field0'}]\r\n');
      case SessionEventDto_Closed():
        if (!_maybeReconnect('Connection lost')) _closed('Disconnected');
      case SessionEventDto_Error(:final field0):
        if (field0.contains('authentication failed') && _host.hardwareKeyId == null && _authTried) {
          _authTried = false;
          _setStatus(field0);
          _retryWithPassword();
        } else if (!_maybeReconnect(field0)) {
          _closed(field0);
        }
    }
  }

  bool _usesCertificate() {
    final hosts = {for (final h in core.listHosts()) h.id: h};
    final ids = [..._host.jumpChain, _host.id];
    for (var i = 0; i < ids.length; i++) {
      final h = hosts[ids[i]];
      if (h == null) continue;
      if (h.useCertificate == true) return true;
      for (final j in h.jumpChain) {
        if (!ids.contains(j)) ids.add(j);
      }
    }
    return false;
  }

  /// A persistent session that dropped (network change, sleep, server
  /// restart) reconnects by itself and reattaches to its tmux session.
  /// Not when the remote program ended, the user closed it, or connecting
  /// would need a typed password.
  bool _maybeReconnect(String reason) {
    final retrying = _attempt > 0;
    if (!persistent || _exited || _userClosed || (!_wasLive && !retrying) || _reusableAuth == null) return false;
    if (_attempt >= _maxAttempts) return false;
    sessionId = null;
    suggestions = const [];
    final delay = _backoff[_attempt.clamp(0, _backoff.length - 1)];
    _attempt++;
    if (!retrying) terminal.write('\r\n[connection lost: reconnecting]\r\n');
    _setStatus('$reason. Reconnecting in ${delay}s… (attempt $_attempt)');
    _retry?.cancel();
    _retry = Timer(Duration(seconds: delay), () => connect());
    state.sessionEnded(this);
    return true;
  }

  /// Skips the wait before the next reconnect attempt (e.g. app back in front).
  void reconnectNow() {
    if (_retry?.isActive ?? false) {
      _retry!.cancel();
      connect();
    }
  }

  bool get reconnecting => _retry?.isActive ?? false;

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
    _attempt = 0;
    _retry?.cancel();
    suggestions = const [];
    _setStatus(status);
    state.sessionEnded(this);
  }

  /// Ends the session (the X button).
  void disconnect() {
    _userClosed = true;
    _retry?.cancel();
    _attempt = 0;
    final id = sessionId;
    if (id != null) {
      core.disconnect(sessionId: id);
    } else {
      _closed('Disconnected');
    }
  }

  /// Releases resources once the session is closed and off screen.
  void release() {
    _retry?.cancel();
    _sub?.cancel();
    _text.cancel();
    _output.close();
  }

  // ------------------------------------------------------------ input

  /// Typing, key bar, paste and snippets. With Broadcast on, the same
  /// bytes also go to the other sessions in the group.
  void send(Uint8List bytes, {bool mirror = true}) {
    if (mirror && state.broadcasting && state.broadcast.contains(this)) {
      for (final other in state.broadcast) {
        if (!identical(other, this)) other.send(bytes, mirror: false);
      }
    }
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
    selected = null;
    notifyListeners();
  }

  void accept(SuggestionDto s) {
    final id = sessionId;
    if (id == null) return;
    core.acceptSuggestion(sessionId: id, line: s.line);
    if (state.broadcasting && state.broadcast.contains(this)) {
      // Others get the whole line: clear what's typed there (Ctrl-U), then type it.
      final line = Uint8List.fromList([0x15, ...utf8.encode(s.line)]);
      for (final other in state.broadcast) {
        if (!identical(other, this)) other.send(line, mirror: false);
      }
    }
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
