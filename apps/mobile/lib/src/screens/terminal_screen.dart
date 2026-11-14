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

  /// The session on screen; the tab row switches it.
  late TerminalSession s = widget.session;
  final _activeTab = GlobalKey();

  void _switchTo(TerminalSession next) {
    if (identical(next, s)) return;
    setState(() => s = next);
    _revealActiveTab();
  }

  void _revealActiveTab() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final ctx = _activeTab.currentContext;
      if (ctx != null) {
        Scrollable.ensureVisible(ctx, alignment: 0.5, duration: const Duration(milliseconds: 200));
      }
    });
  }

  @override
  void initState() {
    super.initState();
    _revealActiveTab();
  }

  /// Opens another session to the current host and switches to it.
  void _newSession() => _switchTo(widget.state.openSession(s.host));
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

  /// Closes [target] (the current session by default). Moves to a
  /// neighbouring tab, or leaves the screen when it was the last one.
  void _close([TerminalSession? target]) {
    final closing = target ?? s;
    final list = widget.state.sessions;
    final i = list.indexOf(closing);
    widget.state.closeSession(closing);
    if (!identical(closing, s)) {
      setState(() {});
      return;
    }
    if (list.isEmpty) {
      Navigator.pop(context);
    } else {
      _switchTo(list[i.clamp(0, list.length - 1)]);
    }
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
          listenable: Listenable.merge([s, widget.state]),
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
                key: ObjectKey(s),
                controller: terminalController,
                autofocus: true,
                backgroundOpacity: 0,
                theme: TerminalThemes.defaultTheme,
              ),
            ),
            SuggestionBar(suggestions: s.suggestions, onAccept: s.accept),
            _SessionTabs(
              sessions: widget.state.sessions,
              current: s,
              activeKey: _activeTab,
              onBack: () => Navigator.pop(context),
              onSelect: _switchTo,
              onClose: _close,
              onNew: _newSession,
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

/// Back button, a scrollable row of open sessions (tap to switch, x to
/// close), and + for another session to the current host.
class _SessionTabs extends StatelessWidget {
  const _SessionTabs({
    required this.sessions,
    required this.current,
    required this.activeKey,
    required this.onBack,
    required this.onSelect,
    required this.onClose,
    required this.onNew,
  });

  final List<TerminalSession> sessions;
  final TerminalSession current;
  final Key activeKey;
  final VoidCallback onBack;
  final void Function(TerminalSession) onSelect;
  final void Function(TerminalSession) onClose;
  final VoidCallback onNew;

  /// "user@host", with a number when the same host is open more than once.
  String _title(TerminalSession t) {
    final base = '${t.host.username.isEmpty ? '' : '${t.host.username}@'}${t.host.label}';
    final same = sessions.where((x) => x.host.id == t.host.id).toList();
    return same.length > 1 ? '$base (${same.indexOf(t) + 1})' : base;
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(8, 4, 8, 4),
      child: Row(children: [
        IconButton.filledTonal(
          onPressed: onBack,
          tooltip: 'Back (sessions keep running)',
          icon: const Icon(Icons.chevron_left),
        ),
        const SizedBox(width: 6),
        Expanded(
          child: SizedBox(
            height: 44,
            child: ListView.separated(
              scrollDirection: Axis.horizontal,
              itemCount: sessions.length,
              separatorBuilder: (_, _) => const SizedBox(width: 6),
              itemBuilder: (context, i) {
                final t = sessions[i];
                final active = identical(t, current);
                return _Tab(
                  key: active ? activeKey : ObjectKey(t),
                  title: _title(t),
                  active: active,
                  live: t.isLive,
                  onTap: () => onSelect(t),
                  onClose: () => onClose(t),
                );
              },
            ),
          ),
        ),
        const SizedBox(width: 6),
        IconButton.filledTonal(onPressed: onNew, tooltip: 'New session to this host', icon: const Icon(Icons.add)),
      ]),
    );
  }
}

class _Tab extends StatelessWidget {
  const _Tab({
    super.key,
    required this.title,
    required this.active,
    required this.live,
    required this.onTap,
    required this.onClose,
  });

  final String title;
  final bool active;
  final bool live;
  final VoidCallback onTap;
  final VoidCallback onClose;

  @override
  Widget build(BuildContext context) {
    const green = Color(0xFF2ECC71);
    final colour = live ? green : Colors.grey;
    return Material(
      color: active ? const Color(0xFF173A2F) : const Color(0xFF22263A),
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(16),
        side: active ? const BorderSide(color: green, width: 1) : BorderSide.none,
      ),
      child: InkWell(
        borderRadius: BorderRadius.circular(16),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.only(left: 12, right: 4),
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            Icon(Icons.terminal, color: colour, size: 16),
            const SizedBox(width: 6),
            ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 180),
              child: Text(
                title,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(color: active ? green : colour.withValues(alpha: 0.8)),
              ),
            ),
            IconButton(
              visualDensity: VisualDensity.compact,
              tooltip: 'Close session',
              icon: Icon(Icons.close, size: 18, color: colour),
              onPressed: onClose,
            ),
          ]),
        ),
      ),
    );
  }
}
