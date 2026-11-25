import 'dart:async';
import 'dart:io' show InternetAddress;

import 'package:flutter/foundation.dart' show listEquals;
import 'package:flutter/material.dart';

import '../connection/desktop_settings.dart';
import '../connection/directory_origin.dart';
import '../connection/settings_dialog.dart';
import '../connection/target_validator.dart';
import '../lan/connection_approval_dialog.dart';
import '../lan/connection_password.dart';
import '../lan/host_controller.dart';
import '../lan/native_desktop.dart';
import '../lan/session_screen.dart';
import '../ui/nexdesk_theme.dart';
import 'home_panel.dart';
import 'local_device.dart';

enum _ConnectionMode { deviceId, lan }

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
  _ConnectionMode _mode = _ConnectionMode.deviceId;
  HostController? _host;
  DialogRoute<void>? _consentDialog;
  int? _consentId;
  List<RecentDevice> _lanRecent = const [];
  List<RecentDevice> _idRecent = const [];
  String? _targetIssue;
  bool _prompting = false;
  bool _otherDirectories = false;

  bool get _isLan => _mode == _ConnectionMode.lan;
  int get _defaultPort => widget.validator?.defaultLanPort ?? 53980;

  @override
  void initState() {
    super.initState();
    if (widget.desktop case final desktop?) {
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
    _host?.removeListener(_hostChanged);
    _host?.dispose();
    _deviceId.dispose();
    _lanTarget.dispose();
    super.dispose();
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
    try {
      final records = await Future.wait([
        desktop.local({'operation': 'recent'}),
        if (desktop.supportsDirectory)
          desktop.local({'operation': 'directory_recent'}),
      ]);
      if (mounted) {
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
      if (mounted) {
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
    if (_prompting) return;
    final byIp = _isLan;
    final host = _host;
    final validator = widget.validator;
    final desktop = widget.desktop;
    final oldDirectory = recent?.origin;
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
    if (error != null || desktop == null) return;

    final address = byIp ? lan.address : device;
    final port = byIp ? int.parse(lan.port) : 0;
    final label = byIp ? lan.endpoint : device;
    final saved =
        recent?.remembered ?? _remembered(byIp, origin, address, port);
    final settings = host?.settings ?? const DesktopSettings();
    setState(() => _prompting = true);
    try {
      final password = await askConnectionPassword(
        context,
        target: label,
        saved: saved,
        offersCollaboration: desktop.collaboration != null,
        permissions: settings.permissions,
      );
      if (!mounted || password == null) return;
      if (!byIp && origin != host?.directoryOrigin) {
        setState(() => _targetIssue = 'ID 服务已切换，请重新选择设备。');
        return;
      }
      final viewer = desktop.connectTarget(address, port, origin, password);
      if (viewer == 0) {
        setState(() => _targetIssue = '无法发起连接，请核对连接信息。');
        return;
      }
      await Navigator.of(context).push(
        MaterialPageRoute<void>(
          builder: (_) => LanSessionScreen(
            native: desktop,
            viewer: viewer,
            address: address,
            port: port,
            directoryOrigin: origin,
            saved: saved,
            host: host,
            settings: settings,
          ),
        ),
      );
      if (mounted) await _loadRecent();
    } finally {
      if (mounted) setState(() => _prompting = false);
    }
  }

  bool _remembered(bool lan, String? origin, String address, int port) {
    if (!lan) {
      return _idRecent.any(
        (item) =>
            item.origin == origin && item.address == address && item.remembered,
      );
    }
    final ip = InternetAddress.tryParse(address);
    return ip != null &&
        _lanRecent.any(
          (item) =>
              item.port == port &&
              item.remembered &&
              listEquals(
                InternetAddress.tryParse(item.address)?.rawAddress,
                ip.rawAddress,
              ),
        );
  }

  Future<void> _editRecent(RecentDevice device, String operation) async {
    final desktop = widget.desktop;
    if (desktop == null) return;
    final origin = device.origin;
    final target = origin == null
        ? {'endpoint': device.endpoint}
        : {'origin': origin, 'device': device.address};
    final prefix = origin == null ? '' : 'directory_';
    try {
      if (operation == 'rename') {
        final controller = TextEditingController(text: device.label);
        final label = await showDialog<String>(
          context: context,
          builder: (context) => AlertDialog(
            title: const Text('重命名设备'),
            content: TextField(
              controller: controller,
              maxLength: 64,
              autofocus: true,
            ),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(context),
                child: const Text('取消'),
              ),
              FilledButton(
                onPressed: () => Navigator.pop(context, controller.text),
                child: const Text('保存'),
              ),
            ],
          ),
        );
        WidgetsBinding.instance.addPostFrameCallback(
          (_) => controller.dispose(),
        );
        if (label == null || label.trim().isEmpty) return;
        await desktop.local({
          'operation': '${prefix}rename',
          ...target,
          'label': label,
        });
      } else {
        await desktop.local({
          'operation': '${prefix}forget',
          ...target,
          'remove_recent': operation == 'remove',
        });
      }
    } on StateError {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('无法更新系统凭据或近期记录。')));
      }
    }
    if (mounted) await _loadRecent();
  }

  void _chooseMode(Set<_ConnectionMode> selection) {
    setState(() {
      _mode = selection.single;
      _targetIssue = null;
      _otherDirectories = false;
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
                padding: const EdgeInsets.fromLTRB(16, 15, 16, 18),
                child: Center(
                  child: ConstrainedBox(
                    constraints: const BoxConstraints(maxWidth: 784),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        _header(),
                        const SizedBox(height: 14),
                        Align(
                          alignment: Alignment.centerLeft,
                          child: SegmentedButton<_ConnectionMode>(
                            style: _modeStyle(),
                            segments: const [
                              ButtonSegment(
                                value: _ConnectionMode.deviceId,
                                label: Text('ID 连接'),
                              ),
                              ButtonSegment(
                                value: _ConnectionMode.lan,
                                label: Text('IP 直连'),
                              ),
                            ],
                            selected: {_mode},
                            onSelectionChanged: _chooseMode,
                            showSelectedIcon: false,
                          ),
                        ),
                        const SizedBox(height: 14),
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

  Widget _header() {
    final theme = Theme.of(context);
    return Row(
      children: [
        const NexDeskMark(),
        const SizedBox(width: 9),
        Expanded(child: Text('NexDesk', style: theme.textTheme.titleLarge)),
        OutlinedButton.icon(
          onPressed: _host?.loading == false ? _openSettings : null,
          icon: const Icon(Icons.settings_outlined, size: 18),
          label: const Text('设置'),
        ),
      ],
    );
  }

  ButtonStyle _modeStyle() {
    final color = Theme.of(context).colorScheme;
    return ButtonStyle(
      minimumSize: const WidgetStatePropertyAll(Size(90, 34)),
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

  Widget _remote() {
    final validator = widget.validator;
    final ready = validator != null && !_prompting;
    final text = Theme.of(context).textTheme;
    return HomePanel(
      key: const ValueKey('remote-device-panel'),
      title: '连接远程设备',
      subtitle: _isLan ? '通过同一局域网内的地址连接。' : '输入对方的设备 ID 来发起连接。',
      action: FilledButton.icon(
        onPressed: ready ? () => _connect() : null,
        icon: const Icon(Icons.arrow_forward_rounded, size: 17),
        label: const Text('连接'),
      ),
      children: [
        Text(_isLan ? '远程 IP / 端口' : '远程设备 ID', style: text.labelMedium),
        const SizedBox(height: 5),
        TextField(
          key: ValueKey(_isLan ? 'lan-target' : 'device-id'),
          controller: _isLan ? _lanTarget : _deviceId,
          keyboardType: _isLan ? TextInputType.text : TextInputType.number,
          autocorrect: false,
          enableSuggestions: false,
          onChanged: (_) {
            if (_targetIssue != null) setState(() => _targetIssue = null);
          },
          onSubmitted: ready ? (_) => _connect() : null,
          decoration: InputDecoration(
            hintText: _isLan ? '192.168.1.20:$_defaultPort' : '输入九位设备 ID',
            errorText: _targetIssue,
          ),
        ),
        if (_isLan) ...[
          const SizedBox(height: 6),
          Text('省略端口时使用 $_defaultPort', style: text.bodySmall),
        ],
        if (validator == null) ...[
          const SizedBox(height: 6),
          Text('未加载 Rust 核心，当前无法建立连接。', style: text.bodySmall),
        ],
      ],
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
    final theme = Theme.of(context);
    return HomePanel(
      title: '近期连接',
      children: [
        if (current.isEmpty)
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
          )
        else
          _recentTiles(current, otherServer: false),
        if (other.isNotEmpty)
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton(
              onPressed: () =>
                  setState(() => _otherDirectories = !_otherDirectories),
              child: Text(_otherDirectories ? '收起其他服务记录' : '查看其他服务记录'),
            ),
          ),
        if (_otherDirectories) _recentTiles(other, otherServer: true),
      ],
    );
  }

  Widget _recentTiles(
    List<RecentDevice> devices, {
    required bool otherServer,
  }) => LayoutBuilder(
    builder: (context, bounds) {
      final width = bounds.maxWidth >= 540
          ? (bounds.maxWidth - 10) / 2
          : bounds.maxWidth;
      return Wrap(
        spacing: 10,
        runSpacing: 8,
        children: [
          for (final item in devices)
            SizedBox(
              width: width,
              child: _recentTile(item, otherServer: otherServer),
            ),
        ],
      );
    },
  );

  Widget _recentTile(RecentDevice item, {required bool otherServer}) =>
      ListTile(
        dense: true,
        contentPadding: const EdgeInsets.only(left: 10, right: 3),
        tileColor: Theme.of(context).colorScheme.surfaceContainerLow,
        shape: RoundedRectangleBorder(
          side: BorderSide(color: Theme.of(context).colorScheme.outlineVariant),
          borderRadius: BorderRadius.circular(7),
        ),
        leading: const Icon(Icons.desktop_windows_outlined),
        title: Text(item.label, maxLines: 1, overflow: TextOverflow.ellipsis),
        subtitle: Text(
          otherServer || item.origin == null ? item.endpoint : item.address,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
        ),
        onTap: _prompting ? null : () => _connect(item),
        trailing: PopupMenuButton<String>(
          tooltip: '管理近期设备',
          onSelected: (value) => _editRecent(item, value),
          itemBuilder: (_) => [
            const PopupMenuItem(value: 'rename', child: Text('重命名')),
            if (item.remembered)
              const PopupMenuItem(value: 'forget', child: Text('清除保存的密码')),
            const PopupMenuItem(value: 'remove', child: Text('移除')),
          ],
        ),
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
    final colors = Theme.of(context).colorScheme;
    return Material(
      color: colors.surface,
      child: Container(
        decoration: BoxDecoration(
          border: Border(top: BorderSide(color: colors.outlineVariant)),
        ),
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
        child: Semantics(
          liveRegion: true,
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
                child: Text(text, style: Theme.of(context).textTheme.bodySmall),
              ),
              if (host?.hasDeferredSettings == true)
                const Flexible(
                  child: Text('会话结束后应用设置', textAlign: TextAlign.end),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
