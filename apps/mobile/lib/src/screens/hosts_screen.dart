import 'package:flutter/material.dart';

import '../app_state.dart';
import '../native.dart';
import '../rust/api.dart';
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

  @override
  Widget build(BuildContext context) {
    final hosts = widget.state.core
        .listHosts()
        .where((h) =>
            _query.isEmpty ||
            '${h.label} ${h.address} ${h.tags.join(' ')}'.toLowerCase().contains(_query.toLowerCase()))
        .toList();
    return Scaffold(
      appBar: AppBar(title: const Text('Hosts')),
      floatingActionButton: FloatingActionButton(onPressed: _edit, child: const Icon(Icons.add)),
      body: Column(children: [
        Padding(
          padding: const EdgeInsets.all(12),
          child: TextField(
            decoration: const InputDecoration(prefixIcon: Icon(Icons.search), hintText: 'Search hosts or tags'),
            onChanged: (v) => setState(() => _query = v),
          ),
        ),
        Expanded(
          child: ListView.builder(
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
      ]),
    );
  }
}

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
  late final _port = TextEditingController(text: widget.host?.port?.toString() ?? '');
  late final _user = TextEditingController(text: widget.host?.username);
  late final _tags = TextEditingController(text: widget.host?.tags.join(', '));
  late String? _credentialId = widget.host?.credentialId;
  late String? _hardwareKeyId = widget.host?.hardwareKeyId;
  late bool _hardwareOnly = widget.host?.hardwareKeysOnly ?? false;
  late bool _syncHistory = widget.host?.syncHistory ?? true;
  List<HardwareKey> _hwKeys = [];

  @override
  void initState() {
    super.initState();
    Native.listHardwareKeys().then((k) => setState(() => _hwKeys = k));
  }

  Future<void> _save() async {
    final address = _address.text.trim();
    if (address.isEmpty) return;
    await widget.state.core.upsertHost(
      host: HostDto(
        id: widget.host?.id ?? AppState.newId(),
        label: _label.text.trim().isEmpty ? address : _label.text.trim(),
        address: address,
        port: int.tryParse(_port.text.trim()),
        username: _user.text.trim(),
        groupId: widget.host?.groupId,
        credentialId: _credentialId,
        hardwareKeyId: _hardwareKeyId,
        tags: _tags.text.split(',').map((t) => t.trim()).where((t) => t.isNotEmpty).toList(),
        os: widget.host?.os,
        keybarProfileId: widget.host?.keybarProfileId,
        hardwareKeysOnly: _hardwareOnly,
        syncHistory: _syncHistory,
        jumpChain: widget.host?.jumpChain ?? const [],
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
            width: 100,
            child: TextField(controller: _port, decoration: dec('Port'), keyboardType: TextInputType.number),
          ),
        ]),
        const SizedBox(height: 12),
        TextField(controller: _tags, decoration: dec('Tags (comma separated)')),
        const SizedBox(height: 12),
        DropdownButtonFormField<String?>(
          value: _credentialId,
          decoration: dec('Password or key from Keychain'),
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
