import 'dart:convert';
import 'dart:io' show Platform;
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:xterm/xterm.dart';

import '../app_state.dart';
import '../native.dart';
import '../rust/api.dart';
import '../terminal_session.dart';
import '../terminal_style.dart';
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
  /// Sessions on screen: one, or two when split. [_focused] is the one the
  /// keyboard, key bar, suggestions and tab row act on.
  late final List<TerminalSession> _panes = [widget.session];
  int _focused = 0;

  /// Split direction: null (one pane), horizontal (side by side) or
  /// vertical (top and bottom).
  Axis? _split;

  final _controllers = [TerminalController(), TerminalController()];
  final _focusNodes = [FocusNode(), FocusNode()];

  TerminalSession get s => _panes[_focused];
  TerminalController get terminalController => _controllers[_focused];
  final _activeTab = GlobalKey();

  /// Shows [next] in the focused pane (or focuses the pane already showing it).
  void _switchTo(TerminalSession next) {
    final at = _panes.indexWhere((p) => identical(p, next));
    setState(() {
      if (at >= 0) {
        _focused = at;
      } else {
        _panes[_focused] = next;
      }
    });
    _focusPane(_focused);
    _revealActiveTab();
  }

  void _focusPane(int i) {
    if (i >= _panes.length) return;
    if (_focused != i) setState(() => _focused = i);
    WidgetsBinding.instance.addPostFrameCallback((_) => _focusNodes[i].requestFocus());
  }

  /// Off → side by side → top and bottom → off. The second pane shows
  /// another open session, or a new one to the same host if there is none.
  void _cycleSplit() {
    // Read the current session before touching _panes: `s` reads _panes,
    // and reading it after clear() threw, leaving the screen with no panes.
    final current = s;
    setState(() {
      switch (_split) {
        case null:
          final other = widget.state.sessions.where((x) => !identical(x, current)).firstOrNull ??
              widget.state.openSession(current.host);
          _panes
            ..clear()
            ..addAll([current, other]);
          _focused = 1;
          _split = Axis.horizontal;
        case Axis.horizontal:
          _split = Axis.vertical;
        case Axis.vertical:
          _panes
            ..clear()
            ..add(current);
          _focused = 0;
          _split = null;
      }
    });
    _focusPane(_focused);
  }

  /// One pane of the screen; tapping it gives it the keyboard.
  Widget _pane(int i) {
    final focused = i == _focused;
    final pane = Listener(
      onPointerDown: (_) => _focusPane(i),
      child: TerminalPane(
        terminal: _panes[i].terminal,
        key: ObjectKey(_panes[i]),
        controller: _controllers[i],
        focusNode: _focusNodes[i],
        autofocus: focused,
        style: terminalStyle(widget.state.terminalFontSize),
        onZoom: _zoom,
        onKey: _suggestionKey,
        onCopy: _copySelection,
      ),
    );
    if (_split == null) return pane;
    return DecoratedBox(
      position: DecorationPosition.foreground,
      decoration: BoxDecoration(
        border: Border.all(color: focused ? const Color(0xFF2ECC71) : Colors.white12, width: focused ? 1.5 : 1),
      ),
      child: pane,
    );
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
    for (final c in _controllers) {
      c.dispose();
    }
    for (final f in _focusNodes) {
      f.dispose();
    }
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

  Future<void> _copySelection() async {
    final sel = terminalController.selection;
    if (sel == null) return;
    final text = s.terminal.buffer.getText(sel).trimRight();
    terminalController.clearSelection();
    if (text.isEmpty) return;
    await Clipboard.setData(ClipboardData(text: text));
    _toast('Copied ${text.length} characters');
  }

  /// Pastes the clipboard into the session (bracketed paste when the shell asks for it).
  Future<void> _paste() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text;
    if (text == null || text.isEmpty || !s.isLive) return;
    s.terminal.paste(text);
  }

  /// Keyboard use of the suggestion bar: Tab / Shift+Tab move the highlight,
  /// Enter runs the highlighted one, Esc drops the highlight. Tab reaches the
  /// shell as usual when there are no suggestions (and from the key bar).
  KeyEventResult _suggestionKey(KeyEvent event) {
    final key = event.logicalKey;
    if (event is KeyUpEvent) {
      return (key == LogicalKeyboardKey.tab && s.suggestions.isNotEmpty) ||
              (key == LogicalKeyboardKey.enter && s.selected != null)
          ? KeyEventResult.handled
          : KeyEventResult.ignored;
    }
    if (key == LogicalKeyboardKey.tab && s.suggestions.isNotEmpty) {
      s.selectNext(back: HardwareKeyboard.instance.isShiftPressed);
      return KeyEventResult.handled;
    }
    final sel = s.selected;
    if (sel != null) {
      if (key == LogicalKeyboardKey.enter || key == LogicalKeyboardKey.numpadEnter) {
        s.accept(s.suggestions[sel]);
        return KeyEventResult.handled;
      }
      if (key == LogicalKeyboardKey.escape) {
        s.clearSelection();
        return KeyEventResult.handled;
      }
      s.clearSelection(); // any other key: back to plain typing
    }
    return KeyEventResult.ignored;
  }

  /// Text size: +1, -1, or 0 for the default.
  void _zoom(int step) {
    final st = widget.state;
    st.terminalFontSize = step == 0 ? defaultTerminalFontSize : st.terminalFontSize + step;
  }

  Future<void> _textSizeSheet() => showModalBottomSheet<void>(
        context: context,
        builder: (ctx) => SafeArea(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(16, 16, 16, 8),
            child: TextSizeControl(state: widget.state),
          ),
        ),
      );

  /// Closes [target] (the current session by default). Moves to a
  /// neighbouring tab, or leaves the screen when it was the last one.
  void _close([TerminalSession? target]) {
    final closing = target ?? s;
    final list = widget.state.sessions;
    final i = list.indexOf(closing);
    widget.state.closeSession(closing);
    final pane = _panes.indexWhere((p) => identical(p, closing));
    if (pane >= 0 && _panes.length > 1) {
      // Closing one half of a split: the other half takes the screen.
      setState(() {
        _panes.removeAt(pane);
        _focused = 0;
        _split = null;
      });
      _focusPane(0);
      return;
    }
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
          listenable: Listenable.merge([..._panes, widget.state]),
          builder: (context, _) => Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
            if (s.status.isNotEmpty)
              MaterialBanner(
                content: Text(s.status),
                actions: [
                  if (s.ended) TextButton(onPressed: () => s.connect(), child: const Text('Reconnect')),
                  TextButton(onPressed: _close, child: const Text('Close')),
                ],
              ),
            Expanded(
              child: _split == null
                  ? _pane(0)
                  : Flex(direction: _split!, children: [
                      Expanded(child: _pane(0)),
                      Expanded(child: _pane(1)),
                    ]),
            ),
            SuggestionBar(suggestions: s.suggestions, selected: s.selected, onAccept: s.accept),
            _SessionTabs(
              sessions: widget.state.sessions,
              current: s,
              activeKey: _activeTab,
              onBack: () => Navigator.pop(context),
              onSelect: _switchTo,
              onClose: _close,
              onNew: _newSession,
              onPaste: _paste,
              onTextSize: _textSizeSheet,
              split: _split,
              onSplit: _cycleSplit,
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
    required this.onPaste,
    required this.onTextSize,
    required this.split,
    required this.onSplit,
  });

  final List<TerminalSession> sessions;
  final TerminalSession current;
  final Key activeKey;
  final VoidCallback onBack;
  final void Function(TerminalSession) onSelect;
  final void Function(TerminalSession) onClose;
  final VoidCallback onNew;
  final VoidCallback onPaste;
  final VoidCallback onTextSize;
  final Axis? split;
  final VoidCallback onSplit;

  static final _compact = IconButton.styleFrom(
    minimumSize: const Size(33, 33),
    fixedSize: const Size(33, 33),
    padding: EdgeInsets.zero,
    tapTargetSize: MaterialTapTargetSize.shrinkWrap,
  );

  /// "user@host", with a number when the same host is open more than once.
  String _title(TerminalSession t) {
    final base = '${t.host.username.isEmpty ? '' : '${t.host.username}@'}${t.host.label}';
    final same = sessions.where((x) => x.host.id == t.host.id).toList();
    return same.length > 1 ? '$base (${same.indexOf(t) + 1})' : base;
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(4, 2, 4, 2),
      child: Row(children: [
        IconButton.filledTonal(
          style: _compact,
          iconSize: 20,
          onPressed: onBack,
          tooltip: 'Back (sessions keep running)',
          icon: const Icon(Icons.chevron_left),
        ),
        const SizedBox(width: 4),
        Expanded(
          child: SizedBox(
            height: 33, // was 44
            child: ListView.separated(
              scrollDirection: Axis.horizontal,
              itemCount: sessions.length,
              separatorBuilder: (_, __) => const SizedBox(width: 4),
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
        const SizedBox(width: 4),
        IconButton.filledTonal(
          style: _compact,
          iconSize: 18,
          onPressed: onSplit,
          tooltip: switch (split) {
            null => 'Split: side by side',
            Axis.horizontal => 'Split: top and bottom',
            Axis.vertical => 'One session',
          },
          icon: Icon(switch (split) {
            null => Icons.vertical_split,
            Axis.horizontal => Icons.horizontal_split,
            Axis.vertical => Icons.crop_square,
          }),
        ),
        const SizedBox(width: 4),
        IconButton.filledTonal(
          style: _compact,
          iconSize: 18,
          onPressed: onTextSize,
          tooltip: 'Text size (Ctrl + = / - / 0)',
          icon: const Icon(Icons.format_size),
        ),
        const SizedBox(width: 4),
        IconButton.filledTonal(
          style: _compact,
          iconSize: 18,
          onPressed: onPaste,
          tooltip: 'Paste',
          icon: const Icon(Icons.content_paste),
        ),
        const SizedBox(width: 4),
        IconButton.filledTonal(
          style: _compact,
          iconSize: 20,
          onPressed: onNew,
          tooltip: 'New session to this host',
          icon: const Icon(Icons.add),
        ),
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
          padding: const EdgeInsets.only(left: 8),
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            Icon(Icons.terminal, color: colour, size: 14),
            const SizedBox(width: 4),
            ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 180),
              child: Text(
                title,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(color: active ? green : colour.withValues(alpha: 0.8)),
              ),
            ),
            IconButton(
              constraints: const BoxConstraints.tightFor(width: 28, height: 28),
              padding: EdgeInsets.zero,
              tooltip: 'Close session',
              icon: Icon(Icons.close, size: 16, color: colour),
              onPressed: onClose,
            ),
          ]),
        ),
      ),
    );
  }
}

/// A − / size / + row with a slider and a reset. Used in Settings and in the
/// terminal's Aa sheet; changes apply to open terminals at once.
class TextSizeControl extends StatelessWidget {
  const TextSizeControl({super.key, required this.state});
  final AppState state;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: state,
      builder: (context, _) {
        final size = state.terminalFontSize;
        return Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
          Row(children: [
            const Icon(Icons.format_size),
            const SizedBox(width: 12),
            const Expanded(child: Text('Terminal text size')),
            TextButton(
              onPressed: size == defaultTerminalFontSize
                  ? null
                  : () => state.terminalFontSize = defaultTerminalFontSize,
              child: const Text('Reset'),
            ),
          ]),
          Row(children: [
            IconButton(
              tooltip: 'Smaller',
              onPressed: size <= minTerminalFontSize ? null : () => state.terminalFontSize = size - 1,
              icon: const Icon(Icons.remove),
            ),
            Expanded(
              child: Slider(
                value: size,
                min: minTerminalFontSize,
                max: maxTerminalFontSize,
                divisions: (maxTerminalFontSize - minTerminalFontSize).round(),
                label: size.round().toString(),
                onChanged: (v) => state.terminalFontSize = v,
              ),
            ),
            IconButton(
              tooltip: 'Larger',
              onPressed: size >= maxTerminalFontSize ? null : () => state.terminalFontSize = size + 1,
              icon: const Icon(Icons.add),
            ),
            SizedBox(width: 28, child: Text('${size.round()}', textAlign: TextAlign.end)),
          ]),
          Container(
            width: double.infinity,
            padding: const EdgeInsets.all(8),
            color: const Color(0xFF151827),
            child: Text(
              'user@host:~\$ ls -la',
              maxLines: 1,
              overflow: TextOverflow.clip,
              style: terminalStyle(size).toTextStyle(color: const Color(0xFFE5E5E5)),
              textScaler: TextScaler.noScaling,
            ),
          ),
        ]);
      },
    );
  }
}

/// The terminal itself plus the Copy button shown while text is selected.
/// Fills all the space it is given (integration_test/terminal_layout_test.dart).
class TerminalPane extends StatelessWidget {
  const TerminalPane({
    super.key,
    required this.terminal,
    required this.controller,
    required this.style,
    this.onZoom,
    this.onKey,
    this.onCopy,
    this.autofocus = true,
    this.focusNode,
  });

  final Terminal terminal;
  final TerminalController controller;
  final TerminalStyle style;
  /// Ctrl + = / - / 0: +1, -1, 0 (reset).
  final void Function(int step)? onZoom;

  /// Sees each key before the terminal; return handled to keep it there.
  final KeyEventResult Function(KeyEvent event)? onKey;
  final VoidCallback? onCopy;
  final bool autofocus;
  final FocusNode? focusNode;

  /// Handles the zoom keys before the terminal sees them.
  KeyEventResult _zoomKey(KeyEvent event) {
    final zoom = onZoom;
    if (zoom == null || !HardwareKeyboard.instance.isControlPressed) return KeyEventResult.ignored;
    final step = switch (event.logicalKey) {
      LogicalKeyboardKey.equal || LogicalKeyboardKey.add || LogicalKeyboardKey.numpadAdd => 1,
      LogicalKeyboardKey.minus || LogicalKeyboardKey.numpadSubtract => -1,
      LogicalKeyboardKey.digit0 || LogicalKeyboardKey.numpad0 => 0,
      _ => null,
    };
    if (step == null) return KeyEventResult.ignored;
    if (event is! KeyUpEvent) zoom(step);
    return KeyEventResult.handled;
  }

  @override
  Widget build(BuildContext context) {
    // StackFit.expand: the Stack takes the full size it is offered. With the
    // default (loose) fit it sized itself to its one unpositioned child, an
    // empty 0x0 placeholder, so on Windows the terminal got zero width and
    // drew a single centred column.
    return Stack(fit: StackFit.expand, children: [
      TerminalView(
          terminal,
          controller: controller,
          autofocus: autofocus,
          focusNode: focusNode,
          backgroundOpacity: 0,
          theme: TerminalThemes.defaultTheme,
          textStyle: style,
          // The size setting is the only scale, whatever the OS text size is.
          textScaler: TextScaler.noScaling,
          // Windows: xterm's text-input client can't attach to the window
          // ("view ID is null"), so typed characters never arrived and only
          // keys like Enter worked. Read characters from key events instead.
          hardwareKeyboardOnly: Platform.isWindows,
          onKeyEvent: (_, event) {
            final r = _zoomKey(event);
            if (r != KeyEventResult.ignored) return r;
            return onKey?.call(event) ?? KeyEventResult.ignored;
          },
        ),
      // Long-press (or drag) selects text; then offer Copy.
      Positioned(
        top: 8,
        right: 8,
        child: ListenableBuilder(
          listenable: controller,
          builder: (context, _) => controller.selection == null
              ? const SizedBox.shrink()
              : Row(mainAxisSize: MainAxisSize.min, children: [
                  FilledButton.tonalIcon(
                    onPressed: onCopy,
                    icon: const Icon(Icons.copy, size: 18),
                    label: const Text('Copy'),
                  ),
                  const SizedBox(width: 4),
                  IconButton.filledTonal(
                    tooltip: 'Clear selection',
                    onPressed: controller.clearSelection,
                    icon: const Icon(Icons.close, size: 18),
                  ),
                ]),
        ),
      ),
    ]);
  }
}
