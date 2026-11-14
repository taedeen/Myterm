import 'package:flutter/material.dart';

import '../app_state.dart';

class SnippetsScreen extends StatefulWidget {
  const SnippetsScreen({super.key, required this.state});
  final AppState state;

  @override
  State<SnippetsScreen> createState() => _SnippetsScreenState();
}

class _SnippetsScreenState extends State<SnippetsScreen> {
  Future<void> _edit({String? id, String name = '', String body = ''}) async {
    final n = TextEditingController(text: name);
    final b = TextEditingController(text: body);
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text(id == null ? 'New snippet' : 'Edit snippet'),
        content: Column(mainAxisSize: MainAxisSize.min, children: [
          TextField(controller: n, decoration: const InputDecoration(labelText: 'Name')),
          TextField(
            controller: b,
            decoration: const InputDecoration(labelText: 'Command', helperText: 'Use {{name}} for values asked at run time'),
            style: const TextStyle(fontFamily: 'monospace'),
            maxLines: 3,
          ),
        ]),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Save')),
        ],
      ),
    );
    if (ok == true && b.text.trim().isNotEmpty) {
      await widget.state.core.upsertSnippet(id: id ?? AppState.newId(), name: n.text.trim(), body: b.text);
      await widget.state.save();
      setState(() {});
    }
  }

  @override
  Widget build(BuildContext context) {
    final snippets = widget.state.core.listSnippets();
    return Scaffold(
      appBar: AppBar(title: const Text('Snippets')),
      floatingActionButton: FloatingActionButton(onPressed: _edit, child: const Icon(Icons.add)),
      body: ListView(children: [
        for (final s in snippets)
          Card(
            child: ListTile(
              title: Text(s.name.isEmpty ? s.body : s.name),
              subtitle: Text(s.body, style: const TextStyle(fontFamily: 'monospace')),
              onTap: () => _edit(id: s.id, name: s.name, body: s.body),
            ),
          ),
      ]),
    );
  }
}
