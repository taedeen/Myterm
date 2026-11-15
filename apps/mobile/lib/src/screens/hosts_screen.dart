import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';

import '../app_state.dart';
import '../native.dart';
import '../rust/api.dart';
import 'sync_screen.dart' show followMoveDialog;
import 'terminal_screen.dart';

class HostsScreen extends StatefulWidget {
  const HostsScreen({super.key, required this.state});
  final AppState state;

  @override
  State<HostsScreen> createState() => _HostsScreenState();
}

class _HostsScreenState extends State<HostsScreen> {
  String _query = '';

  IconData _icon(HostDto h) => switch (h.os) {
        'debian' || 'ubuntu' || 'raspbian' || 'fedora' || 'arch' => Icons.computer,
        _ => Icons.dns,
      };

  Future<void> _edit([HostDto? host]) async {
    await Navigator.of(context).push(MaterialPageRoute(builder: (_) => HostEditScreen(state: widget.state, host: host)));
    setState(() {});
  }

  // A pushed route: the home shell's rebuilds don't reach it, so listen
  // here, or hosts arriving by sync only showed after leaving and coming back.
  @override
  Widget build(BuildContext context) => ListenableBuilder(
        listenable: Listenable.merge([widget.state, widget.state.sync]),
        builder: (context, _) => _build(context),
      );

  Widget _build(BuildContext context) {
    final hosts = widget.state.core
        .listHosts()
        .where((h) =>
            _query.isEmpty ||
            '${h.label} ${h.address} ${h.tags.join(' ')}'.toLowerCase().contains(_query.toLowerCase()))
        .toList();
    final sync = widget.state.sync;
    return Scaffold(
      appBar: AppBar(title: const Text('Hosts'), actions: [
        if (sync.configured)
          IconButton(
            tooltip: 'Sync now',
            onPressed: sync.running ? null : sync.syncNow,
            icon: sync.running
                ? const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                : Icon(sync.lastError == null ? Icons.sync : Icons.sync_problem,
                    color: sync.lastError == null ? null : Colors.orange),
          ),
      ]),
      floatingActionButton: FloatingActionButton(onPressed: _edit, child: const Icon(Icons.add)),
      body: Column(children: [
        if (!sync.configured)
          const ListTile(
            dense: true,
            leading: Icon(Icons.sync_disabled, color: Colors.orange),
            title: Text('Sync is not set up on this device'),
            subtitle: Text('Hosts stay on this device until you join your vault: Settings → Sync'),
          )
        else if (sync.moved)
          ListTile(
            dense: true,
            leading: const Icon(Icons.key, color: Colors.orange),
            title: const Text('Your vault has a new key'),
            subtitle: const Text('A device was revoked elsewhere. Tap to enter your master password and keep syncing.'),
            onTap: () => followMoveDialog(context, widget.state),
          )
        else if (sync.lastError != null)
          ListTile(
            dense: true,
            leading: const Icon(Icons.sync_problem, color: Colors.orange),
            title: const Text('Sync failed'),
            subtitle: Text(sync.lastError!),
          ),
        Padding(
          padding: const EdgeInsets.all(12),
          child: TextField(
            decoration: const InputDecoration(prefixIcon: Icon(Icons.search), hintText: 'Search hosts or tags'),
            onChanged: (v) => setState(() => _query = v),
          ),
        ),
        Expanded(
          child: RefreshIndicator(
            onRefresh: sync.configured ? sync.syncNow : () async {},
            child: ListView.builder(
            physics: const AlwaysScrollableScrollPhysics(),
            itemCount: hosts.length,
            itemBuilder: (context, i) {
              final h = hosts[i];
              final active = widget.state.activeSessionsFor(h.id);
              final subtitle = active > 0
                  ? 'Active'
                  : ['ssh', if (h.username.isNotEmpty) h.username, if (h.os != null) h.os!].join(', ');
              return Card(
                child: ListTile(
                  leading: CircleAvatar(
                    backgroundColor: h.os == null ? const Color(0xFF0D47A1) : const Color(0xFFEF5350),
                    child: Icon(_icon(h), color: Colors.white),
                  ),
                  title: Text(h.label.isEmpty ? h.address : h.label),
                  subtitle: Text(subtitle),
                  trailing: active > 0 ? Text('$active', style: const TextStyle(fontSize: 18)) : null,
                  onTap: () => openTerminal(context, widget.state, h),
                  onLongPress: () => _edit(h),
                ),
              );
            },
          ),
          ),
        ),
      ]),
    );
  }
}

/// Standard SSH port, used when a host doesn't override it.
const defaultSshPort = 22;

class HostEditScreen extends StatefulWidget {
  const HostEditScreen({super.key, required this.state, this.host});
  final AppState state;
  final HostDto? host;

  @override
  State<HostEditScreen> createState() => _HostEditScreenState();
}

class _HostEditScreenState extends State<HostEditScreen> {
  late final _label = TextEditingController(text: widget.host?.label);
  late final _address = TextEditingController(text: widget.host?.address);
  // New hosts start on the standard SSH port; change it to override.
  late final _port = TextEditingController(text: (widget.host?.port ?? defaultSshPort).toString());
  final _password = TextEditingController();
  bool _showPassword = false;
  String? _portError;
  late final _user = TextEditingController(text: widget.host?.username);
  late final _tags = TextEditingController(text: widget.host?.tags.join(', '));
  late String? _credentialId = widget.host?.credentialId;
  late String? _hardwareKeyId = widget.host?.hardwareKeyId;
  late bool _hardwareOnly = widget.host?.hardwareKeysOnly ?? false;
  late bool _syncHistory = widget.host?.syncHistory ?? true;
  late bool _useCertificate = widget.host?.useCertificate ?? false;
  late bool _persistent = widget.host?.persistent ?? false;
  // One jump host is picked here; it may itself go through others.
  late String? _jumpHostId = (widget.host?.jumpChain.isNotEmpty ?? false) ? widget.host!.jumpChain.last : null;
  List<HardwareKey> _hwKeys = [];

  @override
  void initState() {
    super.initState();
    Native.listHardwareKeys().then((k) => setState(() => _hwKeys = k));
  }

  @override
  void dispose() {
    _password.clear();
    _password.dispose();
    super.dispose();
  }

  /// Blank means the default SSH port; anything else must be 1-65535.
  int? _parsePort() {
    final text = _port.text.trim();
    if (text.isEmpty) return defaultSshPort;
    final p = int.tryParse(text);
    return (p != null && p >= 1 && p <= 65535) ? p : null;
  }

  Future<void> _save() async {
    final address = _address.text.trim();
    if (address.isEmpty) return;
    final port = _parsePort();
    if (port == null) {
      setState(() => _portError = '1–65535');
      return;
    }
    final label = _label.text.trim().isEmpty ? address : _label.text.trim();
    final id = widget.host?.id ?? AppState.newId();
    if (_password.text.isNotEmpty) {
      final draft = HostDto(
        id: id,
        label: label,
        address: address,
        port: port,
        username: _user.text.trim(),
        groupId: widget.host?.groupId,
        credentialId: _credentialId,
        hardwareKeyId: _hardwareKeyId,
        tags: const [],
        os: widget.host?.os,
        keybarProfileId: widget.host?.keybarProfileId,
        hardwareKeysOnly: _hardwareOnly,
        syncHistory: _syncHistory,
        jumpChain: _jumpHostId == null ? const [] : [_jumpHostId!],
        useCertificate: _useCertificate,
        persistent: _persistent,
      );
      _credentialId = await widget.state.saveHostPassword(
        draft,
        Uint8List.fromList(utf8.encode(_password.text)),
        updateHost: false,
      );
      _password.clear();
    }
    await widget.state.core.upsertHost(
      host: HostDto(
        id: id,
        label: label,
        address: address,
        port: port,
        username: _user.text.trim(),
        groupId: widget.host?.groupId,
        credentialId: _credentialId,
        hardwareKeyId: _hardwareKeyId,
        tags: _tags.text.split(',').map((t) => t.trim()).where((t) => t.isNotEmpty).toList(),
        os: widget.host?.os,
        keybarProfileId: widget.host?.keybarProfileId,
        hardwareKeysOnly: _hardwareOnly,
        syncHistory: _syncHistory,
        jumpChain: _jumpHostId == null ? const [] : [_jumpHostId!],
        useCertificate: _useCertificate,
        persistent: _persistent,
      ),
    );
    await widget.state.save();
    if (mounted) Navigator.of(context).pop();
  }

  Future<void> _delete() async {
    await widget.state.core.deleteHost(hostId: widget.host!.id);
    await widget.state.save();
    if (mounted) Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final creds = widget.state.core.listCredentials();
    // Any other host can be a jump host, except ones that go through this one.
    final jumpHosts = widget.state.core
        .listHosts()
        .where((h) => h.id != widget.host?.id && !(widget.host != null && h.jumpChain.contains(widget.host!.id)))
        .toList();
    final hasSavedPassword = creds.any((c) => c.id == _credentialId && c.kind == CredentialKind.password);
    InputDecoration dec(String l) => InputDecoration(labelText: l, border: const OutlineInputBorder());
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.host == null ? 'New host' : 'Edit host'),
        actions: [
          if (widget.host != null) IconButton(icon: const Icon(Icons.delete_outline), onPressed: _delete),
          IconButton(icon: const Icon(Icons.check), onPressed: _save),
        ],
      ),
      body: ListView(padding: const EdgeInsets.all(16), children: [
        TextField(controller: _label, decoration: dec('Label')),
        const SizedBox(height: 12),
        TextField(controller: _address, decoration: dec('Address (IP or hostname)'), keyboardType: TextInputType.url),
        const SizedBox(height: 12),
        Row(children: [
          Expanded(child: TextField(controller: _user, decoration: dec('Username'))),
          const SizedBox(width: 12),
          SizedBox(
            width: 110,
            child: TextField(
              controller: _port,
              keyboardType: TextInputType.number,
              decoration: dec('Port').copyWith(hintText: '$defaultSshPort', errorText: _portError),
              onChanged: (_) => setState(() => _portError = null),
            ),
          ),
        ]),
        const SizedBox(height: 12),
        TextField(
          controller: _password,
          obscureText: !_showPassword,
          enableSuggestions: false,
          autocorrect: false,
          keyboardType: TextInputType.visiblePassword,
          decoration: dec('Password').copyWith(
            helperText: hasSavedPassword
                ? 'Saved in Keychain. Type a new one to replace it.'
                : 'Optional. Saved encrypted in the Keychain.',
            suffixIcon: IconButton(
              icon: Icon(_showPassword ? Icons.visibility_off : Icons.visibility),
              onPressed: () => setState(() => _showPassword = !_showPassword),
            ),
          ),
        ),
        const SizedBox(height: 12),
        TextField(controller: _tags, decoration: dec('Tags (comma separated)')),
        const SizedBox(height: 12),
        DropdownButtonFormField<String?>(
          value: _credentialId,
          decoration: dec('Or use a saved password / key'),
          items: [
            const DropdownMenuItem(value: null, child: Text('None')),
            for (final c in creds) DropdownMenuItem(value: c.id, child: Text(c.name)),
          ],
          onChanged: (v) => setState(() => _credentialId = v),
        ),
        const SizedBox(height: 12),
        DropdownButtonFormField<String?>(
          value: _hwKeys.any((k) => k.alias == _hardwareKeyId) ? _hardwareKeyId : null,
          decoration: dec('Hardware key (preferred when set)'),
          items: [
            const DropdownMenuItem(value: null, child: Text('None')),
            for (final k in _hwKeys) DropdownMenuItem(value: k.alias, child: Text('${k.alias} (${k.protection})')),
          ],
          onChanged: (v) => setState(() => _hardwareKeyId = v),
        ),
        DropdownButtonFormField<String?>(
          value: jumpHosts.any((h) => h.id == _jumpHostId) ? _jumpHostId : null,
          decoration: dec('Connect via (jump host)').copyWith(
            helperText: 'For machines only reachable through another host, e.g. server1',
          ),
          items: [
            const DropdownMenuItem(value: null, child: Text('Direct')),
            for (final h in jumpHosts)
              DropdownMenuItem(value: h.id, child: Text(h.label.isEmpty ? h.address : h.label)),
          ],
          onChanged: (v) => setState(() => _jumpHostId = v),
        ),
        SwitchListTile(
          title: const Text('Keep session running'),
          subtitle: const Text('Runs in tmux on the server: reconnects by itself after a dropped connection, '
              'and the same tab on another device picks up where you left off'),
          value: _persistent,
          onChanged: (v) => setState(() => _persistent = v),
        ),
        SwitchListTile(
          title: const Text('Sign in with MyTerm certificate'),
          subtitle: const Text('A short-lived certificate from your sync server (Settings > SSH certificate)'),
          value: _useCertificate,
          onChanged: (v) => setState(() => _useCertificate = v),
        ),
        SwitchListTile(
          title: const Text('Hardware keys only'),
          subtitle: const Text('Refuse passwords and software keys for this host'),
          value: _hardwareOnly,
          onChanged: (v) => setState(() => _hardwareOnly = v),
        ),
        SwitchListTile(
          title: const Text('Sync command history'),
          subtitle: const Text('Share this host\'s history with your other devices for autocomplete'),
          value: _syncHistory,
          onChanged: (v) => setState(() => _syncHistory = v),
        ),
      ]),
    );
  }
}
