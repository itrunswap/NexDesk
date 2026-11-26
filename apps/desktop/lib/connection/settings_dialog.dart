import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../lan/host_controller.dart';
import '../preview/native_preview.dart';
import 'desktop_maintenance.dart';
import 'desktop_settings.dart';
import 'directory_origin.dart';
import 'permission_profile_picker.dart';
import 'session_permissions.dart';

class SettingsDialog extends StatefulWidget {
  const SettingsDialog({super.key, required this.host, this.directory});

  final HostController host;
  final String? directory;

  @override
  State<SettingsDialog> createState() => _SettingsDialogState();
}

class _SettingsDialogState extends State<SettingsDialog> {
  late final DesktopSettings _initial = widget.host.settingsDraft;
  late final _historyOrigin = normalizeDirectoryOrigin(widget.directory ?? '');
  late final _defaultOrigin = normalizeDirectoryOrigin(
    widget.host.defaultDirectory,
  );
  late final _origin = Uri.tryParse(
    _historyOrigin != null && _historyOrigin != _defaultOrigin
        ? _historyOrigin
        : _initial.directory,
  );
  late final _server = TextEditingController(text: _editHost(_origin));
  late final _httpsPort = TextEditingController(
    text: _origin?.hasPort == true ? _origin!.port.toString() : '',
  );
  late final _lanPort = TextEditingController(text: _initial.port.toString());
  late final _shared = TextEditingController(text: _initial.shareRoot);
  late final _downloads = TextEditingController(text: _initial.downloadRoot);
  late bool _selfHosted = _historyOrigin == null
      ? _initial.useSelfHosted
      : _historyOrigin != _defaultOrigin;
  late bool _lan = _initial.lanAllowed, _shell = _initial.allowShell;
  late ApprovalPolicy _approvalPolicy = _initial.approvalPolicy;
  late bool _deviceAuthorization = _initial.permissions.deviceAuthorization;
  late SessionPermissions _outgoing = _initial.permissions.outgoing;
  late SessionPermissions _incoming = _initial.permissions.incoming;
  late String _address = _initial.address, _source = _initial.source;
  late String _quality = _initial.quality;
  late int _fps = _initial.fps;
  String? _error, _testStatus;
  bool _saving = false, _testing = false, _refreshing = false;

  static String _editHost(Uri? uri) {
    if (uri == null || !uri.hasAuthority) return '';
    if (uri.scheme == 'http') return uri.origin;
    return uri.host.contains(':') ? '[${uri.host}]' : uri.host;
  }

  String? get _editedOrigin =>
      directoryOriginFromFields(_server.text, _httpsPort.text);

  String? get _testOrigin => _selfHosted
      ? _editedOrigin
      : normalizeDirectoryOrigin(widget.host.defaultDirectory);

  Future<void> _test() async {
    final origin = _testOrigin;
    if (origin == null) {
      setState(
        () => _testStatus = _selfHosted ? '请填写有效的服务器地址和端口。' : '此版本尚未配置默认服务。',
      );
      return;
    }
    setState(() {
      _testing = true;
      _testStatus = null;
    });
    final result = await widget.host.checkDirectory(origin);
    if (!mounted) return;
    setState(() {
      _testing = false;
      _testStatus = origin == _testOrigin
          ? result ?? 'NexDesk 目录可达，协议版本兼容。'
          : null;
    });
  }

  Future<void> _save() async {
    if (_saving) return;
    final port = int.tryParse(_lanPort.text.trim());
    final origin = _editedOrigin;
    if (port == null || port < 1 || port > 65535) {
      setState(() => _error = '本机监听端口须为 1–65535。');
      return;
    }
    if (_selfHosted && origin == null) {
      setState(() => _error = '请填写有效的自托管地址和 HTTPS 端口。');
      return;
    }
    final value = DesktopSettings(
      useSelfHosted: _selfHosted,
      directory: origin ?? _initial.directory,
      lanAllowed: _lan,
      address: _address,
      port: port,
      source: _source,
      shareRoot: _shared.text.trim(),
      allowShell: _shell,
      approvalPolicy: _approvalPolicy,
      downloadRoot: _downloads.text.trim(),
      quality: _quality,
      fps: _fps,
      permissions: DesktopPermissions(
        outgoing: _outgoing,
        incoming: _incoming,
        deviceAuthorization: _deviceAuthorization,
      ),
    );
    var afterSession = false;
    if (widget.host.affectsCurrentSession(value)) {
      final choice = await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: const Text('何时应用接收设置？'),
          content: const Text('立即应用会结束受影响的接收会话。'),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: const Text('取消'),
            ),
            TextButton(
              onPressed: () => Navigator.pop(context, false),
              child: const Text('立即应用'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: const Text('会话结束后应用'),
            ),
          ],
        ),
      );
      if (!mounted || choice == null) return;
      afterSession = choice;
    }
    setState(() {
      _saving = true;
      _error = null;
    });
    final error = await widget.host.saveSettings(
      value,
      afterSession: afterSession,
    );
    if (!mounted) return;
    setState(() {
      _saving = false;
      _error = error;
    });
    if (error == null) Navigator.pop(context, true);
  }

  Future<void> _refreshDevices() async {
    setState(() => _refreshing = true);
    await widget.host.refreshDevices();
    if (mounted) setState(() => _refreshing = false);
  }

  Future<void> _pick(TextEditingController field) async {
    try {
      final path = await NativePreview.selectDirectory();
      if (mounted && path != null) field.text = path;
    } on PlatformException {
      if (mounted) setState(() => _error = '目录选取失败，可以手动输入完整路径。');
    } on MissingPluginException {
      if (mounted) setState(() => _error = '目录选取不可用，可以手动输入完整路径。');
    }
  }

  @override
  void dispose() {
    _server.dispose();
    _httpsPort.dispose();
    _lanPort.dispose();
    _shared.dispose();
    _downloads.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => PopScope(
    canPop: !_saving,
    child: Dialog(
      insetPadding: const EdgeInsets.all(16),
      child: SizedBox(
        width: 590,
        height: math.min(625, MediaQuery.sizeOf(context).height - 36),
        child: DefaultTabController(
          length: 6,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(22, 18, 22, 2),
                child: Text(
                  '设置',
                  style: Theme.of(context).textTheme.headlineSmall,
                ),
              ),
              const TabBar(
                labelPadding: EdgeInsets.symmetric(horizontal: 8),
                tabs: [
                  Tab(text: '连接'),
                  Tab(text: '权限'),
                  Tab(text: '共享'),
                  Tab(text: '画面'),
                  Tab(text: '存储'),
                  Tab(text: '维护'),
                ],
              ),
              Expanded(
                child: IgnorePointer(
                  ignoring: _saving,
                  child: TabBarView(
                    children: [
                      _page(_network()),
                      _page(_permissionSettings()),
                      _page(_sharing()),
                      _page(_video()),
                      _page(_storage()),
                      DesktopMaintenance(host: widget.host),
                    ],
                  ),
                ),
              ),
              if (_error case final error?)
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 20),
                  child: Semantics(
                    liveRegion: true,
                    child: Text(
                      error,
                      style: TextStyle(
                        color: Theme.of(context).colorScheme.error,
                      ),
                    ),
                  ),
                ),
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 12, 16, 16),
                child: OverflowBar(
                  alignment: MainAxisAlignment.end,
                  spacing: 8,
                  overflowSpacing: 8,
                  children: [
                    TextButton(
                      onPressed: _saving ? null : () => Navigator.pop(context),
                      child: const Text('取消'),
                    ),
                    FilledButton(
                      onPressed: _saving || _testing || _refreshing
                          ? null
                          : _save,
                      child: Text(_saving ? '正在保存…' : '保存'),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    ),
  );

  Widget _page(List<Widget> children) =>
      ListView(padding: const EdgeInsets.all(20), children: children);

  List<Widget> _network() {
    final host = widget.host;
    final defaultOrigin = normalizeDirectoryOrigin(host.defaultDirectory);
    return [
      _heading('ID 服务'),
      SwitchListTile(
        contentPadding: EdgeInsets.zero,
        title: const Text('使用自托管服务'),
        value: _selfHosted,
        onChanged: (value) => setState(() {
          _selfHosted = value;
          _testStatus = null;
        }),
      ),
      if (_selfHosted) ...[
        TextField(
          key: const ValueKey('directory-server'),
          controller: _server,
          decoration: const InputDecoration(
            labelText: '服务器地址',
            hintText: 'desk.example.test',
          ),
          onChanged: (_) => setState(() => _testStatus = null),
        ),
        const SizedBox(height: 12),
        _numberField(
          _httpsPort,
          '信令 HTTPS 端口',
          hint: '443',
          onChanged: (_) => setState(() => _testStatus = null),
        ),
      ] else
        Text(defaultOrigin ?? '此版本尚未配置默认服务。'),
      const SizedBox(height: 8),
      Align(
        alignment: Alignment.centerLeft,
        child: OutlinedButton(
          onPressed: _testing ? null : _test,
          child: Text(_testing ? '正在测试…' : '测试连接'),
        ),
      ),
      if (_testStatus case final status?)
        Semantics(liveRegion: true, child: Text(status)),
      const SizedBox(height: 22),
      _heading('IP 直连'),
      SwitchListTile(
        contentPadding: EdgeInsets.zero,
        title: const Text('允许此电脑通过局域网被连接'),
        value: _lan,
        onChanged: (value) => setState(() => _lan = value),
      ),
      _choice(
        '监听网卡',
        _address,
        {
          '': '自动选择私网地址',
          for (final address in host.interfaces) address: address,
          if (_address.isNotEmpty && !host.interfaces.contains(_address))
            _address: '$_address（当前不可用）',
        },
        (value) => setState(() => _address = value),
        enabled: _lan,
      ),
      const SizedBox(height: 12),
      _numberField(_lanPort, '本机监听端口', enabled: _lan),
      Align(
        alignment: Alignment.centerLeft,
        child: TextButton(
          onPressed: _refreshing ? null : _refreshDevices,
          child: const Text('刷新网卡和显示器'),
        ),
      ),
    ];
  }

  List<Widget> _permissionSettings() => [
    _heading('作为主控端'),
    const SizedBox(height: 12),
    PermissionProfilePicker(
      label: '默认请求权限',
      value: _outgoing,
      onChanged: (value) => setState(() => _outgoing = value),
    ),
    SwitchListTile(
      contentPadding: EdgeInsets.zero,
      title: const Text('使用本机设备授权'),
      subtitle: const Text('被控电脑信任本机后，可按已批准的权限免确认连接。'),
      value: _deviceAuthorization,
      onChanged: (value) => setState(() => _deviceAuthorization = value),
    ),
    const Divider(height: 24),
    _heading('作为被控端'),
    const SizedBox(height: 12),
    PermissionProfilePicker(
      label: '允许的权限上限',
      value: _incoming,
      onChanged: (value) => setState(() => _incoming = value),
    ),
    const SizedBox(height: 8),
    const Text('来访只会获得双方都允许且本机支持的功能。文件访问限于设置的共享目录。'),
  ];

  List<Widget> _sharing() => [
    _heading('接入确认'),
    _choice(
      '接入方式',
      _approvalPolicy.value,
      {for (final policy in ApprovalPolicy.values) policy.value: policy.label},
      (value) =>
          setState(() => _approvalPolicy = ApprovalPolicy.fromValue(value)),
    ),
    const SizedBox(height: 8),
    Text(_approvalPolicy.description),
    ListTile(
      contentPadding: EdgeInsets.zero,
      title: const Text('管理可信设备'),
      trailing: const Icon(Icons.chevron_right),
      onTap: () => showDialog<void>(
        context: context,
        builder: (_) => TrustedDevicesDialog(host: widget.host),
      ),
    ),
    const Divider(height: 24),
    _heading('被控电脑'),
    const SizedBox(height: 12),
    _choice('初始共享来源', _source, {
      '': '自动选择显示器',
      for (final source in widget.host.sources) source.id: source.label,
      if (_source.isNotEmpty &&
          !widget.host.sources.any((source) => source.id == _source))
        _source: '之前的显示器暂不可用',
    }, (value) => setState(() => _source = value)),
    const SizedBox(height: 12),
    _folder(_shared, '被控端文件共享目录'),
    const SizedBox(height: 8),
    const Text('远程访问此目录需要被控端批准文件权限。留空时不提供文件共享。'),
    if (Platform.isMacOS)
      Align(
        alignment: Alignment.centerLeft,
        child: TextButton(
          onPressed: widget.host.requestSystemPermissions,
          child: const Text('申请系统录屏与辅助功能权限'),
        ),
      ),
    ExpansionTile(
      tilePadding: EdgeInsets.zero,
      title: const Text('高级权限'),
      children: [
        SwitchListTile(
          contentPadding: EdgeInsets.zero,
          title: const Text('允许在 ID 连接中请求远程 Shell'),
          subtitle: const Text('每次都在这台电脑单独批准。'),
          value: _shell,
          onChanged: (value) => setState(() => _shell = value),
        ),
      ],
    ),
  ];

  List<Widget> _video() => [
    _heading('新建远程会话'),
    const SizedBox(height: 12),
    _choice('默认画质', _quality, const {
      'auto': '自动',
      'clear': '清晰',
      'smooth': '流畅',
    }, (value) => setState(() => _quality = value)),
    const SizedBox(height: 12),
    _choice('默认帧率上限', _fps.toString(), {
      for (final fps in {10, 15, 24, 30, 45, 60, _fps}) '$fps': '$fps FPS',
    }, (value) => setState(() => _fps = int.parse(value))),
  ];

  List<Widget> _storage() => [
    _heading('文件接收'),
    const SizedBox(height: 12),
    _folder(_downloads, '本机接收文件的默认目录'),
    const SizedBox(height: 8),
    const Text('在文件管理中使用。留空时可在传输前选择目录。'),
    if (widget.host.hasDeferredSettings)
      TextButton(
        onPressed: () {
          widget.host.cancelDeferredSettings();
          Navigator.pop(context);
        },
        child: const Text('取消待生效的设置'),
      ),
  ];

  Widget _heading(String text) =>
      Text(text, style: Theme.of(context).textTheme.titleSmall);

  Widget _choice(
    String label,
    String value,
    Map<String, String> options,
    ValueChanged<String> onChanged, {
    bool enabled = true,
  }) => InputDecorator(
    decoration: InputDecoration(labelText: label, enabled: enabled),
    child: DropdownButtonHideUnderline(
      child: DropdownButton<String>(
        isExpanded: true,
        isDense: true,
        value: value,
        items: [
          for (final option in options.entries)
            DropdownMenuItem(
              value: option.key,
              child: Text(option.value, overflow: TextOverflow.ellipsis),
            ),
        ],
        onChanged: enabled
            ? (value) {
                if (value != null) onChanged(value);
              }
            : null,
      ),
    ),
  );

  Widget _folder(TextEditingController field, String label) => TextField(
    controller: field,
    decoration: InputDecoration(
      labelText: label,
      suffixIcon: IconButton(
        tooltip: '选择目录',
        icon: const Icon(Icons.folder_open_outlined),
        onPressed: () => _pick(field),
      ),
    ),
  );

  Widget _numberField(
    TextEditingController field,
    String label, {
    String? hint,
    bool enabled = true,
    ValueChanged<String>? onChanged,
  }) => TextField(
    controller: field,
    enabled: enabled,
    keyboardType: TextInputType.number,
    inputFormatters: [
      FilteringTextInputFormatter.digitsOnly,
      LengthLimitingTextInputFormatter(5),
    ],
    onChanged: onChanged,
    decoration: InputDecoration(labelText: label, hintText: hint),
  );
}
