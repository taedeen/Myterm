import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';
import 'package:uuid/uuid.dart';

import 'native.dart';
import 'rust/api.dart';
import 'rust/frb_generated.dart';

/// A live terminal session the app is tracking.
class LiveSession {
  LiveSession({required this.id, required this.host});
  final int id;
  final HostDto host;
}

/// Owns the Rust core, the on-disk store and vault unlock state.
///
/// Files (app-private storage):
/// - `meta.json`: device id, password-wrapped and device-wrapped master key,
///   Argon2id parameters. Contains no plaintext secrets.
/// - `store.bin`: vault records (ciphertext only).
class AppState extends ChangeNotifier {
  late final MyTerm core;
  late final Directory _dir;
  Map<String, dynamic> _meta = {};
  final List<LiveSession> sessions = [];
  String _keybarProfileId = 'builtin-tmux';
  String get keybarProfileId => _keybarProfileId;
  set keybarProfileId(String id) {
    _keybarProfileId = id;
    notifyListeners();
  }

  bool get hasVault => _meta['passwordWrapped'] != null;
  bool get hasBiometricUnlock => _meta['deviceWrapped'] != null;
  bool get unlocked => core.isUnlocked();

  File get _metaFile => File('${_dir.path}/meta.json');
  File get _storeFile => File('${_dir.path}/store.bin');

  Future<void> init() async {
    await RustLib.init(externalLibrary: ExternalLibrary.open('libmyterm_ffi.so'));
    _dir = await getApplicationSupportDirectory();
    if (await _metaFile.exists()) {
      _meta = jsonDecode(await _metaFile.readAsString()) as Map<String, dynamic>;
    }
    _meta['deviceId'] ??= const Uuid().v4();
    core = MyTerm(deviceId: _meta['deviceId'] as String);
    if (await _storeFile.exists()) {
      await core.importStore(store: await _storeFile.readAsBytes());
    }
  }

  Future<void> _writeMeta() async {
    final tmp = File('${_metaFile.path}.tmp');
    await tmp.writeAsString(jsonEncode(_meta), flush: true);
    await tmp.rename(_metaFile.path);
  }

  Future<void> save() async {
    if (!unlocked) return;
    final blobs = await core.exportStore();
    final tmp = File('${_storeFile.path}.tmp');
    await tmp.writeAsBytes(blobs.store, flush: true);
    await tmp.rename(_storeFile.path);
  }

  /// First run. Returns false if biometric unlock could not be set up
  /// (the vault still works with the master password).
  Future<bool> createVault(String masterPassword) async {
    final setup = await core.createVault(masterPassword: masterPassword, mobile: true);
    _meta
      ..['passwordWrapped'] = base64Encode(setup.passwordWrapped)
      ..['salt'] = base64Encode(setup.kdfSalt)
      ..['m'] = setup.kdfMKib
      ..['t'] = setup.kdfT
      ..['p'] = setup.kdfP;
    final mk = setup.masterKeyForDeviceWrap;
    Uint8List? wrapped;
    try {
      wrapped = await Native.deviceWrap(mk);
    } catch (_) {
      wrapped = null;
    } finally {
      wipe(mk);
    }
    if (wrapped != null) _meta['deviceWrapped'] = base64Encode(wrapped);
    await _writeMeta();
    await save();
    notifyListeners();
    return wrapped != null;
  }

  Future<bool> unlockWithBiometrics() async {
    final wrapped = _meta['deviceWrapped'] as String?;
    if (wrapped == null) return false;
    final mk = await Native.deviceUnwrap(base64Decode(wrapped));
    if (mk == null) return false;
    try {
      await core.unlockWithMasterKey(masterKey: mk);
    } finally {
      wipe(mk);
    }
    // Re-import so history (sealed in the vault) loads now that it can be decrypted.
    if (await _storeFile.exists()) {
      await core.importStore(store: await _storeFile.readAsBytes());
    }
    notifyListeners();
    return true;
  }

  Future<void> unlockWithPassword(String masterPassword) async {
    await core.unlockWithPassword(
      masterPassword: masterPassword,
      passwordWrapped: base64Decode(_meta['passwordWrapped'] as String),
      kdfSalt: base64Decode(_meta['salt'] as String),
      kdfMKib: _meta['m'] as int,
      kdfT: _meta['t'] as int,
      kdfP: _meta['p'] as int,
    );
    if (await _storeFile.exists()) {
      await core.importStore(store: await _storeFile.readAsBytes());
    }
    notifyListeners();
  }

  Future<void> lock() async {
    await save();
    core.lock();
    notifyListeners();
  }

  void addSession(LiveSession s) {
    sessions.add(s);
    Native.keepAlive(sessions.length);
    notifyListeners();
  }

  void removeSession(int id) {
    sessions.removeWhere((s) => s.id == id);
    Native.keepAlive(sessions.length);
    save();
    notifyListeners();
  }

  int activeSessionsFor(String hostId) => sessions.where((s) => s.host.id == hostId).length;

  static String newId() => const Uuid().v4();
}
