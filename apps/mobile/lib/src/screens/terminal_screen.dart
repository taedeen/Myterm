import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:xterm/xterm.dart';

import '../app_state.dart';
import '../native.dart';
import '../rust/api.dart';
import '../terminal_session.dart';
import '../widgets/key_bar.dart';
import '../widgets/suggestion_bar.dart';

/// Opens [host]. If it already has a live session, asks whether to resume
/// it or start a new one.
Future<void> openTerminal(BuildContext context, AppState state, HostDto host) async {
  final open = state.sessions.where((s) => s.host.id == host.id && !s.ended).toList();
  TerminalSession? session;
  if (open.isEmpty) {
    session = state.openSession(host);
  } else {
    session = await showModalBottomSheet<TerminalSession>(
      context: context,
      builder: (ctx) => SafeArea(
        child: Column(mainAxisSize: MainAxisSize.min, children: [
          for (final (i, s) in open.indexed)
            ListTile(
              leading: const Icon(Icons.terminal, color: Color(0xFF2ECC71)),
              title: Text('Resume session ${i + 1}'),
              subtitle: Text(s.status.isEmpty ? 'Active' : s.status),
              onTap: () => Navigator.pop(ctx, s),
            ),
          ListTile(
            leading: const Icon(Icons.add),
            title: const Text('New session'),
            onTap: () => Navigator.pop(ctx, state.openSession(host)),
          ),
        ]),
      ),
    );
  }
  if (session != null && context.mounted) await showSession(context, state, session);
}

/// Shows an existing session's terminal.
Future<void> showSession(BuildContext context, AppState state, TerminalSession session) =>
    Navigator.of(context).push(MaterialPageRoute(builder: (_) => TerminalScreen(state: state, session: session)));

/// Displays a [TerminalSession]. Leaving this screen keeps the session
/// running; the X in the session bar ends it.
class TerminalScreen extends StatefulWidget {
  const TerminalScreen({super.key, required this.state, required this.session});
  final AppState state;
  final TerminalSession session;

  @override
  State<TerminalScreen> createState() => _TerminalScreenState();
}

class _TerminalScreenState extends State<TerminalScreen> {
  final terminalController = TerminalController();

  TerminalSession get s => widget.session;
  MyTerm get core => widget.state.core;

  @override
  void dispose() {
    terminalController.dispose();
    super.dispose();
  }

  void _toast(String msg) {
    if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(msg)));
  }

  /// Spec 3.5 C: secure "Type password".
  Future<void> typePassword(String? credentialId) async {
    final id = s.sessionId;
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
        if (!await Native.confirmUser('Type password into ${s.host.label}')) return;
        r = core.typePassword(sessionId: id, credentialId: credentialId, overridePrompt: override, confirmed: true);
      }
      final msg = switch (r) {
        InjectDto.refusedNeverAutoType => 'This credential is set to never be auto-typed.',
        InjectDto.refusedOneTimeCode => 'This prompt wants a one-time code, not a password.',
        _ => null,
      };
      if (msg != null) _toast(msg);
    } catch (e) {
      _toast('$e');
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
    final snippet = core.listSnippets().where((x) => x.id == snippetId).firstOrNull;
    if (snippet == null) return;
    final values = <String, String>{};
    for (final p in snippet.params) {
      final v = await _promptText('Value for $p');
      if (v == null) return;
      values[p] = v;
    }
    final line = core.renderSnippet(snippetId: snippetId, values: values);
    if (line != null) s.send(Uint8List.fromList(utf8.encode(line)));
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

  void _close() {
    widget.state.closeSession(s);
    Navigator.pop(context);
  }

  @override
  Widget build(BuildContext context) {
    final keybars = core.builtinKeybars();
    final profile = keybars.firstWhere(
      (k) => k.id == (s.host.keybarProfileId ?? widget.state.keybarProfileId),
      orElse: () => keybars.first,
    );
    return Scaffold(
      backgroundColor: const Color(0xFF151827),
      body: SafeArea(
        child: ListenableBuilder(
          listenable: s,
          builder: (context, _) => Column(children: [
            if (s.status.isNotEmpty)
              MaterialBanner(
                content: Text(s.status),
                actions: [
                  if (s.ended) TextButton(onPressed: () => s.connect(), child: const Text('Reconnect')),
                  TextButton(onPressed: _close, child: const Text('Close')),
                ],
              ),
            Expanded(
              child: TerminalView(
                s.terminal,
                controller: terminalController,
                autofocus: true,
                backgroundOpacity: 0,
                theme: TerminalThemes.defaultTheme,
              ),
            ),
            SuggestionBar(suggestions: s.suggestions, onAccept: s.accept),
            _SessionBar(
              title: '${s.host.username.isEmpty ? '' : '${s.host.username}@'}${s.host.label}',
              live: s.isLive,
              onBack: () => Navigator.pop(context),
              onClose: _close,
            ),
            KeyBar(
              profile: profile,
              onBytes: s.send,
              onSnippet: runSnippet,
              onTypePassword: typePassword,
            ),
          ]),
        ),
      ),
    );
  }
}

class _SessionBar extends StatelessWidget {
  const _SessionBar({required this.title, required this.live, required this.onBack, required this.onClose});
  final String title;
  final bool live;
  final VoidCallback onBack;
  final VoidCallback onClose;

  @override
  Widget build(BuildContext context) {
    final colour = live ? const Color(0xFF2ECC71) : Colors.grey;
    return Padding(
      padding: const EdgeInsets.fromLTRB(8, 4, 8, 4),
      child: Row(children: [
        IconButton.filledTonal(
          onPressed: onBack,
          tooltip: 'Back (session keeps running)',
          icon: const Icon(Icons.chevron_left),
        ),
        const SizedBox(width: 8),
        Expanded(
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
            decoration: BoxDecoration(color: const Color(0xFF173A2F), borderRadius: BorderRadius.circular(16)),
            child: Row(children: [
              Icon(Icons.terminal, color: colour, size: 18),
              const SizedBox(width: 8),
              Expanded(child: Text(title, overflow: TextOverflow.ellipsis, style: TextStyle(color: colour))),
              InkWell(onTap: onClose, child: Icon(Icons.close, color: colour)),
            ]),
          ),
        ),
      ]),
    );
  }
}
