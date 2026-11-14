import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/widgets.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart' show ExternalLibrary;
import 'package:path_provider/path_provider.dart';
import 'package:uuid/uuid.dart';

import 'native.dart';
import 'rust/api.dart';
import 'rust/frb_generated.dart';
import 'terminal_session.dart';

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
  /// Open terminal sessions (connecting, live, or ended but not yet closed).
  /// They belong to the app, not to a screen, so they survive navigation.
  final List<TerminalSession> sessions = [];

  /// App-wide navigator, for dialogs raised by sessions in the background.
  static final navigatorKey = GlobalKey<NavigatorState>();

  int get liveCount => sessions.where((s) => s.isLive).length;
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
    for (final s in List.of(sessions)) {
      closeSession(s);
    }
    await save();
    core.lock();
    notifyListeners();
  }

  /// Creates a session for [host] and starts connecting.
  TerminalSession openSession(HostDto host) {
    final s = TerminalSession(state: this, host: host);
    sessions.add(s);
    notifyListeners();
    s.connect();
    return s;
  }

  void sessionStarted(TerminalSession s) {
    Native.keepAlive(liveCount);
    notifyListeners();
  }

  void sessionEnded(TerminalSession s) {
    Native.keepAlive(liveCount);
    save();
    notifyListeners();
  }

  /// Ends a session and removes it from the list.
  void closeSession(TerminalSession s) {
    s.disconnect();
    sessions.remove(s);
    s.release();
    Native.keepAlive(liveCount);
    notifyListeners();
  }

  int activeSessionsFor(String hostId) => sessions.where((s) => s.host.id == hostId && s.isLive).length;

  static String newId() => const Uuid().v4();

  /// Copy of [h] with a different saved credential.
  static HostDto withCredential(HostDto h, String? credentialId) => HostDto(
        id: h.id,
        label: h.label,
        address: h.address,
        port: h.port,
        username: h.username,
        groupId: h.groupId,
        credentialId: credentialId,
        hardwareKeyId: h.hardwareKeyId,
        tags: h.tags,
        os: h.os,
        keybarProfileId: h.keybarProfileId,
        hardwareKeysOnly: h.hardwareKeysOnly,
        syncHistory: h.syncHistory,
        jumpChain: h.jumpChain,
      );

  /// Saves [password] to the Keychain as [host]'s login password and links it
  /// to the host. Updates the host's existing password entry if it has one.
  /// Wipes [password] afterwards. Does not write the host itself unless
  /// [updateHost] is true.
  Future<String> saveHostPassword(HostDto host, Uint8List password, {bool updateHost = true}) async {
    final existing = core
        .listCredentials()
        .where((c) => c.id == host.credentialId && c.kind == CredentialKind.password)
        .firstOrNull;
    final id = existing?.id ?? newId();
    try {
      await core.putCredential(
        cred: CredentialDto(
          id: id,
          name: existing?.name ?? '${host.label.isEmpty ? host.address : host.label} password',
          kind: CredentialKind.password,
          confirm: existing?.confirm ?? ConfirmDto.oncePerSession,
          neverAutoType: existing?.neverAutoType ?? false,
          appendEnter: true,
          deviceOnly: existing?.deviceOnly ?? false,
        ),
        secret: password,
      );
    } finally {
      wipe(password);
    }
    if (updateHost) await core.upsertHost(host: withCredential(host, id));
    await save();
    return id;
  }
}
