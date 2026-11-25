import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../connection/desktop_settings.dart';
import '../connection/directory_origin.dart';
import 'native_desktop.dart';

final class HostRequest {
  const HostRequest(
    this.id,
    this.peer,
    this.canControl,
    this.displays,
    this.capabilities,
    this.directory,
    this.controller,
  );
  final int id;
  final String peer;
  final bool canControl;
  final bool displays;
  final int capabilities;
  final bool directory;
  final String? controller;
}

final class HostSource {
  const HostSource(this.id, this.label);
  final String id, label;
}

final class HostController extends ChangeNotifier {
  HostController(this.native, {required this.defaultDirectory});

  final NativeDesktop native;
  final String defaultDirectory;
  List<String> interfaces = const ['127.0.0.1'];
  List<HostSource> sources = const [];
  DesktopSettings settings = const DesktopSettings();
  DesktopSettings? _deferred;
  HostRequest? request;
  String? peer, grant, issue, lanIssue, shareIssue, code, identity, endpoint;
  String? directoryDevice, directoryStatus, directoryCode, directoryIdentity;
  int activeCapabilities = 0;
  bool unattendedSession = false;
  bool loading = true;
  bool applying = false;
  bool _settingsLoaded = false;
  bool _disposed = false;
  bool _rechecking = false;
  int _nextRequest = 0;
  int? _activeRequest, _directoryGeneration;
  bool? _activeDirectory, _approvingDirectory;
  Timer? _approvalTimer, _networkTimer;
  _HostProcess? _current;
  String? _resolvedAddress;

  String? get directoryOrigin => _settingsLoaded && native.supportsDirectory
      ? settings.activeDirectory(defaultDirectory)
      : null;
  bool get hasDeferredSettings => _deferred != null;
  DesktopSettings get settingsDraft => _deferred ?? settings;

  static String get _executable {
    const configured = String.fromEnvironment('NEXDESK_AGENT_PATH');
    if (configured.isNotEmpty) return configured;
    final directory = File(Platform.resolvedExecutable).parent.path;
    final separator = Platform.pathSeparator;
    final name = Platform.isWindows ? 'nexdesk-agent.exe' : 'nexdesk-agent';
    return '$directory$separator$name';
  }

  Future<void> initialize() async {
    try {
      final saved = await native.local({'operation': 'desktop_preferences'});
      if (_disposed) return;
      settings = DesktopSettings.fromJson(
        (saved as Map).cast<String, Object?>(),
      );
      _settingsLoaded = true;
      await refreshDevices();
      if (!_disposed) await _apply(settings, save: false);
    } on StateError catch (error) {
      issue = error.message.toString();
    } on TimeoutException {
      issue = '准备本机连接超时。';
    } on Exception {
      issue = '无法加载本机设置。';
    } finally {
      loading = false;
      _notify();
      if (!_disposed) {
        _networkTimer = Timer.periodic(
          const Duration(seconds: 15),
          (_) => unawaited(_refreshNetwork()),
        );
      }
    }
  }

  Future<void> refreshDevices() async {
    final (addresses, displays) = await (_interfaces(), _sources()).wait;
    if (_disposed) return;
    interfaces = addresses;
    sources = displays;
    _notify();
  }

  Future<void> _refreshNetwork() async {
    if (loading ||
        applying ||
        _disposed ||
        _rechecking ||
        !_settingsLoaded ||
        (!settings.lanAllowed && directoryOrigin == null)) {
      return;
    }
    _rechecking = true;
    try {
      final addresses = await _interfaces();
      if (applying || _disposed) return;
      interfaces = addresses;
      if (_current == null && sources.isEmpty) {
        sources = await _sources();
        if (_disposed) return;
      }
      final address = _addressFor(settings);
      if (_current != null &&
          address == _resolvedAddress &&
          (endpoint != null || address == null)) {
        return;
      }
      await _apply(settings, save: false);
    } on StateError catch (error) {
      issue = error.message.toString();
    } on TimeoutException {
      lanIssue = '正在等待本机监听更新。';
    } finally {
      _rechecking = false;
      _notify();
      _applyDeferredWhenIdle();
    }
  }

  static Future<List<String>> _interfaces() async {
    try {
      final adapters = await NetworkInterface.list(
        includeLoopback: true,
        includeLinkLocal: false,
      );
      final preferred = <String>{};
      final alternatives = <String>{};
      final virtual = RegExp(
        r'vethernet|virtual|vmware|vbox|docker|wsl|tunnel',
        caseSensitive: false,
      );
      for (final adapter in adapters) {
        for (final ip in adapter.addresses) {
          final octets = ip.rawAddress;
          final ipv4 = ip.type == InternetAddressType.IPv4;
          final private = ipv4
              ? octets[0] == 10 ||
                    (octets[0] == 172 && octets[1] >= 16 && octets[1] <= 31) ||
                    (octets[0] == 192 && octets[1] == 168)
              : (octets[0] & 0xfe) == 0xfc;
          if (private) {
            (ipv4 && !virtual.hasMatch(adapter.name) ? preferred : alternatives)
                .add(ip.address);
          }
        }
      }
      return {
        ...preferred,
        ...alternatives,
        '127.0.0.1',
        '::1',
      }.toList(growable: false);
    } on SocketException {
      return const ['127.0.0.1'];
    }
  }

  static Future<List<HostSource>> _sources() async {
    final sources = <HostSource>[];
    try {
      final result = await Process.run(
        _executable,
        ['sources', 'screen'],
        stdoutEncoding: utf8,
        stderrEncoding: utf8,
      ).timeout(const Duration(seconds: 6));
      if (result.exitCode == 0) {
        final options = (jsonDecode(result.stdout as String) as List)
            .whereType<Map>();
        for (final (index, source) in options.indexed) {
          final title = source['title'] as String;
          final id = source['id'];
          sources.add(
            HostSource(
              'screen:$id',
              title.isEmpty ? '显示器 ${index + 1}' : title,
            ),
          );
        }
      }
    } on ProcessException {
      return const [];
    } on TimeoutException {
      return const [];
    } on FormatException {
      return const [];
    }
    if (Platform.isMacOS ||
        Platform.isLinux &&
            Platform.environment.containsKey('WAYLAND_DISPLAY')) {
      sources.add(const HostSource('picker', '使用系统选屏窗口'));
    }
    return sources;
  }

  String? _addressFor(DesktopSettings config) {
    if (!config.lanAllowed) return null;
    if (config.address.isEmpty) return interfaces.firstOrNull;
    return interfaces.contains(config.address) ? config.address : null;
  }

  String _sourceFor(DesktopSettings config) =>
      sources.any((source) => source.id == config.source)
      ? config.source
      : sources.firstOrNull?.id ?? '';

  Future<String?> checkDirectory(String origin) async {
    try {
      final result = await native.local({
        'operation': 'directory_health',
        'origin': origin,
      });
      return switch (result) {
        'ready' => null,
        'timeout' => '目录响应超时。请检查域名、端口和网络。',
        'tls' => 'HTTPS 握手失败。请核对域名和证书信任。',
        'incompatible' => '目录服务和当前客户端的协议版本不兼容。',
        'invalid_response' => '该地址没有返回 NexDesk 目录健康状态。',
        _ => '无法访问目录服务。请检查域名、端口和网络。',
      };
    } on StateError {
      return '无法检查此目录，请核对 HTTPS 地址。';
    }
  }

  bool affectsCurrentSession(DesktopSettings config) {
    final mode = _activeDirectory ?? _approvingDirectory ?? request?.directory;
    if (mode == null) return false;
    if (_sourceFor(config) != _sourceFor(settings) ||
        config.shareRoot != settings.shareRoot ||
        !config.permissions.incoming.permits(settings.permissions.incoming) ||
        unattendedSession && config.unattended != settings.unattended) {
      return true;
    }
    if (mode) {
      return config.activeDirectory(defaultDirectory) != directoryOrigin ||
          config.allowShell != settings.allowShell;
    }
    return _addressFor(config) != _addressFor(settings) ||
        (config.lanAllowed && config.port != settings.port);
  }

  Future<String?> saveSettings(
    DesktopSettings config, {
    bool afterSession = false,
  }) async {
    if (loading || applying || _rechecking || _disposed) {
      return '正在处理其他设置，请稍后再试。';
    }
    applying = true;
    _notify();
    try {
      final origin = config.activeDirectory(defaultDirectory);
      if (origin != null && origin != directoryOrigin) {
        final error = await checkDirectory(origin);
        if (error != null) return error;
      }
      if (_disposed) return '应用已关闭。';
      if (afterSession && affectsCurrentSession(config)) {
        _deferred = config;
        return null;
      }
      await _apply(config, save: true);
      _deferred = null;
      return null;
    } on StateError catch (error) {
      return error.message.toString();
    } on TimeoutException {
      return '更新接收设置超时，请重试。';
    } finally {
      applying = false;
      _notify();
    }
  }

  void cancelDeferredSettings() {
    _deferred = null;
    _notify();
  }

  Future<void> _apply(DesktopSettings config, {required bool save}) async {
    final source = _sourceFor(config);
    final address = _addressFor(config);
    if (source.isEmpty && _current == null) {
      if (save) await _saveWithoutReceiver(config);
      issue = '没有可用的共享画面。请检查显示器、系统录屏权限和被控程序。';
      return;
    }
    final _HostProcess receiver;
    try {
      receiver = await _launch();
    } on StateError catch (error) {
      if (!save || _current != null) rethrow;
      await _saveWithoutReceiver(config);
      issue = error.message.toString();
      return;
    }
    if (_disposed) return;
    final id = ++_nextRequest;
    final response = await receiver.apply(id, {
      'settings': config.toJson(),
      'default_directory': native.supportsDirectory
          ? normalizeDirectoryOrigin(defaultDirectory) ?? ''
          : '',
      'lan_address': address,
      'source': source,
      'save': save,
    });
    if (_disposed || !identical(receiver, _current)) return;
    if (response['error'] case final String error) {
      throw StateError(_settingsError(error));
    }
    if (save) {
      final value = response['settings'] as Map;
      settings = DesktopSettings.fromJson(value.cast<String, Object?>());
      _settingsLoaded = true;
    }
    _resolvedAddress = address;
    final generation = response['generation'] as int?;
    if (_directoryGeneration != generation) {
      directoryDevice = null;
      directoryStatus = generation == null ? null : 'connecting';
      _directoryGeneration = generation;
      if (receiver.presence case final status?) _setPresence(status);
    }
    endpoint = response['endpoint'] as String?;
    lanIssue = _receiverIssue(response['lan_issue']);
    shareIssue = _receiverIssue(response['share_issue']);
    issue = null;
  }

  Future<void> _saveWithoutReceiver(DesktopSettings config) async {
    final saved = await native.local({
      'operation': 'save_desktop_preferences',
      'settings': config.toJson(),
    });
    settings = DesktopSettings.fromJson((saved as Map).cast<String, Object?>());
    _settingsLoaded = true;
  }

  Future<_HostProcess> _launch() async {
    if (_current case final receiver?) return receiver;
    final Process process;
    try {
      process = await Process.start(_executable, ['host', '--managed']);
    } on ProcessException {
      throw StateError('无法启动本机接收。请检查 NexDesk 安装。');
    }
    final receiver = _HostProcess(process, _event, _exited);
    try {
      final ready = await receiver.ready.future.timeout(
        const Duration(seconds: 9),
      );
      if (_disposed || receiver.closed || ready['managed'] != true) {
        throw StateError('本机接收程序不可用。请检查 NexDesk 安装。');
      }
      _current = receiver;
      code = ready['code'] as String?;
      identity = ready['identity'] as String?;
      directoryCode = ready['directory_code'] as String?;
      directoryIdentity = ready['directory_identity'] as String?;
      return receiver;
    } on TimeoutException {
      unawaited(receiver.stop());
      throw StateError('本机接收程序启动超时。');
    } on StateError {
      unawaited(receiver.stop());
      rethrow;
    }
  }

  Future<void> requestSystemPermissions() async {
    try {
      final result = await Process.run(_executable, ['permissions']);
      issue = (result.stdout as String).contains('true')
          ? null
          : '请在系统设置中授权录屏和辅助功能。';
    } on ProcessException {
      issue = '无法打开系统授权。';
    }
    _notify();
  }

  void decide(
    int id,
    String decision, {
    int capabilities = 0,
    bool rememberController = false,
  }) {
    if (decision != 'deny' && request?.id == id) {
      _approvingDirectory = request?.directory;
      _approvalTimer?.cancel();
      _approvalTimer = Timer(const Duration(seconds: 12), _approvalFinished);
    }
    _current?.send({
      'command': 'decide',
      'request': id,
      'decision': decision,
      if (capabilities != 0) 'capabilities': capabilities,
      if (rememberController) 'remember_controller': true,
    });
  }

  void revoke() => _current?.send({'command': 'revoke'});

  Future<List<Map<String, Object?>>> trustedDevices() async {
    final devices =
        (await native.local({'operation': 'trusted_devices'})) as List;
    return devices
        .map((item) => (item as Map).cast<String, Object?>())
        .toList();
  }

  Future<bool> revokeTrustedDevice(String scope, String identity) async {
    if (_disposed) return false;
    try {
      if (_current case final receiver?) {
        final id = ++_nextRequest;
        final result = await receiver.exchange(id, {
          'command': 'revoke_controller',
          'request': id,
          'scope': scope,
          'identity': identity,
        });
        return result['ok'] == true;
      }
      await native.local({
        'operation': 'revoke_trusted_device',
        'scope': scope,
        'identity': identity,
      });
      return true;
    } on StateError {
      return false;
    } on TimeoutException {
      return false;
    }
  }

  Future<String?> changePassword({
    required bool directory,
    String? password,
  }) async {
    final receiver = _current;
    if (_disposed || receiver == null) return '本机接收程序不可用。';
    final id = ++_nextRequest;
    try {
      final result = await receiver.exchange(id, {
        'command': 'rotate',
        'request': id,
        'directory': directory,
        'password': ?password,
      });
      if (result['ok'] == true) return null;
      return result['error'] == 'invalid_code'
          ? '请输入六位英文字母或数字。'
          : '系统凭据库无法更新密码。';
    } on StateError catch (error) {
      return error.message.toString();
    } on TimeoutException {
      return '更新密码超时，请核对当前显示的密码。';
    }
  }

  void _approvalFinished() {
    _approvalTimer?.cancel();
    _approvingDirectory = null;
    _applyDeferredWhenIdle();
  }

  void _applyDeferredWhenIdle() {
    final deferred = _deferred;
    if (deferred == null ||
        applying ||
        _rechecking ||
        _disposed ||
        affectsCurrentSession(deferred)) {
      return;
    }
    _deferred = null;
    unawaited(
      saveSettings(deferred).then((error) {
        if (error != null && !_disposed) {
          issue = error;
          _notify();
        }
      }),
    );
  }

  void _setPresence(Map<String, Object?> event) {
    if (event['generation'] == _directoryGeneration) {
      directoryStatus = event['status'] as String?;
      directoryDevice = event['device'] as String? ?? directoryDevice;
    }
  }

  void _event(_HostProcess process, Map<String, Object?> event) {
    if (_disposed || !identical(process, _current)) return;
    switch (event['event']) {
      case 'request':
        request = HostRequest(
          event['request'] as int,
          event['peer'] as String,
          event['request_input'] == true && event['can_input'] == true,
          event['displays'] == true,
          event['capabilities'] as int? ?? 0,
          event['mode'] == 'id',
          event['controller'] as String?,
        );
      case 'request_closed':
        if (request?.id == event['request']) request = null;
        _applyDeferredWhenIdle();
      case 'active':
        _activeRequest = event['request'] as int;
        _activeDirectory = event['mode'] == 'id';
        peer = event['peer'] as String;
        grant = event['grant'] as String;
        activeCapabilities = event['capabilities'] as int? ?? 0;
        unattendedSession = event['unattended'] == true;
        _approvalFinished();
      case 'ended':
        if (event['request'] == _activeRequest) {
          peer = grant = null;
          activeCapabilities = 0;
          unattendedSession = false;
          _activeRequest = null;
          _activeDirectory = null;
          _applyDeferredWhenIdle();
        }
      case 'code':
        if (event['directory'] == true) {
          directoryCode = event['code'] as String?;
        } else {
          code = event['code'] as String?;
        }
      case 'directory':
        _setPresence(event);
      case 'lan_issue':
        if (endpoint == event['endpoint']) {
          endpoint = null;
          lanIssue = _receiverIssue(event['error']);
        }
      case 'issue':
        issue = event['code'] == 'trust_store'
            ? '本次会话已授权，设备授权记录无法保存。'
            : '系统凭据库无法更新密码。';
    }
    _notify();
  }

  void _exited(_HostProcess process) {
    if (!identical(process, _current)) return;
    _current = null;
    request = null;
    code = identity = endpoint = null;
    directoryCode = directoryIdentity = directoryDevice = directoryStatus =
        null;
    _directoryGeneration = null;
    activeCapabilities = 0;
    unattendedSession = false;
    peer = grant = null;
    _activeRequest = null;
    _activeDirectory = null;
    issue = '本机接收程序已退出。保存设置可重新应用配置。';
    _approvalFinished();
    _notify();
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  static String _settingsError(String error) => switch (error) {
    'port_in_use' => '该 IP 和端口已被占用，请选择其他端口。',
    'address' || 'listen' => '无法监听所选地址，请检查网卡和端口。',
    'source' => '无法使用所选显示器，请刷新并选择可共享的来源。',
    'share_root' => '无法打开被控端共享目录，请检查路径和权限。',
    'download_root' => '无法打开本机接收文件的目录。',
    'directory' => '请输入有效的目录 HTTPS 地址。',
    'storage' => '本机设置无法保存。',
    _ => '无法应用设置，请检查输入。',
  };

  static String? _receiverIssue(Object? code) =>
      code is String ? _settingsError(code) : null;

  @override
  void dispose() {
    _disposed = true;
    _networkTimer?.cancel();
    _approvalTimer?.cancel();
    if (_current case final receiver?) unawaited(receiver.stop());
    super.dispose();
  }
}

final class _HostProcess {
  _HostProcess(
    this.process,
    void Function(_HostProcess, Map<String, Object?>) onEvent,
    void Function(_HostProcess) onExit,
  ) {
    unawaited(process.stderr.drain<void>().catchError((Object _) {}));
    process.stdout
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen(
          (line) {
            if (!line.startsWith('{')) return;
            try {
              final event = (jsonDecode(line) as Map).cast<String, Object?>();
              switch (event['event']) {
                case 'ready':
                  if (!ready.isCompleted) ready.complete(event);
                case 'directory':
                  final generation = event['generation'] as int? ?? 0;
                  final cached = presence?['generation'] as int? ?? 0;
                  if (generation >= cached) presence = event;
                case 'settings_result':
                  final result = _settings.remove(event['request']);
                  result?.complete(event);
              }
              onEvent(this, event);
            } on FormatException {
              // The last line can be interrupted when closing the application.
            }
          },
          onDone: () {
            closed = true;
            final error = StateError('本机接收程序不可用，请检查系统凭据和 NexDesk 安装。');
            if (!ready.isCompleted) ready.completeError(error);
            for (final request in _settings.values) {
              request.completeError(error);
            }
            _settings.clear();
            onExit(this);
          },
          onError: (Object _) {},
        );
    unawaited(process.stdin.done.then<void>((_) {}).catchError((Object _) {}));
  }

  final Process process;
  final ready = Completer<Map<String, Object?>>();
  final _settings = <int, Completer<Map<String, Object?>>>{};
  Map<String, Object?>? presence;
  bool closed = false;
  bool _stopping = false;

  Future<Map<String, Object?>> apply(int id, Map<String, Object?> settings) =>
      exchange(id, {
        'command': 'apply_settings',
        'config': {'request': id, ...settings},
      });

  Future<Map<String, Object?>> exchange(
    int id,
    Map<String, Object?> command,
  ) async {
    if (closed || _stopping) throw StateError('本机接收程序不可用。');
    final response = Completer<Map<String, Object?>>();
    _settings[id] = response;
    try {
      send(command);
      return await response.future.timeout(const Duration(seconds: 12));
    } finally {
      _settings.remove(id);
    }
  }

  void send(Map<String, Object?> command) {
    if (!_stopping && !closed) process.stdin.writeln(jsonEncode(command));
  }

  Future<void> stop() async {
    if (_stopping || closed) return;
    send({'command': 'stop'});
    _stopping = true;
    try {
      unawaited(
        process.stdin.close().then<void>((_) {}).catchError((Object _) {}),
      );
      await process.exitCode.timeout(const Duration(seconds: 3));
    } on TimeoutException {
      process.kill();
    }
  }
}
