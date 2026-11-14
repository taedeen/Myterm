import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:xterm/xterm.dart';

import '../app_state.dart';
import '../native.dart';
import '../rust/api.dart';
import '../widgets/key_bar.dart';
import '../widgets/suggestion_bar.dart';

/// Connects to [host] and opens its terminal.
Future<void> openTerminal(BuildContext context, AppState state, HostDto host) async {
  await Navigator.of(context).push(MaterialPageRoute(builder: (_) => TerminalScreen(state: state, host: host)));
}

class TerminalScreen extends StatefulWidget {
  const TerminalScreen({super.key, required this.state, required this.host});
  final AppState state;
  final HostDto host;

  @override
  State<TerminalScreen> createState() => _TerminalScreenState();
}

class _TerminalScreenState extends State<TerminalScreen> {
  final terminal = Terminal(maxLines: 10000);
  final terminalController = TerminalController();
  StreamSubscription<SessionEventDto>? _sub;
  int? _sessionId;
  String _status = 'Connecting…';
  List<SuggestionDto> _suggestions = const [];
  late final ByteConversionSink _decoder =
      const Utf8Decoder(allowMalformed: true).startChunkedConversion(StringConversionSink.withCallback(terminal.write));

  MyTerm get core => widget.state.core;

  @override
  void initState() {
    super.initState();
    terminal.onOutput = _onTyped;
    terminal.onResize = (w, h, pw, ph) {
      final id = _sessionId;
      if (id != null) core.resize(sessionId: id, cols: w, rows: h);
    };
    _connect();
  }

  /// The host as currently saved (updated if a password is saved on connect).
  late HostDto _host = widget.host;
  bool _authTried = false;

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
    return await _askPassword(retry: false);
  }

  /// Asks for the host's login password. Returns null if cancelled.
  /// With "Save to Keychain" ticked the password is stored and linked to the
  /// host; otherwise it is used for this connection only.
  Future<AuthChoice?> _askPassword({required bool retry}) async {
    if (!mounted) return null;
    final ctrl = TextEditingController();
    var save = true;
    var show = false;
    final ok = await showDialog<bool>(
      context: context,
      barrierDismissible: false,
      builder: (ctx) => StatefulBuilder(
        builder: (ctx, setD) => AlertDialog(
          title: Text(retry ? 'Password rejected' : 'Password for ${_host.username.isEmpty ? '' : '${_host.username}@'}${_host.label}'),
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
      final id = await widget.state.saveHostPassword(_host, bytes);
      _host = AppState.withCredential(_host, id);
      return const AuthChoice.stored();
    }
    return AuthChoice.password(password: bytes);
  }

  Future<void> _connect({AuthChoice? auth}) async {
    auth ??= await _auth();
    if (auth == null) {
      _setStatus('Cancelled');
      return;
    }
    _authTried = true;
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

  void _onEvent(SessionEventDto ev) {
    switch (ev) {
      case SessionEventDto_Started(:final field0):
        _sessionId = field0;
        widget.state.addSession(LiveSession(id: field0, host: widget.host));
        _setStatus(null);
      case SessionEventDto_Output(:final field0):
        _decoder.add(field0);
      case SessionEventDto_Exit(:final field0):
        terminal.write('\r\n[process exited${field0 == null ? '' : ' with $field0'}]\r\n');
      case SessionEventDto_Closed():
        _closed();
      case SessionEventDto_Error(:final field0):
        _setStatus(field0);
        if (field0.contains('authentication failed') && _host.hardwareKeyId == null && _authTried) {
          _authTried = false;
          _retryWithPassword();
        }
    }
  }

  /// Wrong or missing password: ask once more, then reconnect.
  Future<void> _retryWithPassword() async {
    await _sub?.cancel();
    final auth = await _askPassword(retry: true);
    if (auth == null) return;
    _setStatus('Connecting…');
    await _connect(auth: auth);
  }

  void _closed() {
    final id = _sessionId;
    if (id != null) widget.state.removeSession(id);
    _sessionId = null;
    _setStatus('Disconnected');
  }

  void _setStatus(String? s) {
    if (mounted) setState(() => _status = s ?? '');
  }

  Future<bool> _confirmHostKey(String host, int port, String kind, String fingerprint) async {
    if (!mounted) return false;
    final ok = await showDialog<bool>(
      context: context,
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

  void _onTyped(String data) => _send(Uint8List.fromList(utf8.encode(data)));

  void _send(Uint8List bytes) {
    final id = _sessionId;
    if (id == null) return;
    try {
      core.sendInput(sessionId: id, data: bytes);
    } catch (_) {
      return;
    }
    _refreshSuggestions();
  }

  void _refreshSuggestions() {
    final id = _sessionId;
    if (id == null) return;
    setState(() => _suggestions = core.suggest(sessionId: id, limit: 6));
  }

  void _accept(SuggestionDto s) {
    final id = _sessionId;
    if (id == null) return;
    core.acceptSuggestion(sessionId: id, line: s.line);
    _refreshSuggestions();
  }

  /// Spec 3.5 C: secure "Type password".
  Future<void> typePassword(String? credentialId) async {
    final id = _sessionId;
    if (id == null) return;
    var override = false;
    try {
      var r = core.typePassword(sessionId: id, credentialId: credentialId, overridePrompt: false, confirmed: false);
      if (r == InjectDto.askOverride) {
        if (!await _ask('No password prompt detected', 'Type the password anyway?')) return;
        override = true;
        r = core.typePassword(sessionId: id, credentialId: credentialId, overridePrompt: true, confirmed: false);
      }
      if (r == InjectDto.needConfirmation) {
        if (!await Native.confirmUser('Type password into ${widget.host.label}')) return;
        r = core.typePassword(sessionId: id, credentialId: credentialId, overridePrompt: override, confirmed: true);
      }
      final msg = switch (r) {
        InjectDto.refusedNeverAutoType => 'This credential is set to never be auto-typed.',
        InjectDto.refusedOneTimeCode => 'This prompt wants a one-time code, not a password.',
        _ => null,
      };
      if (msg != null && mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(msg)));
    } catch (e) {
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$e')));
    }
  }

  Future<bool> _ask(String title, String body) async =>
      await showDialog<bool>(
        context: context,
        builder: (ctx) => AlertDialog(
          title: Text(title),
          content: Text(body),
          actions: [
            TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
            FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Type it')),
          ],
        ),
      ) ??
      false;

  Future<void> runSnippet(String snippetId) async {
    final snippet = core.listSnippets().where((s) => s.id == snippetId).firstOrNull;
    if (snippet == null) return;
    final values = <String, String>{};
    for (final p in snippet.params) {
      final v = await _promptText('Value for $p');
      if (v == null) return;
      values[p] = v;
    }
    final line = core.renderSnippet(snippetId: snippetId, values: values);
    if (line != null) _send(Uint8List.fromList(utf8.encode(line)));
  }

  Future<String?> _promptText(String label) {
    final c = TextEditingController();
    return showDialog<String>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text(label),
        content: TextField(controller: c, autofocus: true),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(ctx, c.text), child: const Text('OK')),
        ],
      ),
    );
  }

  @override
  void dispose() {
    _sub?.cancel();
    final id = _sessionId;
    if (id != null) {
      core.disconnect(sessionId: id);
      final state = widget.state;
      // Not during dispose: listeners would rebuild a locked tree.
      Future.microtask(() => state.removeSession(id));
    }
    _decoder.close();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final keybars = core.builtinKeybars();
    final profile = keybars.firstWhere(
      (k) => k.id == (widget.host.keybarProfileId ?? widget.state.keybarProfileId),
      orElse: () => keybars.first,
    );
    return Scaffold(
      backgroundColor: const Color(0xFF151827),
      body: SafeArea(
        child: Column(children: [
          if (_status.isNotEmpty)
            MaterialBanner(
              content: Text(_status),
              actions: [TextButton(onPressed: () => Navigator.pop(context), child: const Text('Close'))],
            ),
          Expanded(
            child: TerminalView(
              terminal,
              controller: terminalController,
              autofocus: true,
              backgroundOpacity: 0,
              theme: TerminalThemes.defaultTheme,
            ),
          ),
          SuggestionBar(suggestions: _suggestions, onAccept: _accept),
          _SessionBar(
            title: '${widget.host.username.isEmpty ? '' : '${widget.host.username}@'}${widget.host.label}',
            onBack: () => Navigator.pop(context),
            onClose: () {
              final id = _sessionId;
              if (id != null) core.disconnect(sessionId: id);
            },
          ),
          KeyBar(
            profile: profile,
            onBytes: _send,
            onSnippet: runSnippet,
            onTypePassword: typePassword,
          ),
        ]),
      ),
    );
  }
}

class _SessionBar extends StatelessWidget {
  const _SessionBar({required this.title, required this.onBack, required this.onClose});
  final String title;
  final VoidCallback onBack;
  final VoidCallback onClose;

  @override
  Widget build(BuildContext context) {
    const green = Color(0xFF2ECC71);
    return Padding(
      padding: const EdgeInsets.fromLTRB(8, 4, 8, 4),
      child: Row(children: [
        IconButton.filledTonal(onPressed: onBack, icon: const Icon(Icons.chevron_left)),
        const SizedBox(width: 8),
        Expanded(
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
            decoration: BoxDecoration(color: const Color(0xFF173A2F), borderRadius: BorderRadius.circular(16)),
            child: Row(children: [
              const Icon(Icons.terminal, color: green, size: 18),
              const SizedBox(width: 8),
              Expanded(child: Text(title, overflow: TextOverflow.ellipsis, style: const TextStyle(color: green))),
              InkWell(onTap: onClose, child: const Icon(Icons.close, color: green)),
            ]),
          ),
        ),
      ]),
    );
  }
}
