import 'dart:async';
import 'dart:io' show InternetAddress;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../connection/desktop_settings.dart';
import '../connection/desktop_version.dart';
import '../connection/directory_origin.dart';
import '../connection/settings_dialog.dart';
import '../connection/target_validator.dart';
import '../lan/connect_dialog.dart';
import '../lan/connection_approval_dialog.dart';
import '../lan/connection_password.dart';
import '../lan/host_controller.dart';
import '../lan/native_desktop.dart';
import '../lan/session_window.dart';
import 'home_panel.dart';
import 'local_device.dart';
import 'recent_connections.dart';

enum _ConnectionMode { deviceId, lan }

typedef _SessionTarget = (String? origin, String address, int port);

class HomeScreen extends StatefulWidget {
  const HomeScreen({
    super.key,
    required this.validator,
    this.desktop,
    this.defaultDirectory = defaultDirectoryOrigin,
  });

  final TargetValidator? validator;
  final NativeDesktop? desktop;
  final String defaultDirectory;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  final _deviceId = TextEditingController();
  final _lanTarget = TextEditingController();
  final _deviceFocus = FocusNode(debugLabel: 'remote device ID');
  final _lanFocus = FocusNode(debugLabel: 'remote IP address');
  late final _version = loadDesktopVersion();
  _ConnectionMode _mode = _ConnectionMode.deviceId;
  HostController? _host;
  DialogRoute<void>? _consentDialog;
  int? _consentId;
  List<RecentDevice> _lanRecent = const [];
  List<RecentDevice> _idRecent = const [];
  final _openSessions = <_SessionTarget, int>{};
  final _presence = <_SessionTarget, String>{};
  int _recentRequest = 0;
  String? _targetIssue;
  bool _prompting = false;
  bool _otherDirectories = false;
  String? _recentGroup;

  bool get _isLan => _mode == _ConnectionMode.lan;
  int get _defaultPort => widget.validator?.defaultLanPort ?? 53980;
  TextEditingController get _targetInput => _isLan ? _lanTarget : _deviceId;
  FocusNode get _targetFocus => _isLan ? _lanFocus : _deviceFocus;

  @override
  void initState() {
    super.initState();
    if (widget.desktop case final desktop?) {
      SessionWindow.onClosed(_sessionClosed);
      final host = HostController(
        desktop,
        defaultDirectory: widget.defaultDirectory,
      );
      _host = host;
      host.addListener(_hostChanged);
      unawaited(host.initialize());
      unawaited(_loadRecent());
    }
  }

  @override
  void dispose() {
    if (widget.desktop != null) SessionWindow.onClosed(null);
    _host?.removeListener(_hostChanged);
    _host?.dispose();
    _deviceId.dispose();
    _lanTarget.dispose();
    _deviceFocus.dispose();
    _lanFocus.dispose();
    super.dispose();
  }

  void _sessionClosed(int viewer) {
    if (!mounted) return;
    setState(() => _openSessions.removeWhere((_, value) => value == viewer));
    unawaited(_loadRecent());
  }

  void _hostChanged() {
    if (!mounted) return;
    setState(() {});
    final host = _host;
    final request = host?.request;
    final current = _consentDialog;
    final navigator = Navigator.of(context);
    if (_consentId != request?.id && current != null) {
      _consentDialog = null;
      _consentId = null;
      if (current.isActive) navigator.removeRoute(current);
    }
    if (request == null || host == null || _consentDialog != null) return;
    var decided = false;
    final dialog = DialogRoute<void>(
      context: context,
      barrierDismissible: false,
      builder: (_) => ConnectionApprovalDialog(
        request: request,
        onDecision: (decision, capabilities, remember) {
          decided = true;
          host.decide(
            request.id,
            decision,
            capabilities: capabilities,
            rememberController: remember,
          );
          final route = _consentDialog;
          if (route != null && route.isActive) navigator.removeRoute(route);
        },
      ),
    );
    _consentDialog = dialog;
    _consentId = request.id;
    unawaited(
      navigator.push(dialog).then((_) {
        if (identical(_consentDialog, dialog)) {
          _consentDialog = null;
          _consentId = null;
        }
        if (!decided && host.request?.id == request.id) {
          host.decide(request.id, 'deny');
        }
      }),
    );
  }

  Future<void> _loadRecent() async {
    final desktop = widget.desktop;
    if (desktop == null) return;
    final request = ++_recentRequest;
    try {
      final records = await Future.wait([
        desktop.local({'operation': 'recent'}),
        if (desktop.supportsDirectory)
          desktop.local({'operation': 'directory_recent'}),
      ]);
      if (mounted && request == _recentRequest) {
        setState(() {
          _lanRecent = (records.first as List)
              .whereType<Map>()
              .map(RecentDevice.fromJson)
              .toList();
          if (records.length == 2) {
            _idRecent = (records[1] as List)
                .whereType<Map>()
                .map(RecentDevice.fromDirectory)
                .toList();
          }
        });
      }
    } on StateError {
      if (mounted && request == _recentRequest) {
        setState(() {
          _lanRecent = const [];
          _idRecent = const [];
        });
      }
    }
  }

  Future<bool> _openSettings({String? directory}) async {
    final host = _host;
    if (host == null || host.loading) return false;
    if (host.sources.isEmpty) {
      await host.refreshDevices();
      if (!mounted) return false;
    }
    final saved = await showDialog<bool>(
      context: context,
      barrierDismissible: false,
      builder: (_) => SettingsDialog(host: host, directory: directory),
    );
    return saved == true;
  }

  Future<void> _connect([RecentDevice? recent]) async {
    if (_prompting || (recent == null && _targetInput.text.trim().isEmpty)) {
      return;
    }
    setState(() => _prompting = true);
    try {
      await _connectTarget(recent);
    } finally {
      if (mounted) setState(() => _prompting = false);
    }
  }

  Future<void> _connectTarget(RecentDevice? recent) async {
    final byIp = _isLan;
    final host = _host;
    final validator = widget.validator;
    final desktop = widget.desktop;
    final oldDirectory = recent?.origin;
    if (recent != null && await _focusSession(_recentTarget(recent))) return;
    if (!mounted) return;
    if (!byIp &&
        oldDirectory != null &&
        oldDirectory != host?.directoryOrigin) {
      final change = await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: const Text('切换连接服务？'),
          content: Text('此设备登记在 $oldDirectory。可在设置中切换至该服务。'),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: const Text('打开设置'),
            ),
          ],
        ),
      );
      if (!mounted ||
          change != true ||
          !await _openSettings(directory: oldDirectory)) {
        return;
      }
      if (!mounted || oldDirectory != host?.directoryOrigin) return;
    }
    if (!mounted || _isLan != byIp || validator == null) return;

    if (recent != null) {
      (byIp ? _lanTarget : _deviceId).text = byIp
          ? recent.endpoint
          : recent.address;
    }
    final lan = LanTarget.fromInput(_lanTarget.text, _defaultPort);
    final device = _deviceId.text.replaceAll(RegExp(r'\s'), '');
    final issue = byIp
        ? validator.checkLan(lan.address, lan.port)
        : validator.checkDeviceId(device);
    final origin = byIp ? null : host?.directoryOrigin;
    final error = switch (issue) {
      TargetIssue.address => '请输入有效的 IPv4 或 IPv6 地址。',
      TargetIssue.port => '端口范围为 1–65535。',
      TargetIssue.deviceId => '请输入九位设备 ID。',
      TargetIssue.text => '连接地址过长。',
      TargetIssue.nativeFailure => '无法校验连接地址。',
      null when desktop == null => '此构建未加载远控核心。',
      null when !byIp && origin == null => '请先在设置中配置 ID 服务。',
      null => null,
    };
    setState(() => _targetIssue = error);
    if (issue != null) _targetFocus.requestFocus();
    if (error != null || desktop == null) return;

    final address = byIp ? lan.address : device;
    final port = byIp ? int.parse(lan.port) : 0;
    final target = _sessionTarget(origin, address, port);
    if (recent == null && await _focusSession(target)) return;
    if (!mounted) return;
    final label = byIp ? lan.endpoint : device;
    final saved =
        recent?.remembered ?? _remembered(byIp, origin, address, port);
    final settings = host?.settings ?? const DesktopSettings();
    final offersCollaboration = desktop.collaboration != null;
    final password = saved
        ? ConnectionPassword.request(
            permissions: settings.permissions,
            offersCollaboration: offersCollaboration,
            stored: true,
          )
        : await askConnectionPassword(
            context,
            target: label,
            saved: false,
            offersCollaboration: offersCollaboration,
            permissions: settings.permissions,
          );
    if (!mounted || password == null) return;
    if (!byIp && origin != host?.directoryOrigin) {
      setState(() => _targetIssue = 'ID 服务已切换，请重新选择设备。');
      return;
    }
    final viewer = await showDialog<int>(
      context: context,
      barrierDismissible: false,
      builder: (_) => ConnectDialog(
        native: desktop,
        address: address,
        port: port,
        origin: origin,
        label: recent?.label ?? label,
        password: password,
        settings: settings,
      ),
    );
    if (!mounted || viewer == null) return;
    setState(() => _openSessions[target] = viewer);
    await _loadRecent();
  }

  Future<bool> _focusSession(_SessionTarget target) async {
    final viewer = _openSessions[target];
    if (viewer == null) return false;
    if (await SessionWindow.focus(viewer)) return true;
    if (mounted && _openSessions[target] == viewer) {
      setState(() => _openSessions.remove(target));
    }
    return false;
  }

  _SessionTarget _recentTarget(RecentDevice device) =>
      _sessionTarget(device.origin, device.address, device.port);

  _SessionTarget _sessionTarget(String? origin, String address, int port) {
    if (origin != null) return (origin, address, 0);
    final ip = InternetAddress.tryParse(address.trim());
    if (ip == null) return (null, address, port);
    var bytes = ip.rawAddress;
    if (bytes.length == 16 &&
        bytes.take(10).every((byte) => byte == 0) &&
        bytes[10] == 255 &&
        bytes[11] == 255) {
      bytes = bytes.sublist(12);
    }
    return (null, InternetAddress.fromRawAddress(bytes).address, port);
  }

  bool _remembered(bool lan, String? origin, String address, int port) {
    final target = _sessionTarget(origin, address, port);
    return (lan ? _lanRecent : _idRecent).any(
      (item) => item.remembered && _recentTarget(item) == target,
    );
  }

  Future<void> _editRecent(RecentDevice device, RecentAction action) async {
    final desktop = widget.desktop;
    if (desktop == null) return;
    final origin = device.origin;
    final target = origin == null
        ? {'endpoint': device.endpoint}
        : {'origin': origin, 'device': device.address};
    final prefix = origin == null ? '' : 'directory_';
    final key = _recentTarget(device);
    if (action == RecentAction.presence && origin != null) {
      await _checkPresence(desktop, device, origin);
      return;
    }
    try {
      switch (action) {
        case RecentAction.rename:
          final label = await _editLabel('编辑别名', device.label, 64);
          if (label == null || label.trim().isEmpty) return;
          await desktop.local({
            'operation': '${prefix}rename',
            ...target,
            'label': label,
          });
        case RecentAction.group:
          final group = await _editLabel(
            '修改分组',
            device.group,
            40,
            hint: '留空可取消分组',
          );
          if (group == null) return;
          await desktop.local({
            'operation': 'recent_group',
            'origin': origin,
            'target': origin == null ? device.endpoint : device.address,
            'name': group,
          });
        case RecentAction.disconnect:
          final viewer = _openSessions[key];
          if (viewer != null && !await SessionWindow.requestClose(viewer)) {
            _sessionClosed(viewer);
          }
          return;
        case RecentAction.forget || RecentAction.remove:
          final remove = action == RecentAction.remove;
          if (remove && !await _confirmRemoval(device.label)) return;
          await desktop.local({
            'operation': '${prefix}forget',
            ...target,
            'remove_recent': remove,
          });
          if (remove) _presence.remove(key);
        case RecentAction.presence:
          return;
      }
    } on StateError {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('无法更新系统凭据或近期记录。')));
      }
    }
    if (mounted) await _loadRecent();
  }

  Future<String?> _editLabel(
    String title,
    String value,
    int limit, {
    String? hint,
  }) => showDialog<String>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(title),
      content: TextFormField(
        initialValue: value,
        autofocus: true,
        maxLength: limit,
        decoration: InputDecoration(hintText: hint),
        onChanged: (text) => value = text,
        onFieldSubmitted: (text) => Navigator.pop(context, text),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(context, value),
          child: const Text('保存'),
        ),
      ],
    ),
  );

  Future<bool> _confirmRemoval(String label) async =>
      await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: const Text('移除近期记录？'),
          content: Text('将移除 $label 的记录、已保存密码和设备身份。'),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context, false),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: const Text('移除'),
            ),
          ],
        ),
      ) ==
      true;

  Future<void> _checkPresence(
    NativeDesktop desktop,
    RecentDevice device,
    String origin,
  ) async {
    final key = _recentTarget(device);
    if (_presence[key] == '正在检查…') return;
    setState(() => _presence[key] = '正在检查…');
    var result = '查询失败';
    try {
      final reply = await desktop.local({
        'operation': 'directory_presence',
        'origin': origin,
        'device': device.address,
      });
      result = switch ((reply as Map)['online']) {
        true => '上次查询在线',
        false => '上次查询离线',
        _ => '查询失败',
      };
    } on StateError {
      // Keep the card available for the next connection attempt.
    }
    if (mounted && _presence.containsKey(key)) {
      setState(() => _presence[key] = result);
    }
  }

  void _chooseMode(Set<_ConnectionMode> selection) {
    setState(() {
      _mode = selection.single;
      _targetIssue = null;
      _otherDirectories = false;
      _recentGroup = null;
    });
  }

  @override
  Widget build(BuildContext context) {
    final host = _host;
    return Scaffold(
      body: SafeArea(
        child: Column(
          children: [
            Expanded(
              child: SingleChildScrollView(
                padding: const EdgeInsets.fromLTRB(16, 10, 16, 16),
                child: Center(
                  child: ConstrainedBox(
                    constraints: const BoxConstraints(maxWidth: 784),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        _toolbar(),
                        const SizedBox(height: 11),
                        LayoutBuilder(
                          builder: (context, constraints) {
                            final local = LocalDevice(
                              key: ValueKey(_mode),
                              host: host,
                              lan: _isLan,
                            );
                            final remote = _remote();
                            if (constraints.maxWidth < 560) {
                              return Column(
                                crossAxisAlignment: CrossAxisAlignment.stretch,
                                children: [
                                  local,
                                  const SizedBox(height: 12),
                                  remote,
                                ],
                              );
                            }
                            return IntrinsicHeight(
                              child: Row(
                                crossAxisAlignment: CrossAxisAlignment.stretch,
                                children: [
                                  Expanded(child: local),
                                  const SizedBox(width: 12),
                                  Expanded(child: remote),
                                ],
                              ),
                            );
                          },
                        ),
                        const SizedBox(height: 14),
                        _recent(),
                      ],
                    ),
                  ),
                ),
              ),
            ),
            _status(),
          ],
        ),
      ),
    );
  }

  Widget _toolbar() => LayoutBuilder(
    builder: (context, bounds) {
      final compact = bounds.maxWidth < 326;
      final openSettings = _host?.loading == false ? _openSettings : null;
      return Row(
        mainAxisAlignment: MainAxisAlignment.spaceBetween,
        children: [
          Flexible(
            child: SegmentedButton<_ConnectionMode>(
              style: _modeStyle(),
              segments: const [
                ButtonSegment(
                  value: _ConnectionMode.deviceId,
                  label: Text('ID 连接'),
                ),
                ButtonSegment(value: _ConnectionMode.lan, label: Text('IP 直连')),
              ],
              selected: {_mode},
              onSelectionChanged: _prompting ? null : _chooseMode,
              showSelectedIcon: false,
            ),
          ),
          const SizedBox(width: 8),
          Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (_host?.peer != null) ...[
                if (compact)
                  IconButton(
                    tooltip: '共享中',
                    onPressed: _showIncoming,
                    icon: const Icon(Icons.screen_share_outlined, size: 17),
                  )
                else
                  TextButton.icon(
                    onPressed: _showIncoming,
                    icon: const Icon(Icons.screen_share_outlined, size: 17),
                    label: const Text('共享中'),
                  ),
                const SizedBox(width: 5),
              ],
              if (compact)
                IconButton.outlined(
                  tooltip: '设置',
                  onPressed: openSettings,
                  icon: const Icon(Icons.settings_outlined, size: 17),
                  style: IconButton.styleFrom(
                    fixedSize: const Size(34, 34),
                    shape: RoundedRectangleBorder(
                      borderRadius: BorderRadius.circular(7),
                    ),
                    tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                    padding: EdgeInsets.zero,
                  ),
                )
              else
                OutlinedButton.icon(
                  onPressed: openSettings,
                  icon: const Icon(Icons.settings_outlined, size: 17),
                  label: const Text('设置'),
                  style: OutlinedButton.styleFrom(
                    minimumSize: const Size(76, 34),
                    padding: const EdgeInsets.symmetric(horizontal: 10),
                    tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                  ),
                ),
            ],
          ),
        ],
      );
    },
  );

  Future<void> _showIncoming() async {
    final host = _host;
    if (host == null) return;
    await showDialog<void>(
      context: context,
      builder: (context) => ListenableBuilder(
        listenable: host,
        builder: (context, _) {
          final peer = host.peer;
          return AlertDialog(
            title: const Text('正在访问本机的连接'),
            content: Text(
              peer == null
                  ? '会话已结束。'
                  : '$peer\n${host.grant == 'control' ? '正在控制' : '正在观看'}'
                        '${host.activeCapabilities & 64 != 0 ? '\n远程 Shell 已授权' : ''}',
            ),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(context),
                child: const Text('关闭'),
              ),
              if (peer != null)
                FilledButton(
                  onPressed: () {
                    host.revoke();
                    Navigator.pop(context);
                  },
                  child: const Text('断开此连接'),
                ),
            ],
          );
        },
      ),
    );
  }

  ButtonStyle _modeStyle() {
    final color = Theme.of(context).colorScheme;
    return ButtonStyle(
      minimumSize: const WidgetStatePropertyAll(Size(76, 34)),
      padding: const WidgetStatePropertyAll(
        EdgeInsets.symmetric(horizontal: 11),
      ),
      visualDensity: VisualDensity.compact,
      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
      shape: WidgetStatePropertyAll(
        RoundedRectangleBorder(borderRadius: BorderRadius.circular(7)),
      ),
      side: WidgetStatePropertyAll(BorderSide(color: color.outlineVariant)),
      backgroundColor: WidgetStateProperty.resolveWith(
        (states) => states.contains(WidgetState.selected)
            ? color.primaryContainer
            : color.surface,
      ),
      foregroundColor: WidgetStateProperty.resolveWith(
        (states) => states.contains(WidgetState.selected)
            ? color.onPrimaryContainer
            : color.onSurfaceVariant,
      ),
      overlayColor: WidgetStatePropertyAll(
        color.primary.withValues(alpha: .09),
      ),
    );
  }

  void _clearTargetIssue() {
    if (_targetIssue != null && mounted) setState(() => _targetIssue = null);
  }

  Future<void> _pasteTarget(TextEditingController controller) async {
    final previous = controller.text;
    try {
      final data = await Clipboard.getData(Clipboard.kTextPlain);
      if (!mounted ||
          _targetInput != controller ||
          _prompting ||
          controller.text != previous) {
        return;
      }
      final value = data?.text?.trim() ?? '';
      if (value.isEmpty) {
        _showInputNotice('剪贴板中没有文本。');
        return;
      }
      controller.value = TextEditingValue(
        text: value,
        selection: TextSelection.collapsed(offset: value.length),
      );
      _clearTargetIssue();
      _targetFocus.requestFocus();
    } on PlatformException {
      if (mounted) _showInputNotice('无法读取剪贴板。');
    }
  }

  void _showInputNotice(String message) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(message)));
  }

  Widget _remote() {
    final engineAvailable = widget.validator != null;
    final controller = _targetInput;
    final enabled = engineAvailable && !_prompting;
    final text = Theme.of(context).textTheme;
    return ValueListenableBuilder<TextEditingValue>(
      valueListenable: controller,
      builder: (context, value, _) {
        final hasInput = value.text.trim().isNotEmpty;
        final canConnect = enabled && hasInput;
        return HomePanel(
          key: const ValueKey('remote-device-panel'),
          title: '连接远程设备',
          subtitle: _isLan ? '使用局域网地址连接。' : '使用对方的设备 ID 连接。',
          action: FilledButton.icon(
            onPressed: canConnect ? () => _connect() : null,
            icon: const Icon(Icons.arrow_forward_rounded, size: 17),
            label: Text(_prompting ? '连接中…' : '连接'),
          ),
          children: [
            Text(_isLan ? '远程 IP / 端口' : '远程设备 ID', style: text.labelMedium),
            const SizedBox(height: 5),
            TextField(
              key: ValueKey(_isLan ? 'lan-target' : 'device-id'),
              controller: controller,
              focusNode: _targetFocus,
              readOnly: _prompting,
              keyboardType: _isLan ? TextInputType.text : TextInputType.number,
              textInputAction: TextInputAction.go,
              autocorrect: false,
              enableSuggestions: false,
              onChanged: (_) => _clearTargetIssue(),
              onSubmitted: enabled ? (_) => _connect() : null,
              decoration: InputDecoration(
                hintText: _isLan ? '192.168.1.20:$_defaultPort' : '输入九位设备 ID',
                errorText: _targetIssue,
                suffixIconConstraints: const BoxConstraints(
                  minWidth: 36,
                  minHeight: 36,
                ),
                suffixIcon: IconButton(
                  tooltip: hasInput ? '清空输入' : '粘贴',
                  iconSize: 17,
                  style: IconButton.styleFrom(
                    fixedSize: const Size(36, 36),
                    tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                  ),
                  onPressed: !enabled
                      ? null
                      : hasInput
                      ? () {
                          controller.clear();
                          _clearTargetIssue();
                          _targetFocus.requestFocus();
                        }
                      : () => _pasteTarget(controller),
                  icon: Icon(
                    hasInput
                        ? Icons.close_rounded
                        : Icons.content_paste_outlined,
                  ),
                ),
              ),
            ),
            if (_isLan && _targetIssue == null) ...[
              const SizedBox(height: 6),
              Text('省略端口时使用 $_defaultPort', style: text.bodySmall),
            ],
            if (!engineAvailable) ...[
              const SizedBox(height: 6),
              Text('未加载 Rust 核心，当前无法建立连接。', style: text.bodySmall),
            ],
          ],
        );
      },
    );
  }

  Widget _recent() {
    final origin = _host?.directoryOrigin;
    final current = _isLan
        ? _lanRecent
        : _idRecent.where((device) => device.origin == origin).toList();
    final other = _isLan
        ? const <RecentDevice>[]
        : _idRecent.where((device) => device.origin != origin).toList();
    final groups = {
      for (final device in [...current, if (_otherDirectories) ...other])
        if (device.group.isNotEmpty) device.group,
    }.toList()..sort();
    final group = groups.contains(_recentGroup) ? _recentGroup : null;
    final visible = current
        .where((device) => group == null || device.group == group)
        .toList();
    final visibleOther = other
        .where((device) => group == null || device.group == group)
        .toList();
    final theme = Theme.of(context);
    return HomePanel(
      title: '近期连接',
      headerAction: groups.isEmpty
          ? null
          : SizedBox(
              width: 134,
              child: DropdownButton<String>(
                isExpanded: true,
                isDense: true,
                value: group ?? '',
                onChanged: (value) => setState(() => _recentGroup = value),
                items: [
                  const DropdownMenuItem(value: '', child: Text('全部分组')),
                  for (final name in groups)
                    DropdownMenuItem(
                      value: name,
                      child: Text(
                        name,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                ],
              ),
            ),
      children: [
        if (visible.isEmpty && (!_otherDirectories || visibleOther.isEmpty))
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 9),
            child: Row(
              children: [
                Container(
                  width: 38,
                  height: 38,
                  decoration: BoxDecoration(
                    color: theme.colorScheme.surfaceContainer,
                    borderRadius: BorderRadius.circular(7),
                  ),
                  child: Icon(
                    Icons.history_rounded,
                    size: 19,
                    color: theme.colorScheme.onSurfaceVariant,
                  ),
                ),
                const SizedBox(width: 10),
                Expanded(
                  child: Text(
                    _isLan ? '暂无局域网连接记录' : '暂无设备 ID 连接记录',
                    style: theme.textTheme.bodySmall,
                  ),
                ),
              ],
            ),
          ),
        if (visible.isNotEmpty) _recentGrid(visible),
        if (other.isNotEmpty)
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton(
              onPressed: () => setState(() {
                _otherDirectories = !_otherDirectories;
                _recentGroup = null;
              }),
              child: Text(_otherDirectories ? '收起其他服务记录' : '查看其他服务记录'),
            ),
          ),
        if (_otherDirectories && visibleOther.isNotEmpty)
          _recentGrid(visibleOther, showOrigin: true),
      ],
    );
  }

  Widget _recentGrid(List<RecentDevice> devices, {bool showOrigin = false}) =>
      RecentDeviceGrid(
        devices: devices,
        enabled: !_prompting,
        isOpen: (device) => _openSessions.containsKey(_recentTarget(device)),
        presence: (device) => _presence[_recentTarget(device)],
        onConnect: _connect,
        onAction: _editRecent,
        showOrigin: showOrigin,
      );

  Widget _status() {
    final host = _host;
    final ready = _isLan
        ? host?.endpoint != null
        : host?.directoryStatus == 'online';
    final text = switch (host) {
      null => '远控核心不可用',
      _ when host.loading => '正在准备本机连接…',
      _ when _isLan && !host.settings.lanAllowed => '本机禁止局域网来访',
      _ when _isLan && host.endpoint != null =>
        host.endpoint!.startsWith('127.') || host.endpoint!.startsWith('[::1]')
            ? 'IP 直连 · 仅本机'
            : 'IP 直连可用',
      _ when _isLan => '局域网地址暂不可用',
      _ when host.directoryOrigin == null => '请在设置中配置 ID 服务',
      _ when host.directoryStatus == 'online' =>
        host.settings.useSelfHosted ? '已连接自托管服务' : '已连接默认服务',
      _ when host.directoryStatus == 'revoked' => '本机 ID 已被目录撤销',
      _ when host.directoryStatus == 'offline' => 'ID 服务暂不可用，正在重试',
      _ when host.issue != null => '本机 ID 暂不可用',
      _ => '正在向 ID 服务注册…',
    };
    final theme = Theme.of(context);
    final colors = theme.colorScheme;
    return Material(
      color: colors.surface,
      child: Container(
        decoration: BoxDecoration(
          border: Border(top: BorderSide(color: colors.outlineVariant)),
        ),
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
        child: Row(
          children: [
            Icon(
              Icons.circle,
              size: 8,
              color: ready
                  ? (colors.brightness == Brightness.dark
                        ? const Color(0xff68d4a8)
                        : const Color(0xff167756))
                  : colors.outline,
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Semantics(
                liveRegion: true,
                child: Wrap(
                  spacing: 10,
                  children: [
                    Text(text, style: theme.textTheme.bodySmall),
                    if (host?.hasDeferredSettings == true)
                      Text('会话结束后应用设置', style: theme.textTheme.bodySmall),
                  ],
                ),
              ),
            ),
            FutureBuilder<String?>(
              future: _version,
              builder: (context, snapshot) {
                final version = snapshot.data;
                if (version == null) return const SizedBox.shrink();
                return Padding(
                  padding: const EdgeInsets.only(left: 12),
                  child: Text(
                    'v$version',
                    style: theme.textTheme.bodySmall?.copyWith(
                      fontFeatures: const [FontFeature.tabularFigures()],
                    ),
                  ),
                );
              },
            ),
          ],
        ),
      ),
    );
  }
}
