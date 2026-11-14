import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';

import '../app_state.dart';
import '../native.dart';
import '../rust/api.dart';

/// Keychain: stored passwords and software private keys. Secrets go into
/// the Rust vault and never come back out to Dart.
class CredentialsScreen extends StatefulWidget {
  const CredentialsScreen({super.key, required this.state});
  final AppState state;

  @override
  State<CredentialsScreen> createState() => _CredentialsScreenState();
}

class _CredentialsScreenState extends State<CredentialsScreen> {
  @override
  void initState() {
    super.initState();
    Native.setSecureWindow(true);
  }

  @override
  void dispose() {
    Native.setSecureWindow(false);
    super.dispose();
  }

  Future<void> _add() async {
    await Navigator.of(context).push(MaterialPageRoute(builder: (_) => CredentialEditScreen(state: widget.state)));
    setState(() {});
  }

  @override
  Widget build(BuildContext context) {
    final creds = widget.state.core.listCredentials();
    return Scaffold(
      appBar: AppBar(title: const Text('Keychain')),
      floatingActionButton: FloatingActionButton(onPressed: _add, child: const Icon(Icons.add)),
      body: creds.isEmpty
          ? const Center(child: Text('No passwords or keys yet.'))
          : ListView(children: [
              for (final c in creds)
                Card(
                  child: ListTile(
                    leading: Icon(c.kind == CredentialKind.password ? Icons.password : Icons.vpn_key),
                    title: Text(c.name),
                    subtitle: Text([
                      c.kind == CredentialKind.password ? 'Password' : 'Private key',
                      switch (c.confirm) {
                        ConfirmDto.always => 'confirm every use',
                        ConfirmDto.oncePerSession => 'confirm once per session',
                        ConfirmDto.never => 'no confirmation',
                      },
                      if (c.neverAutoType) 'never auto-typed',
                      c.deviceOnly ? 'this device only' : 'synced',
                    ].join(' · ')),
                  ),
                ),
            ]),
    );
  }
}

class CredentialEditScreen extends StatefulWidget {
  const CredentialEditScreen({super.key, required this.state});
  final AppState state;

  @override
  State<CredentialEditScreen> createState() => _CredentialEditScreenState();
}

class _CredentialEditScreenState extends State<CredentialEditScreen> {
  final _name = TextEditingController();
  final _secret = TextEditingController();
  CredentialKind _kind = CredentialKind.password;
  ConfirmDto _confirm = ConfirmDto.oncePerSession;
  bool _neverAutoType = false;
  bool _deviceOnly = false;

  @override
  void dispose() {
    _secret.clear();
    _name.dispose();
    _secret.dispose();
    super.dispose();
  }

  Future<void> _save() async {
    if (_name.text.trim().isEmpty || _secret.text.isEmpty) return;
    final bytes = Uint8List.fromList(utf8.encode(_secret.text));
    try {
      await widget.state.core.putCredential(
        cred: CredentialDto(
          id: AppState.newId(),
          name: _name.text.trim(),
          kind: _kind,
          confirm: _confirm,
          neverAutoType: _neverAutoType,
          appendEnter: true,
          deviceOnly: _deviceOnly,
        ),
        secret: bytes,
      );
    } finally {
      wipe(bytes);
      _secret.clear();
    }
    await widget.state.save();
    if (mounted) Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    InputDecoration dec(String l) => InputDecoration(labelText: l, border: const OutlineInputBorder());
    return Scaffold(
      appBar: AppBar(
        title: const Text('New credential'),
        actions: [IconButton(icon: const Icon(Icons.check), onPressed: _save)],
      ),
      body: ListView(padding: const EdgeInsets.all(16), children: [
        SegmentedButton<CredentialKind>(
          segments: const [
            ButtonSegment(value: CredentialKind.password, label: Text('Password')),
            ButtonSegment(value: CredentialKind.privateKey, label: Text('Private key')),
          ],
          selected: {_kind},
          onSelectionChanged: (s) => setState(() => _kind = s.first),
        ),
        const SizedBox(height: 12),
        TextField(controller: _name, decoration: dec('Name (e.g. "server1 sudo")')),
        const SizedBox(height: 12),
        TextField(
          controller: _secret,
          obscureText: _kind == CredentialKind.password,
          enableSuggestions: false,
          autocorrect: false,
          maxLines: _kind == CredentialKind.password ? 1 : 8,
          decoration: dec(_kind == CredentialKind.password ? 'Password' : 'OpenSSH private key'),
        ),
        const SizedBox(height: 12),
        DropdownButtonFormField<ConfirmDto>(
          value: _confirm,
          decoration: dec('Confirm with biometrics before typing'),
          items: const [
            DropdownMenuItem(value: ConfirmDto.always, child: Text('Every time')),
            DropdownMenuItem(value: ConfirmDto.oncePerSession, child: Text('Once per session')),
            DropdownMenuItem(value: ConfirmDto.never, child: Text('Never')),
          ],
          onChanged: (v) => setState(() => _confirm = v ?? _confirm),
        ),
        SwitchListTile(
          title: const Text('Never auto-type'),
          subtitle: const Text('For high-value passwords: the Type password key refuses this one'),
          value: _neverAutoType,
          onChanged: (v) => setState(() => _neverAutoType = v),
        ),
        SwitchListTile(
          title: const Text('This device only'),
          subtitle: const Text('Keep it out of sync'),
          value: _deviceOnly,
          onChanged: (v) => setState(() => _deviceOnly = v),
        ),
      ]),
    );
  }
}
